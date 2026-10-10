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
        .with_icon(Arc::new(icon()));
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

/// A small generated window icon: a page with a down arrow, "Markdown"'s mark.
fn icon() -> egui::IconData {
    const SIZE: u32 = 64;
    let mut rgba = vec![0u8; (SIZE * SIZE * 4) as usize];
    let blue = [0x09, 0x69, 0xda, 0xff];
    let white = [0xff, 0xff, 0xff, 0xff];
    let inside_rounded = |x: f32, y: f32, min: f32, max: f32, r: f32| {
        let cx = x.clamp(min + r, max - r);
        let cy = y.clamp(min + r, max - r);
        (x - cx).powi(2) + (y - cy).powi(2) <= r * r
    };
    for y in 0..SIZE {
        for x in 0..SIZE {
            let (fx, fy) = (x as f32 + 0.5, y as f32 + 0.5);
            let mut color = [0, 0, 0, 0];
            if inside_rounded(fx, fy, 2.0, 62.0, 12.0) {
                color = blue;
                // "M": two posts and a V between them.
                let in_post = (14.0..20.0).contains(&fx) || (36.0..42.0).contains(&fx);
                let v = (18.0..=34.0).contains(&fy)
                    && ((fx - 17.0) - (fy - 18.0) * 0.69).abs() < 3.2
                    || (18.0..=34.0).contains(&fy)
                        && ((39.0 - fx) - (fy - 18.0) * 0.69).abs() < 3.2;
                if (18.0..46.0).contains(&fy) && (in_post || v) {
                    color = white;
                }
                // Down arrow on the right.
                let shaft = (48.0..52.0).contains(&fx) && (18.0..38.0).contains(&fy);
                let head = (34.0..=46.0).contains(&fy) && (fx - 50.0).abs() <= (46.0 - fy) * 0.8;
                if shaft || head {
                    color = white;
                }
            }
            let i = ((y * SIZE + x) * 4) as usize;
            rgba[i..i + 4].copy_from_slice(&color);
        }
    }
    egui::IconData {
        rgba,
        width: SIZE,
        height: SIZE,
    }
}
