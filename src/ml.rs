//! Koharu's own vision models, from its open-source crates in `vendor/` (MIT OR
//! Apache-2.0): Koharu Layout HighRes finds the lettering (with each glyph's pixels),
//! manga-ocr reads it, and LaMa repaints the artwork under it. They run on libtorch from
//! Koharu's store (`~/.cache/koharu/store`), which the original downloads on first use;
//! on this laptop it is already there from the installed Koharu.
//!
//! Models load on first use, once, and stay loaded, as the original's pipeline does.

use std::{
    path::PathBuf,
    sync::{Mutex, OnceLock},
};

use anyhow::{Context, Result};
use image::{DynamicImage, GrayImage, Luma, RgbImage, RgbaImage};
use koharu_ml::{
    koharu_layout_highres::{KoharuLayoutHighres, Layout, Options},
    lama::Lama,
    manga_ocr_ar::MangaOcrAr,
};
use kurbo::Shape;

use crate::project::Block;

fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("start the model runtime")
    })
}

#[derive(Default)]
struct Models {
    layout: Option<KoharuLayoutHighres>,
    ocr: Option<MangaOcrAr>,
    lama: Option<Lama>,
}

static MODELS: Mutex<Option<Models>> = Mutex::new(None);

fn store() -> PathBuf {
    dirs::cache_dir().unwrap_or_else(std::env::temp_dir).join("koharu").join("store")
}

/// Whether Koharu's models and libtorch are on this computer already.
pub fn available() -> bool {
    let s = store();
    s.join("torch").is_dir() && s.join("huggingface/models/mayocream--koharu-layout-highres").is_dir()
}

fn device() -> Result<&'static koharu_ml::Device> {
    runtime().block_on(koharu_runtime::device()).context("Koharu's model runtime couldn't start")
}

fn with_models<T>(f: impl FnOnce(&mut Models) -> Result<T>) -> Result<T> {
    let mut guard = MODELS.lock().map_err(|_| anyhow::anyhow!("the models are unavailable after an earlier failure"))?;
    let models = guard.get_or_insert_with(Models::default);
    f(models)
}

/// The page's lettering as text boxes in reading order, and a mask of every glyph's
/// pixels on the page. A text in a bubble gets its room in the bubble as its box.
pub fn find(page: &RgbaImage) -> Result<(Vec<Block>, GrayImage)> {
    let layout = detect(page)?;
    let mut blocks = Vec::new();
    for l in &layout.lettering {
        let [x0, y0, x1, y1] = l.bbox;
        let (mut bx0, mut by0, mut bx1, mut by1) = (x0, y0, x1, y1);
        if let Some(room) = &l.room {
            let r = room.bounding_box();
            bx0 = bx0.min(r.x0 as f32);
            by0 = by0.min(r.y0 as f32);
            bx1 = bx1.max(r.x1 as f32);
            by1 = by1.max(r.y1 as f32);
        }
        blocks.push(Block {
            x: bx0.max(0.0).round(),
            y: by0.max(0.0).round(),
            w: (bx1 - bx0).max(12.0).round(),
            h: (by1 - by0).max(12.0).round(),
            // The artwork is repainted under the text by Clean up, not covered.
            cover: false,
            ..Block::default()
        });
    }
    Ok((blocks, glyph_mask(&layout, page.width(), page.height())))
}

fn detect(page: &RgbaImage) -> Result<Layout> {
    with_models(|m| {
        if m.layout.is_none() {
            let d = device()?;
            m.layout = Some(runtime().block_on(KoharuLayoutHighres::load(d))?);
        }
        let model = m.layout.as_ref().expect("loaded");
        let image = DynamicImage::ImageRgba8(page.clone());
        let detected = model.inference(&image, &Options::default())?;
        Ok(detected.layout(&image.to_rgb8()))
    })
}

fn glyph_mask(layout: &Layout, w: u32, h: u32) -> GrayImage {
    let mut mask = GrayImage::new(w, h);
    for l in &layout.lettering {
        let m = &l.mask;
        for y in m.y..(m.y + m.height).min(h) {
            for x in m.x..(m.x + m.width).min(w) {
                if m.contains(x, y) {
                    mask.put_pixel(x, y, Luma([255]));
                }
            }
        }
    }
    mask
}

/// The text in each box, read by manga-ocr from the box's crop of the page.
pub fn read(page: &RgbaImage, boxes: &[Block]) -> Result<Vec<String>> {
    let crops: Vec<DynamicImage> = boxes
        .iter()
        .map(|b| {
            let x = (b.x.max(0.0) as u32).min(page.width().saturating_sub(1));
            let y = (b.y.max(0.0) as u32).min(page.height().saturating_sub(1));
            let w = (b.w as u32).clamp(1, page.width() - x);
            let h = (b.h as u32).clamp(1, page.height() - y);
            DynamicImage::ImageRgba8(image::imageops::crop_imm(page, x, y, w, h).to_image())
        })
        .collect();
    with_models(|m| {
        if m.ocr.is_none() {
            let d = device()?;
            m.ocr = Some(runtime().block_on(MangaOcrAr::load(d))?);
        }
        m.ocr.as_ref().expect("loaded").inference(&crops)
    })
}

/// The glyphs inside the boxes, grown by `grow` pixels so their anti-aliased edges go too.
pub fn mask_in_boxes(mask: &GrayImage, boxes: &[Block], grow: u32) -> GrayImage {
    let (w, h) = mask.dimensions();
    let mut out = GrayImage::new(w, h);
    let inside = |x: u32, y: u32| {
        boxes.iter().any(|b| x as f32 >= b.x && (x as f32) < b.x + b.w && y as f32 >= b.y && (y as f32) < b.y + b.h)
    };
    let g = grow as i64;
    for (x, y, p) in mask.enumerate_pixels() {
        if p.0[0] == 0 || !inside(x, y) {
            continue;
        }
        for dy in -g..=g {
            for dx in -g..=g {
                let (nx, ny) = (x as i64 + dx, y as i64 + dy);
                if nx >= 0 && ny >= 0 && (nx as u32) < w && (ny as u32) < h {
                    out.put_pixel(nx as u32, ny as u32, Luma([255]));
                }
            }
        }
    }
    out
}

/// The page repainted by LaMa where `mask` is white.
pub fn inpaint(page: &RgbaImage, mask: &GrayImage) -> Result<RgbImage> {
    with_models(|m| {
        if m.lama.is_none() {
            let d = device()?;
            m.lama = Some(runtime().block_on(Lama::load(d))?);
        }
        m.lama.as_ref().expect("loaded").inference(&DynamicImage::ImageRgba8(page.clone()), mask)
    })
}

/// LaMa's repaint as a cleanup layer: the repainted pixels under the mask, transparent
/// elsewhere.
pub fn as_layer(repainted: &RgbImage, mask: &GrayImage) -> RgbaImage {
    let (w, h) = mask.dimensions();
    let mut layer = RgbaImage::new(w, h);
    for (x, y, p) in mask.enumerate_pixels() {
        if p.0[0] > 0 {
            let c = repainted.get_pixel(x, y).0;
            layer.put_pixel(x, y, image::Rgba([c[0], c[1], c[2], 255]));
        }
    }
    layer
}
