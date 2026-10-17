//! The page printed by WebView2 (Edge's engine, part of Windows 10 and 11),
//! on a thread of its own with a window of its own.
//!
//! For a PDF the window stays hidden and `PrintToPdf` writes the file. For
//! the printer the window is shown over the app's and the browser's print
//! dialog opens in it, with its preview; a script added to the page tells
//! when the dialog has closed (`afterprint`), and the window closes then.
//! WebView2 calls back through the thread's message queue, so every wait
//! pumps messages (`webview2_com::wait_with_pump`).

use std::cell::RefCell;
use std::sync::mpsc;
use webview2_com::Microsoft::Web::WebView2::Win32::*;
use webview2_com::{
    AcceleratorKeyPressedEventHandler, AddScriptToExecuteOnDocumentCreatedCompletedHandler,
    CoreWebView2EnvironmentOptions, CreateCoreWebView2ControllerCompletedHandler,
    CreateCoreWebView2EnvironmentCompletedHandler, NavigationCompletedEventHandler,
    NavigationStartingEventHandler, NewWindowRequestedEventHandler, PrintToPdfCompletedHandler,
    WebMessageReceivedEventHandler,
};
use windows::Win32::Foundation::{
    E_POINTER, ERROR_FILE_NOT_FOUND, HWND, LPARAM, LRESULT, RECT, WPARAM,
};
use windows::Win32::Graphics::Gdi::{
    GetMonitorInfoW, MONITOR_DEFAULTTONEAREST, MONITORINFO, MonitorFromWindow,
};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx, CoUninitialize};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, Interface, PCWSTR, w};

/// Where the page goes.
pub enum Target<'a> {
    /// The print dialog, in a window over `owner`, its caption dark or not.
    Printer {
        owner: Option<isize>,
        title: &'a str,
        dark: bool,
    },
    Pdf(&'a std::path::Path),
}

#[derive(Debug)]
pub enum Error {
    /// The WebView2 runtime is not installed.
    NoRuntime,
    /// The reader closed the window before it was done.
    Closed,
    Failed(String),
}

impl From<windows::core::Error> for Error {
    fn from(error: windows::core::Error) -> Error {
        if error.code() == ERROR_FILE_NOT_FOUND.to_hresult() {
            Error::NoRuntime
        } else {
            Error::Failed(error.message())
        }
    }
}

impl From<webview2_com::Error> for Error {
    fn from(error: webview2_com::Error) -> Error {
        match error {
            webview2_com::Error::WindowsError(error) => error.into(),
            webview2_com::Error::TaskCanceled => Error::Closed,
            other => Error::Failed(format!("{other:?}")),
        }
    }
}

thread_local! {
    /// The view in this thread's window, which follows the window's size.
    static CONTROLLER: RefCell<Option<ICoreWebView2Controller>> = const { RefCell::new(None) };
}

/// The message the page's script sends once the print dialog has closed.
const AFTER_PRINT: &str = "afterprint";

/// Prints the page at `url` (a `file:` URL) to `target`. Blocks until done:
/// the PDF written, or the print dialog closed. `Ok(false)` if the reader
/// closed the window before the dialog could open.
pub fn run(url: &str, target: Target<'_>) -> Result<bool, Error> {
    unsafe { CoInitializeEx(None, COINIT_APARTMENTTHREADED).ok()? };
    let result = print(url, target);
    unsafe { CoUninitialize() };
    result
}

fn print(url: &str, target: Target<'_>) -> Result<bool, Error> {
    let hwnd = create_window(&target)?;
    let result = print_in(hwnd, url, &target);
    if let Some(controller) = CONTROLLER.with(|c| c.borrow_mut().take()) {
        let _ = unsafe { controller.Close() };
    }
    unsafe {
        if IsWindow(Some(hwnd)).as_bool() {
            let _ = DestroyWindow(hwnd);
        }
    }
    match result {
        Err(Error::Closed) => Ok(false),
        other => other,
    }
}

