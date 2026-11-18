#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod agent;
mod ai;
mod app;
mod export;
mod import;
mod launcher;
mod paint;
mod psd;
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
        if a == "--install" {
            // Only add Inkwell to the desktop's app menu, then exit.
            match launcher::install(true) {
                Ok(p) => {
                    println!("Added to the app menu: {}", p.map(|p| p.display().to_string()).unwrap_or_default());
                    std::process::exit(0);
                }
                Err(e) => {
                    eprintln!("Couldn't add Inkwell to the app menu: {e}");
                    std::process::exit(1);
                }
            }
        } else if a == "--screenshot" {
            screenshot = args.next().map(PathBuf::from);
        } else if a == "--process" {
            // Run the Run steps on one page without the window, printing every message
            // (for checks): --process <comic folder> <page number> <find,read,clean,translate>
            let dir = PathBuf::from(args.next().unwrap_or_default());
            let page: usize = args.next().and_then(|p| p.parse().ok()).unwrap_or(1);
            let wanted = args.next().unwrap_or_else(|| "find,read,clean,translate".into());
            let steps = process::Steps {
                find: wanted.contains("find"),
                read: wanted.contains("read"),
                translate: wanted.contains("translate"),
                clean: wanted.contains("clean"),
            };
            let (p, _) = project::load(&dir).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(1)
            });
            let i = page.saturating_sub(1).min(p.pages.len().saturating_sub(1));
            let job = process::PageJob { index: i, path: dir.join(&p.pages[i].file), blocks: p.pages[i].blocks.clone() };
            let langs = ai::tesseract_langs().unwrap_or_default();
            let run = process::start(egui::Context::default(), settings::Settings::load(), langs, steps, vec![job], p.next_id, false);
            let mut last = String::new();
            while let Ok(m) = run.rx.recv() {
                match m {
                    process::Msg::Progress { label, .. } => {
                        if label != last {
                            println!("progress: {label}");
                            last = label;
                        }
                    }
                    process::Msg::Found { blocks, .. } => println!("found {} boxes", blocks.len()),
                    process::Msg::Update { id, source, translation, .. } => {
                        println!("box {id}: {:?} -> {:?}", source, translation)
                    }
                    process::Msg::Error(e) => println!("problem: {e}"),
                    process::Msg::Finished { .. } => {
                        println!("finished");
                        break;
                    }
                }
            }
            std::process::exit(0);
        } else if a == "--export-cbz" || a == "--export-psd" {
            let format = if a == "--export-psd" { export::Format::Psd } else { export::Format::Cbz };
            // Export a comic folder without opening the window (for scripts and checks).
            let dir = PathBuf::from(args.next().unwrap_or_default());
            let code = match project::load(&dir) {
                Ok((p, _)) => {
                    let fonts = textfit::FontStore::new(&settings::Settings::load().default_font);
                    let stop = std::sync::atomic::AtomicBool::new(false);
                    match export::run(&dir, &p, &fonts, format, 4, &stop, |d, t| {
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
    session::install_signal_note();

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
