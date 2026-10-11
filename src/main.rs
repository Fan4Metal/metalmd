//! MetalMD: a fast native Markdown viewer.
//!
//! Startup is ordered for the time-to-document: the files named on the command
//! line are read and parsed on background threads while the window is being
//! created, and a second launch hands its files to the running window instead
//! of starting another one.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

// First, so that `tr!` is in scope in the modules below.
#[macro_use]
mod i18n;

mod app;
mod bench;
mod caption;
mod fs_text;
mod icon;
mod markdown;
mod placement;
mod settings;
mod single_instance;
mod style;
mod ui;
mod watcher;

use app::{App, AppMsg, Startup};
use bench::Bench;
use settings::Settings;
use std::path::PathBuf;
use std::sync::{Arc, OnceLock};

fn main() -> eframe::Result {
    install_panic_hook();
    let mut bench_output = None;
    let mut new_instance = false;
    let mut raw_paths = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--bench") => {
                // Only a .json file: `--bench notes.md`, the timings file
                // forgotten, would write the timings over the document.
                let output = args.next().map(PathBuf::from).filter(|p| {
                    p.extension()
                        .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                });
                let Some(output) = output else {
                    eprintln!("usage: metalmd --bench <timings.json> [FILE...]");
                    std::process::exit(2);
                };
                bench_output = Some(output);
            }
            Some("--new-instance") => new_instance = true,
            Some("--export-icon") => {
                // For tools/make_release.py: the installer's icon, then exit.
                let out = args.next().unwrap_or_else(|| "app.ico".into());
                if std::fs::write(&out, icon::ico(&icon::ICO_SIZES)).is_err() {
                    std::process::exit(1);
                }
                return Ok(());
            }
            Some(flag) if flag.starts_with("--") => {}
            _ => raw_paths.push(PathBuf::from(arg)),
        }
    }
    let mut bench = bench_output.map(Bench::new);
    // Each file once, as opening it again from the window would.
    let mut paths: Vec<PathBuf> = Vec::new();
    for path in raw_paths.iter().map(|p| app::normalize_path(p)) {
        if !paths.iter().any(|p| settings::same_path(p, &path)) {
            paths.push(path);
        }
    }

    // A running instance takes the files; this process is done.
    let single = !new_instance && bench.is_none();
    if single && single_instance::hand_off(&paths) {
        return Ok(());
    }
    let server = if single {
        single_instance::Server::bind()
    } else {
        None
    };

    // Start reading and parsing now, in parallel with window creation.
    let (sender, receiver) = std::sync::mpsc::channel::<AppMsg>();
    let ctx_cell: app::CtxCell = Arc::new(OnceLock::new());
    let mut tabs = Vec::new();
    for (i, path) in paths.into_iter().enumerate() {
        let id = i as u64 + 1;
        app::spawn_load(sender.clone(), ctx_cell.clone(), id, 0, path.clone());
        tabs.push((id, path));
    }

    let settings = Settings::load();
    i18n::set_lang(settings.language.resolve());
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("MetalMD")
        .with_app_id("metalmd")
        .with_min_inner_size([420.0, 300.0])
        .with_drag_and_drop(true)
        .with_icon(Arc::new(egui::IconData {
            rgba: embedded_icon(64).to_vec(),
            width: 64,
            height: 64,
        }));
    let centered = match settings.window {
        Some(window) => {
            viewport = viewport
                .with_inner_size([window.width, window.height])
                // `App::new` moves it back if it is off every screen.
                .with_position([window.x, window.y])
                .with_maximized(window.maximized);
            false
        }
        None => {
            viewport = viewport.with_inner_size([980.0, 780.0]);
            true
        }
    };
    let benchmarking = bench.is_some();
    let options = eframe::NativeOptions {
        viewport,
        // OpenGL starts fastest: measured on Windows (RTX 4070 Ti), creating
        // the context takes ~130 ms with glow vs ~240 ms with wgpu on DX12.
        renderer: eframe::Renderer::Glow,
        centered,
        event_loop_builder: benchmarking.then(|| {
            Box::new(|_: &mut _| bench::mark_external("event_loop")) as eframe::EventLoopBuilderHook
        }),
        window_builder: benchmarking.then(|| {
            Box::new(|builder| {
                bench::mark_external("window_builder");
                builder
            }) as eframe::WindowBuilderHook
        }),
        ..Default::default()
    };
    if let Some(b) = bench.as_mut() {
        b.mark("run_native");
    }
    let startup = Startup {
        tabs,
        sender,
        receiver,
        ctx_cell,
        settings,
        server,
        bench,
    };
    eframe::run_native(
        "MetalMD",
        options,
        Box::new(move |cc| Ok(Box::new(App::new(cc, startup)))),
    )
}

/// The version shown in the UI.
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

/// The app icon rasterised at build time (`build.rs`) as straight RGBA,
/// `size` x `size` pixels for 64, 128 and 256.
pub fn embedded_icon(size: u32) -> &'static [u8] {
    match size {
        64 => include_bytes!(concat!(env!("OUT_DIR"), "/app_icon_64.rgba")),
        128 => include_bytes!(concat!(env!("OUT_DIR"), "/app_icon_128.rgba")),
        _ => include_bytes!(concat!(env!("OUT_DIR"), "/app_icon_256.rgba")),
    }
}

/// The release build aborts on a panic and has no console, so the window
/// would vanish without a word: say what happened in a message box. The
/// default hook still prints it (seen in a debug build). A stack overflow is
/// not a panic and still ends the process silently.
fn install_panic_hook() {
    let default = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default(info);
        let text = tr!(
            format!(
                "MetalMD stopped because of an internal error.

{info}"
            ),
            format!(
                "MetalMD остановлен из-за внутренней ошибки.

{info}"
            )
        );
        error_box("MetalMD", &text);
    }));
}

/// Shows `text` in a system message box with an error icon and waits until
/// it is closed. It needs no window of the app, so it works from any thread
/// and while the app is failing.
#[cfg(windows)]
fn error_box(title: &str, text: &str) {
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        MB_ICONERROR, MB_SETFOREGROUND, MessageBoxW,
    };
    let wide = |s: &str| s.encode_utf16().chain(Some(0)).collect::<Vec<u16>>();
    let (text, title) = (wide(text), wide(title));
    unsafe {
        MessageBoxW(
            std::ptr::null_mut(),
            text.as_ptr(),
            title.as_ptr(),
            MB_ICONERROR | MB_SETFOREGROUND,
        );
    }
}

#[cfg(not(windows))]
fn error_box(_title: &str, _text: &str) {}