fn print_in(hwnd: HWND, url: &str, target: &Target<'_>) -> Result<bool, Error> {
    let environment = environment()?;
    let controller = {
        let (tx, rx) = mpsc::channel();
        let environment = environment.clone();
        CreateCoreWebView2ControllerCompletedHandler::wait_for_async_operation(
            Box::new(move |handler| unsafe {
                environment
                    .CreateCoreWebView2Controller(hwnd, &handler)
                    .map_err(webview2_com::Error::WindowsError)
            }),
            Box::new(move |error, controller| {
                error?;
                let _ = tx.send(controller.ok_or_else(|| windows::core::Error::from(E_POINTER)));
                Ok(())
            }),
        )?;
        rx.recv().map_err(|e| Error::Failed(e.to_string()))??
    };
    CONTROLLER.with(|c| *c.borrow_mut() = Some(controller.clone()));
    unsafe {
        let mut rect = RECT::default();
        GetClientRect(hwnd, &mut rect)?;
        controller.SetBounds(rect)?;
    }
    let webview = unsafe { controller.CoreWebView2()? };
    unsafe {
        let settings = webview.Settings()?;
        settings.SetAreDevToolsEnabled(false)?;
        settings.SetAreDefaultContextMenusEnabled(false)?;
        settings.SetIsStatusBarEnabled(false)?;
        settings.SetIsZoomControlEnabled(false)?;
    }
    guard_navigation(&webview)?;
    if matches!(target, Target::Printer { .. }) {
        close_after_printing(hwnd, &webview, &controller)?;
    }

    let (tx, rx) = mpsc::channel();
    let mut token = 0;
    unsafe {
        webview.add_NavigationCompleted(
            &NavigationCompletedEventHandler::create(Box::new(move |_, args| {
                let mut ok = windows::core::BOOL::default();
                if let Some(args) = args {
                    args.IsSuccess(&mut ok)?;
                }
                let _ = tx.send(ok.as_bool());
                Ok(())
            })),
            &mut token,
        )?;
        webview.Navigate(&HSTRING::from(url))?;
    }
    if !webview2_com::wait_with_pump(rx)? {
        return Err(Error::Failed("the page could not be shown".to_owned()));
    }

    match target {
        Target::Pdf(path) => {
            pdf(&environment, &webview, path)?;
            Ok(true)
        }
        Target::Printer { .. } => {
            unsafe {
                let _ = ShowWindow(hwnd, SW_SHOW);
                let _ = SetForegroundWindow(hwnd);
                controller.SetIsVisible(true)?;
                controller.MoveFocus(COREWEBVIEW2_MOVE_FOCUS_REASON_PROGRAMMATIC)?;
                webview
                    .cast::<ICoreWebView2_16>()?
                    .ShowPrintUI(COREWEBVIEW2_PRINT_DIALOG_KIND_BROWSER)?;
            }
            // Until the window closes: after the dialog, on Esc, or by hand.
            let mut msg = MSG::default();
            while unsafe { GetMessageW(&mut msg, None, 0, 0) }.as_bool() {
                unsafe {
                    let _ = TranslateMessage(&msg);
                    DispatchMessageW(&msg);
                }
            }
            Ok(true)
        }
    }
}

/// The WebView2 environment, its data kept in the app's local folder.
fn environment() -> Result<ICoreWebView2Environment, Error> {
    let data = std::env::var_os("LOCALAPPDATA")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(std::env::temp_dir)
        .join("MetalMD")
        .join("WebView2");
    let options = CoreWebView2EnvironmentOptions::default();
    // The print dialog in the app's language.
    let language = match crate::i18n::lang() {
        crate::i18n::Lang::En => "en-US",
        crate::i18n::Lang::Ru => "ru",
    };
    unsafe { options.set_language(language.to_owned()) };
    let options: ICoreWebView2EnvironmentOptions = options.into();
    let (tx, rx) = mpsc::channel();
    CreateCoreWebView2EnvironmentCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            CreateCoreWebView2EnvironmentWithOptions(
                PCWSTR::null(),
                &HSTRING::from(data.as_os_str()),
                &options,
                &handler,
            )
            .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |error, environment| {
            error?;
            let _ = tx.send(environment.ok_or_else(|| windows::core::Error::from(E_POINTER)));
            Ok(())
        }),
    )?;
    Ok(rx.recv().map_err(|e| Error::Failed(e.to_string()))??)
}

