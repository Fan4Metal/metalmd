//! One window per user: a second launch hands its files to the first one.
//!
//! The first process listens on a local socket (a named pipe on Windows).
//! A later process connects, writes its paths one per line and exits, which
//! takes a few milliseconds instead of starting a second window.

use interprocess::local_socket::{
    GenericFilePath, GenericNamespaced, ListenerOptions, Name, Stream, prelude::*,
};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;

fn socket_name() -> std::io::Result<Name<'static>> {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_else(|_| "user".into());
    let user: String = user
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .collect();
    if GenericNamespaced::is_supported() {
        format!("qmarkdown-{user}.sock").to_ns_name::<GenericNamespaced>()
    } else {
        std::env::temp_dir()
            .join(format!("qmarkdown-{user}.sock"))
            .to_fs_name::<GenericFilePath>()
            .map(|n| n.into_owned())
    }
}

/// Sends `paths` to a running instance. `Ok(true)` when one took them.
pub fn hand_off(paths: &[PathBuf]) -> bool {
    let Ok(name) = socket_name() else {
        return false;
    };
    let Ok(mut stream) = Stream::connect(name) else {
        return false;
    };
    #[cfg(windows)]
    allow_foreground();
    let mut message = String::new();
    for path in paths {
        message.push_str(&path.to_string_lossy());
        message.push('\n');
    }
    // An empty message still means "bring the window to the front".
    message.push('\n');
    stream.write_all(message.as_bytes()).is_ok() && stream.flush().is_ok()
}

/// Lets the running instance take focus when it gets our files. Windows
/// otherwise only flashes its taskbar button.
#[cfg(windows)]
fn allow_foreground() {
    use windows_sys::Win32::UI::WindowsAndMessaging::{ASFW_ANY, AllowSetForegroundWindow};
    // SAFETY: plain Win32 call with a constant argument.
    unsafe {
        AllowSetForegroundWindow(ASFW_ANY);
    }
}

pub struct Server {
    listener: interprocess::local_socket::Listener,
}

impl Server {
    /// Claims the instance name. `None` when another process holds it.
    pub fn bind() -> Option<Server> {
        let name = socket_name().ok()?;
        let listener = ListenerOptions::new()
            .name(name)
            .try_overwrite(true)
            .create_sync()
            .ok()?;
        Some(Server { listener })
    }

    /// Serves hand-offs on a background thread.
    pub fn spawn(self, on_paths: impl Fn(Vec<PathBuf>) + Send + 'static) {
        std::thread::Builder::new()
            .name("single-instance".into())
            .spawn(move || {
                for connection in self.listener.incoming().flatten() {
                    let mut paths = Vec::new();
                    for line in BufReader::new(connection).lines() {
                        let Ok(line) = line else { break };
                        if line.is_empty() {
                            break;
                        }
                        paths.push(PathBuf::from(line));
                    }
                    on_paths(paths);
                }
            })
            .expect("spawn single-instance thread");
    }
}
