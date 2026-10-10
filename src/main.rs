//! qmarkdown: a fast native Markdown viewer.
//!
//! Startup is ordered for the time-to-document: the files named on the command
//! line are read and parsed on background threads while the window is being
//! created, and a second launch hands its files to the running window instead
//! of starting another one.

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod bench;
mod fs_text;
#[cfg(test)]
mod icon;
mod markdown;
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
    let mut bench_output = None;
    let mut new_instance = false;
    let mut raw_paths = Vec::new();
    let mut args = std::env::args_os().skip(1);
    while let Some(arg) = args.next() {
        match arg.to_str() {
            Some("--bench") => bench_output = args.next().map(PathBuf::from),
            Some("--new-instance") => new_instance = true,
            Some(flag) if flag.starts_with("--") => {}
            _ => raw_paths.push(PathBuf::from(arg)),
        }
    }
    let mut bench = bench_output.map(Bench::new);
    let paths: Vec<PathBuf> = raw_paths.iter().map(|p| app::normalize_path(p)).collect();

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
    let mut viewport = egui::ViewportBuilder::default()
        .with_title("qmarkdown")
        .with_app_id("qmarkdown")
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
                .with_position([window.x.max(-20.0), window.y.max(-20.0)])
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
        "qmarkdown",
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