/// The page stays the page: links open outside (web and mail addresses in
/// the browser, nothing else), and it opens no windows.
fn guard_navigation(webview: &ICoreWebView2) -> Result<(), Error> {
    let mut token = 0;
    let mut first = true;
    unsafe {
        webview.add_NavigationStarting(
            &NavigationStartingEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                if std::mem::take(&mut first) {
                    return Ok(());
                }
                args.SetCancel(true)?;
                let mut uri = windows::core::PWSTR::null();
                args.Uri(&mut uri)?;
                open_outside(webview2_com::take_pwstr(uri));
                Ok(())
            })),
            &mut token,
        )?;
        webview.add_NewWindowRequested(
            &NewWindowRequestedEventHandler::create(Box::new(|_, args| {
                let Some(args) = args else { return Ok(()) };
                args.SetHandled(true)?;
                let mut uri = windows::core::PWSTR::null();
                args.Uri(&mut uri)?;
                open_outside(webview2_com::take_pwstr(uri));
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

fn open_outside(uri: String) {
    let lower = uri.to_ascii_lowercase();
    if ["http://", "https://", "mailto:"]
        .iter()
        .any(|scheme| lower.starts_with(scheme))
    {
        std::thread::spawn(move || open::that_detached(uri));
    }
}

/// The window closes when the print dialog does, or on Esc.
fn close_after_printing(
    hwnd: HWND,
    webview: &ICoreWebView2,
    controller: &ICoreWebView2Controller,
) -> Result<(), Error> {
    let close = move || unsafe {
        let _ = PostMessageW(Some(hwnd), WM_CLOSE, WPARAM(0), LPARAM(0));
    };
    let script = format!(
        "addEventListener('afterprint', () => chrome.webview.postMessage('{AFTER_PRINT}'));"
    );
    let webview2 = webview.clone();
    AddScriptToExecuteOnDocumentCreatedCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            webview2
                .AddScriptToExecuteOnDocumentCreated(&HSTRING::from(script), &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(|error, _id| error),
    )?;
    let mut token = 0;
    unsafe {
        webview.add_WebMessageReceived(
            &WebMessageReceivedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let mut message = windows::core::PWSTR::null();
                if args.TryGetWebMessageAsString(&mut message).is_ok()
                    && webview2_com::take_pwstr(message) == AFTER_PRINT
                {
                    close();
                }
                Ok(())
            })),
            &mut token,
        )?;
        controller.add_AcceleratorKeyPressed(
            &AcceleratorKeyPressedEventHandler::create(Box::new(move |_, args| {
                let Some(args) = args else { return Ok(()) };
                let (mut kind, mut key) = (COREWEBVIEW2_KEY_EVENT_KIND::default(), 0);
                args.KeyEventKind(&mut kind)?;
                args.VirtualKey(&mut key)?;
                if kind == COREWEBVIEW2_KEY_EVENT_KIND_KEY_DOWN && key == 0x1b {
                    args.SetHandled(true)?;
                    close();
                }
                Ok(())
            })),
            &mut token,
        )?;
    }
    Ok(())
}

/// Writes the page to `path` as PDF: paper of the reader's measurement
/// system (A4, or Letter where inches are used), the margins of the page's
/// style sheet, no header or footer.
fn pdf(
    environment: &ICoreWebView2Environment,
    webview: &ICoreWebView2,
    path: &std::path::Path,
) -> Result<(), Error> {
    let settings = unsafe {
        let settings = environment
            .cast::<ICoreWebView2Environment6>()?
            .CreatePrintSettings()?;
        settings.SetOrientation(COREWEBVIEW2_PRINT_ORIENTATION_PORTRAIT)?;
        let (width, height) = if uses_letter() {
            (8.5, 11.0)
        } else {
            (210.0 / 25.4, 297.0 / 25.4)
        };
        settings.SetPageWidth(width)?;
        settings.SetPageHeight(height)?;
        settings.SetMarginTop(16.0 / 25.4)?;
        settings.SetMarginBottom(16.0 / 25.4)?;
        settings.SetMarginLeft(15.0 / 25.4)?;
        settings.SetMarginRight(15.0 / 25.4)?;
        settings.SetShouldPrintBackgrounds(true)?;
        settings.SetShouldPrintHeaderAndFooter(false)?;
        settings
    };
    let webview = webview.cast::<ICoreWebView2_7>()?;
    let path = HSTRING::from(path.as_os_str());
    let (tx, rx) = mpsc::channel();
    PrintToPdfCompletedHandler::wait_for_async_operation(
        Box::new(move |handler| unsafe {
            webview
                .PrintToPdf(&path, &settings, &handler)
                .map_err(webview2_com::Error::WindowsError)
        }),
        Box::new(move |error, ok| {
            let _ = tx.send(error.map(|()| ok));
            Ok(())
        }),
    )?;
    match rx.recv().map_err(|e| Error::Failed(e.to_string()))? {
        Ok(true) => Ok(()),
        Ok(false) => Err(Error::Failed("the PDF could not be written".to_owned())),
        Err(error) => Err(error.into()),
    }
}

/// The reader's region measures in inches (the US and a few others), so
/// their paper is Letter.
fn uses_letter() -> bool {
    use windows::Win32::Globalization::{GetLocaleInfoEx, LOCALE_IMEASURE};
    let mut buffer = [0u16; 4];
    let len = unsafe { GetLocaleInfoEx(PCWSTR::null(), LOCALE_IMEASURE, Some(&mut buffer)) };
    len > 0 && buffer[0] == u16::from(b'1')
}

/// The window the view lives in: hidden for a PDF, for the printer shown
/// later centred over `owner` and as large as the dialog wants.
fn create_window(target: &Target<'_>) -> Result<HWND, Error> {
    let class = w!("MetalMDPrint");
    unsafe {
        let instance = GetModuleHandleW(None)?;
        static REGISTER: std::sync::Once = std::sync::Once::new();
        REGISTER.call_once(|| {
            let window_class = WNDCLASSEXW {
                cbSize: size_of::<WNDCLASSEXW>() as u32,
                lpfnWndProc: Some(window_proc),
                hInstance: instance.into(),
                // The exe's icon (resource 1, from build.rs).
                hIcon: LoadIconW(Some(instance.into()), PCWSTR(1 as _)).unwrap_or_default(),
                hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
                lpszClassName: class,
                ..Default::default()
            };
            RegisterClassExW(&window_class);
        });
        let (owner, title, dark) = match *target {
            Target::Printer { owner, title, dark } => (owner, title, dark),
            Target::Pdf(_) => (None, "", false),
        };
        let owner = owner.map(|h| HWND(h as _));
        let (x, y, width, height) =
            owner.map_or((CW_USEDEFAULT, CW_USEDEFAULT, 1100, 860), placement);
        let caption = tr!(format!("Print — {title}"), format!("Печать — {title}"));
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            class,
            &HSTRING::from(caption),
            WS_OVERLAPPEDWINDOW,
            x,
            y,
            width,
            height,
            owner,
            None,
            Some(instance.into()),
            None,
        )?;
        if matches!(target, Target::Printer { .. }) {
            crate::caption::set_dark(hwnd.0 as isize, dark);
        }
        Ok(hwnd)
    }
}

