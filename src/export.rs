//! Export: every page with its text boxes pasted on, as a CBZ of PNGs (stored, since PNGs
//! are already compressed) or as numbered PNG files. Pages are drawn on several threads
//! at once and written in order.

use std::{
    fs,
    io::{BufWriter, Cursor, Write},
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver},
        Arc,
    },
    thread,
};

use eframe::egui;
use image::{codecs::png, ImageEncoder};

use crate::{
    project::{self, Project},
    textfit::{self, FontStore},
};

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Format {
    Cbz,
    Png,
}

pub enum Msg {
    Progress(usize, usize),
    Done(PathBuf),
    Failed(String),
}

pub struct Exporting {
    pub rx: Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
    pub done: usize,
    pub total: usize,
}

fn render_page(dir: &Path, page: &project::Page, fonts: &FontStore) -> Result<Vec<u8>, String> {
    let mut img = image::open(dir.join(&page.file))
        .map_err(|e| format!("{}: {e}", page.file))?
        .to_rgba8();
    for b in page.blocks.iter().filter(|b| b.visible) {
        let layer = textfit::render_block(b, fonts);
        textfit::paste(&mut img, &layer, b.x, b.y);
    }
    let mut out = Cursor::new(Vec::new());
    png::PngEncoder::new_with_quality(
        &mut out,
        png::CompressionType::Default,
        png::FilterType::Adaptive,
    )
    .write_image(
        img.as_raw(),
        img.width(),
        img.height(),
        image::ExtendedColorType::Rgba8,
    )
    .map_err(|e| e.to_string())?;
    Ok(out.into_inner())
}

/// Draws and writes every page; `progress(done, total)` after each one.
pub fn run(
    dir: &Path,
    project: &Project,
    fonts: &FontStore,
    format: Format,
    workers: usize,
    stop: &AtomicBool,
    progress: impl Fn(usize, usize),
) -> Result<PathBuf, String> {
    let total = project.pages.len();

    let out_dir = dir.join("export");
    fs::create_dir_all(&out_dir).map_err(|e| e.to_string())?;
    let digits = total.to_string().len().max(3);
    let (dest, mut zip) = match format {
        Format::Cbz => {
            let path = out_dir.join(format!("{} (translated).cbz", project.name));
            let part = project::with_suffix(&path, ".part");
            let f = fs::File::create(&part).map_err(|e| e.to_string())?;
            (path, Some((part, zip::ZipWriter::new(BufWriter::new(f)))))
        }
        Format::Png => {
            let d = out_dir.join(format!("{} (translated)", project.name));
            fs::create_dir_all(&d).map_err(|e| e.to_string())?;
            (d, None)
        }
    };
    let indices: Vec<usize> = (0..total).collect();
    for chunk in indices.chunks(workers.max(1)) {
        if stop.load(Ordering::Relaxed) {
            return Err("Export cancelled".into());
        }
        let encoded: Vec<Result<Vec<u8>, String>> = thread::scope(|s| {
            let handles: Vec<_> = chunk
                .iter()
                .map(|&i| {
                    let (dir, page, fonts) = (dir, &project.pages[i], fonts);
                    s.spawn(move || render_page(dir, page, fonts))
                })
                .collect();
            handles
                .into_iter()
                .map(|h| {
                    h.join()
                        .unwrap_or_else(|_| Err("a page failed to draw".into()))
                })
                .collect()
        });
        for (&i, bytes) in chunk.iter().zip(encoded) {
            let bytes = bytes?;
            let name = format!("{:0digits$}.png", i + 1);
            match &mut zip {
                Some((_, z)) => {
                    let opts = zip::write::SimpleFileOptions::default()
                        .compression_method(zip::CompressionMethod::Stored);
                    z.start_file(name, opts).map_err(|e| e.to_string())?;
                    z.write_all(&bytes).map_err(|e| e.to_string())?;
                }
                None => fs::write(dest.join(name), bytes).map_err(|e| e.to_string())?,
            }
            progress(i + 1, total);
        }
    }
    if let Some((part, z)) = zip {
        let w = z.finish().map_err(|e| e.to_string())?;
        let f = w.into_inner().map_err(|e| e.to_string())?;
        f.sync_all().map_err(|e| e.to_string())?;
        fs::rename(&part, &dest).map_err(|e| e.to_string())?;
    }
    Ok(dest)
}

pub fn start(
    ctx: egui::Context,
    dir: PathBuf,
    project: Project,
    fonts: FontStore,
    format: Format,
    workers: usize,
) -> Exporting {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    let total = project.pages.len();
    thread::Builder::new()
        .name("export".into())
        .spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let result = run(&dir, &project, &fonts, format, workers, &stop, |d, t| {
                send(Msg::Progress(d, t))
            });
            send(match result {
                Ok(p) => Msg::Done(p),
                Err(e) => Msg::Failed(e),
            });
        })
        .expect("start export");
    Exporting {
        rx,
        cancel,
        done: 0,
        total,
    }
}
