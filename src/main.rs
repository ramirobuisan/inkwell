#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod ai;
mod app;
mod export;
mod import;
mod process;
mod project;
mod session;
mod settings;
mod textfit;
mod work;

use std::path::PathBuf;

use eframe::egui;

fn main() -> eframe::Result<()> {
    let mut screenshot: Option<PathBuf> = None;
    let mut files: Vec<PathBuf> = Vec::new();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        if a == "--screenshot" {
            screenshot = args.next().map(PathBuf::from);
        } else if a == "--export-cbz" {
            // Export a comic folder without opening the window (for scripts and checks).
            let dir = PathBuf::from(args.next().unwrap_or_default());
            let code = match project::load(&dir) {
                Ok((p, _)) => {
                    let fonts = textfit::FontStore::new(&settings::Settings::load().default_font);
                    let stop = std::sync::atomic::AtomicBool::new(false);
                    match export::run(&dir, &p, &fonts, export::Format::Cbz, 4, &stop, |d, t| {
                        eprintln!("page {d}/{t}")
                    }) {
                        Ok(path) => {
                            println!("{}", path.display());
                            0
                        }
                        Err(e) => {
                            eprintln!("{e}");
                            1
                        }
                    }
                }
                Err(e) => {
                    eprintln!("{e}");
                    1
                }
            };
            std::process::exit(code);
        } else {
            files.push(PathBuf::from(a));
        }
    }
    session::install_panic_hook();

    let mut viewport = egui::ViewportBuilder::default()
        .with_title("Inkwell")
        .with_app_id("inkwell")
        .with_inner_size([1360.0, 860.0])
        .with_min_inner_size([900.0, 600.0])
        .with_drag_and_drop(true);
    if screenshot.is_some() {
        viewport = viewport.with_active(false);
        #[cfg(target_os = "windows")]
        {
            viewport = viewport.with_window_level(egui::WindowLevel::AlwaysOnBottom);
        }
    }
    #[allow(unused_mut)]
    let mut options = eframe::NativeOptions {
        viewport,
        vsync: true,
        ..Default::default()
    };
    #[cfg(target_os = "macos")]
    if screenshot.is_some() {
        options.event_loop_builder = Some(Box::new(|builder| {
            use winit::platform::macos::EventLoopBuilderExtMacOS;
            builder.with_activate_ignoring_other_apps(false);
        }));
    }
    eframe::run_native(
        "Inkwell",
        options,
        Box::new(move |cc| Ok(Box::new(app::App::new(cc, screenshot, files)))),
    )
}