/// Where the print window goes: centred over `owner`, as large as the print
/// dialog likes to be, within the work area of `owner`'s monitor.
fn placement(owner: HWND) -> (i32, i32, i32, i32) {
    unsafe {
        let scale = GetDpiForWindow(owner).max(96) as f32 / 96.0;
        let monitor = MonitorFromWindow(owner, MONITOR_DEFAULTTONEAREST);
        let mut info = MONITORINFO {
            cbSize: size_of::<MONITORINFO>() as u32,
            ..Default::default()
        };
        let mut owner_rect = RECT::default();
        if !GetMonitorInfoW(monitor, &mut info).as_bool()
            || GetWindowRect(owner, &mut owner_rect).is_err()
        {
            return (CW_USEDEFAULT, CW_USEDEFAULT, 1100, 860);
        }
        let work = info.rcWork;
        let (work_w, work_h) = (work.right - work.left, work.bottom - work.top);
        let width = ((1100.0 * scale) as i32).min(work_w * 92 / 100);
        let height = ((860.0 * scale) as i32).min(work_h * 92 / 100);
        let centre_x = (owner_rect.left + owner_rect.right) / 2;
        let centre_y = (owner_rect.top + owner_rect.bottom) / 2;
        let x = (centre_x - width / 2).clamp(work.left, (work.right - width).max(work.left));
        let y = (centre_y - height / 2).clamp(work.top, (work.bottom - height).max(work.top));
        (x, y, width, height)
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    match message {
        WM_SIZE => {
            CONTROLLER.with(|controller| {
                if let Some(controller) = controller.borrow().as_ref() {
                    let mut rect = RECT::default();
                    unsafe {
                        if GetClientRect(hwnd, &mut rect).is_ok() {
                            let _ = controller.SetBounds(rect);
                        }
                    }
                }
            });
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Writes a PDF of the sample through WebView2, which must be installed:
    /// `cargo test pdf_of_the_sample -- --ignored`.
    #[test]
    #[ignore]
    fn pdf_of_the_sample() {
        let sample =
            std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("samples/markdown-syntax.md");
        let doc =
            crate::markdown::document::Document::parse(&std::fs::read_to_string(&sample).unwrap());
        let options = super::super::html::PageOptions {
            title: "markdown-syntax.md".to_owned(),
            base_dir: sample.parent().map(Into::into),
            body_font: None,
            mono_font: None,
            text_px: 16.0,
            mark_bg: [0xff, 0xf1, 0x76],
            toggled_details: Default::default(),
            print_on_load: false,
        };
        let dir = std::env::temp_dir().join("metalmd-pdf-test");
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("sample.html");
        std::fs::write(&page, super::super::html::page(&doc, &options)).unwrap();
        let pdf = dir.join("sample.pdf");
        let _ = std::fs::remove_file(&pdf);
        let url = super::super::html::file_url(&page);
        assert!(matches!(run(&url, Target::Pdf(&pdf)), Ok(true)));
        let bytes = std::fs::read(&pdf).unwrap();
        assert!(bytes.starts_with(b"%PDF"), "{}", pdf.display());
        assert!(bytes.len() > 50_000, "{}", bytes.len());
    }
}
