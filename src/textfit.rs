//! Drawing a text box into pixels: the cover over the original lettering, then the
//! translation fitted to the box, with an optional outline. The canvas shows exactly
//! these pixels and export pastes the same ones, so what you see is what you export.

use std::{
    collections::HashMap,
    fs,
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use ab_glyph::{Font, FontVec, PxScale, ScaleFont};
use image::RgbaImage;

use crate::project::{Align, Block, CoverShape};

/// Line height as a multiple of the font height: koharu-renderer text/style.rs:73
/// (`line_height.unwrap_or(1.2)`).
pub const LINE_HEIGHT: f32 = 1.2;
/// A set size shrinks no smaller than this share of itself: text/fit.rs:66.
pub const SHRINK_FLOOR: f32 = 0.5;
/// Bisection steps when searching a size: the compiled `koharu_renderer::text::fit::halve`
/// (0x16fc5ec) runs a loop of 8 with midpoint `(lo + hi) * 0.5`.
pub const HALVINGS: usize = 8;
pub const MIN_PX: f32 = 6.0;
pub const MAX_PX: f32 = 160.0;

#[derive(Clone)]
pub struct FontStore {
    cache: Arc<Mutex<HashMap<String, Arc<FontVec>>>>,
    default: Arc<FontVec>,
    pub default_path: String,
    cjk: Option<Arc<FontVec>>,
    pub cjk_path: Option<PathBuf>,
    /// (display name, file path) of every font on the system.
    pub list: Arc<Vec<(String, String)>>,
}

fn scan_fonts() -> Vec<(String, String)> {
    let mut roots = vec![
        PathBuf::from("/usr/share/fonts"),
        PathBuf::from("/usr/local/share/fonts"),
    ];
    if let Some(h) = dirs::home_dir() {
        roots.push(h.join(".local/share/fonts"));
        roots.push(h.join(".fonts"));
    }
    let mut out = Vec::new();
    let mut stack = roots;
    while let Some(dir) = stack.pop() {
        let Ok(read) = fs::read_dir(&dir) else {
            continue;
        };
        for e in read.flatten() {
            let p = e.path();
            if p.is_dir() {
                stack.push(p);
            } else {
                let ext = crate::import::extension(&p);
                if ext == "ttf" || ext == "otf" || ext == "ttc" {
                    let name = p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    out.push((name, p.to_string_lossy().into_owned()));
                }
            }
        }
    }
    out.sort_by(|a, b| a.0.to_lowercase().cmp(&b.0.to_lowercase()));
    out.dedup_by(|a, b| a.0 == b.0);
    out
}

fn load_font(path: &str) -> Option<FontVec> {
    let bytes = fs::read(path).ok()?;
    FontVec::try_from_vec_and_index(bytes, 0).ok()
}

impl FontStore {
    pub fn new(preferred_default: &str) -> Self {
        let list = scan_fonts();
        let find = |stem: &str| list.iter().find(|(n, _)| n == stem).map(|(_, p)| p.clone());
        // A bold, plain sans reads best in speech bubbles.
        let mut default_path = String::new();
        let mut default = None;
        let mut candidates: Vec<String> = Vec::new();
        if !preferred_default.is_empty() {
            candidates.push(preferred_default.to_owned());
        }
        for stem in [
            "LiberationSans-Bold",
            "DejaVuSans-Bold",
            "NotoSans-Bold",
            "Arial Bold",
            "LiberationSans-Regular",
            "DejaVuSans",
        ] {
            if let Some(p) = find(stem) {
                candidates.push(p);
            }
        }
        for c in candidates {
            if let Some(f) = load_font(&c) {
                default = Some(f);
                default_path = c;
                break;
            }
        }
        let default = default.unwrap_or_else(|| {
            let defs = eframe::egui::FontDefinitions::default();
            let data = defs
                .font_data
                .get("Ubuntu-Light")
                .or_else(|| defs.font_data.values().next())
                .expect("egui ships fonts");
            FontVec::try_from_vec(data.font.to_vec()).expect("egui's font loads")
        });
        let cjk_path = [
            "NotoSansCJK-Bold",
            "NotoSansCJK-Regular",
            "NotoSansCJKjp-Regular",
            "NotoSansJP-Regular",
            "SourceHanSans-Regular",
        ]
        .iter()
        .find_map(|s| find(s))
        .map(PathBuf::from);
        let cjk = cjk_path
            .as_ref()
            .and_then(|p| load_font(&p.to_string_lossy()))
            .map(Arc::new);
        Self {
            cache: Arc::new(Mutex::new(HashMap::new())),
            default: Arc::new(default),
            default_path,
            cjk,
            cjk_path,
            list: Arc::new(list),
        }
    }

    pub fn get(&self, path: &str) -> Arc<FontVec> {
        if path.is_empty() || path == self.default_path {
            return self.default.clone();
        }
        let mut cache = self.cache.lock().expect("font cache");
        if let Some(f) = cache.get(path) {
            return f.clone();
        }
        let font = load_font(path)
            .map(Arc::new)
            .unwrap_or_else(|| self.default.clone());
        cache.insert(path.to_owned(), font.clone());
        font
    }

    pub fn name_of(&self, path: &str) -> String {
        let p = if path.is_empty() {
            &self.default_path
        } else {
            path
        };
        Path::new(p)
            .file_stem()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| "Default".into())
    }
}

/// A primary font with a CJK fallback for characters it lacks.
struct Shaper<'a> {
    fonts: Vec<&'a FontVec>,
}

impl Shaper<'_> {
    fn pick(&self, c: char) -> usize {
        if c.is_whitespace() {
            return 0;
        }
        for (i, f) in self.fonts.iter().enumerate() {
            if f.glyph_id(c).0 != 0 {
                return i;
            }
        }
        0
    }

    fn advance(&self, c: char, px: f32) -> f32 {
        let f = self.fonts[self.pick(c)];
        f.as_scaled(PxScale::from(px)).h_advance(f.glyph_id(c))
    }

    fn width(&self, s: &str, px: f32) -> f32 {
        s.chars().map(|c| self.advance(c, px)).sum()
    }

    fn ascent(&self, px: f32) -> f32 {
        self.fonts[0].as_scaled(PxScale::from(px)).ascent()
    }

    fn height(&self, px: f32) -> f32 {
        let s = self.fonts[0].as_scaled(PxScale::from(px));
        s.ascent() - s.descent()
    }
}

fn is_cjk(c: char) -> bool {
    matches!(c as u32,
        0x3000..=0x30FF | 0x3400..=0x4DBF | 0x4E00..=0x9FFF | 0xF900..=0xFAFF |
        0xFF00..=0xFFEF | 0xAC00..=0xD7AF | 0x1100..=0x11FF)
}

/// Words, spaces and single CJK characters (which may break anywhere).
fn tokens(para: &str) -> Vec<(String, bool)> {
    let mut out: Vec<(String, bool)> = Vec::new();
    for c in para.chars() {
        let space = c.is_whitespace();
        if is_cjk(c) {
            out.push((c.to_string(), false));
            continue;
        }
        match out.last_mut() {
            Some((s, sp)) if *sp == space && !s.chars().last().is_some_and(is_cjk) => s.push(c),
            _ => out.push((c.to_string(), space)),
        }
    }
    out
}

/// Greedy wrapping; the flag says a word had to be split to fit.
fn wrap(sh: &Shaper, text: &str, px: f32, max_w: f32) -> (Vec<String>, bool) {
    let mut lines = Vec::new();
    let mut split = false;
    for para in text.split('\n') {
        let mut line = String::new();
        let mut w = 0.0;
        for (tok, space) in tokens(para) {
            let tw = sh.width(&tok, px);
            if space {
                if !line.is_empty() {
                    line.push_str(&tok);
                    w += tw;
                }
                continue;
            }
            if w + tw > max_w && !line.trim().is_empty() {
                lines.push(line.trim_end().to_owned());
                line.clear();
                w = 0.0;
            }
            if tw > max_w {
                if !is_cjk(tok.chars().next().unwrap_or(' ')) {
                    split = true;
                }
                for c in tok.chars() {
                    let cw = sh.advance(c, px);
                    if w + cw > max_w && !line.is_empty() {
                        lines.push(line.trim_end().to_owned());
                        line.clear();
                        w = 0.0;
                    }
                    line.push(c);
                    w += cw;
                }
            } else {
                line.push_str(&tok);
                w += tw;
            }
        }
        lines.push(line.trim_end().to_owned());
    }
    (lines, split)
}

/// Margins (x, y) around the text at size `px`. A balloon (oval cover) keeps `inset_em`
/// letter widths of air from its outline (the original's `Frame::DEFAULT_INSET` = 1 em,
/// koharu-scene values.rs:296) and uses the rectangle inscribed in the oval; a plain box
/// keeps a thin margin.
fn margins(wf: f32, hf: f32, oval: bool, stroke: f32, px: f32, inset_em: f32) -> (f32, f32) {
    if oval {
        let inset = px * inset_em;
        let a = (wf / 2.0 - inset).max(2.0);
        let b = (hf / 2.0 - inset).max(2.0);
        let (iw, ih) = (a * std::f32::consts::SQRT_2, b * std::f32::consts::SQRT_2);
        (((wf - iw) / 2.0).max(1.0) + stroke, ((hf - ih) / 2.0).max(1.0) + stroke)
    } else {
        ((wf * 0.05).max(2.0) + stroke, (hf * 0.05).max(2.0) + stroke)
    }
}

pub struct Fitted {
    pub px: f32,
    pub lines: Vec<String>,
    pub pad_x: f32,
    pub pad_y: f32,
}

/// Fits `text` in a `wf`×`hf` box the way the original's fit.rs does:
/// - a set size is kept when its whole words fit; otherwise it shrinks, whole words
///   only, to the largest scale down to half the set size (fit.rs:66, floor 0.5);
/// - at that floor a balloon's margin narrows to half (fit.rs:89), and only then are
///   words broken;
/// - auto size takes the largest size whose whole words fit (grow, up to MAX_PX).
fn fit(sh: &Shaper, text: &str, wf: f32, hf: f32, oval: bool, stroke: f32, size: Option<f32>) -> Fitted {
    let attempt = |px: f32, inset_em: f32| {
        let (pad_x, pad_y) = margins(wf, hf, oval, stroke, px, inset_em);
        let rw = (wf - 2.0 * pad_x).max(4.0);
        let rh = (hf - 2.0 * pad_y).max(4.0);
        let (lines, split) = wrap(sh, text, px, rw);
        let ok = !split && lines.len() as f32 * sh.height(px) * LINE_HEIGHT <= rh;
        (ok, Fitted { px, lines, pad_x, pad_y })
    };
    let largest = |lo: f32, hi: f32| -> Option<f32> {
        if !attempt(lo, 1.0).0 {
            return None;
        }
        let (mut lo, mut hi) = (lo, hi.max(lo));
        for _ in 0..HALVINGS {
            let mid = (lo + hi) / 2.0;
            if attempt(mid, 1.0).0 {
                lo = mid;
            } else {
                hi = mid;
            }
        }
        Some((lo * 10.0).floor() / 10.0)
    };
    match size {
        Some(set) => {
            let (ok, at_set) = attempt(set, 1.0);
            if ok {
                return at_set;
            }
            let floor = set * SHRINK_FLOOR;
            match largest(floor, set) {
                Some(px) => attempt(px, 1.0).1,
                None => attempt(floor, 0.5).1,
            }
        }
        None => match largest(MIN_PX, MAX_PX.min(hf.max(MIN_PX))) {
            Some(px) => attempt(px, 1.0).1,
            None => attempt(MIN_PX, 0.5).1,
        },
    }
}

fn over(dst: &mut [u8], src: [u8; 4], alpha: f32) {
    let a = (src[3] as f32 / 255.0) * alpha.clamp(0.0, 1.0);
    if a <= 0.0 {
        return;
    }
    let da = dst[3] as f32 / 255.0;
    let out_a = a + da * (1.0 - a);
    for i in 0..3 {
        let c = (src[i] as f32 * a + dst[i] as f32 * da * (1.0 - a)) / out_a.max(1e-6);
        dst[i] = c.round().clamp(0.0, 255.0) as u8;
    }
    dst[3] = (out_a * 255.0).round() as u8;
}

/// The box as pixels, `w`×`h` of the page.
pub fn render_block(b: &Block, fonts: &FontStore) -> RgbaImage {
    let w = b.w.round().max(1.0) as u32;
    let h = b.h.round().max(1.0) as u32;
    let mut img = RgbaImage::new(w, h);
    let (wf, hf) = (w as f32, h as f32);

    if b.cover {
        let (cx, cy, rx, ry) = (wf / 2.0, hf / 2.0, wf / 2.0, hf / 2.0);
        for y in 0..h {
            for x in 0..w {
                let a = match b.cover_shape {
                    CoverShape::Box => 1.0,
                    CoverShape::Oval => {
                        let dx = (x as f32 + 0.5 - cx) / rx;
                        let dy = (y as f32 + 0.5 - cy) / ry;
                        let d = (dx * dx + dy * dy).sqrt();
                        ((1.0 - d) * rx.min(ry) + 0.5).clamp(0.0, 1.0)
                    }
                };
                over(&mut img.get_pixel_mut(x, y).0, b.cover_color, a);
            }
        }
    }

    let text = if b.uppercase {
        b.translation.to_uppercase()
    } else {
        b.translation.clone()
    };
    if text.trim().is_empty() {
        return img;
    }
    let primary = fonts.get(&b.font);
    let mut list: Vec<&FontVec> = vec![&primary];
    if let Some(c) = fonts.cjk.as_deref() {
        list.push(c);
    }
    let sh = Shaper { fonts: list };
    let oval = b.cover && b.cover_shape == CoverShape::Oval;
    let Fitted { px, lines, pad_x, pad_y } = fit(&sh, &text, wf, hf, oval, b.stroke_width, b.size);
    let room_w = (wf - 2.0 * pad_x).max(4.0);
    let room_h = (hf - 2.0 * pad_y).max(4.0);
    let line_h = sh.height(px) * LINE_HEIGHT;
    let block_h = lines.len() as f32 * line_h;
    let top = pad_y + (room_h - block_h) / 2.0;

    let mut mask = vec![0f32; (w * h) as usize];
    for (i, line) in lines.iter().enumerate() {
        let lw = sh.width(line, px);
        let x0 = match b.align {
            Align::Left => pad_x,
            Align::Center => pad_x + (room_w - lw) / 2.0,
            Align::Right => pad_x + room_w - lw,
        };
        let base = top + i as f32 * line_h + (line_h - sh.height(px)) / 2.0 + sh.ascent(px);
        let mut x = x0;
        for c in line.chars() {
            let f = sh.fonts[sh.pick(c)];
            let id = f.glyph_id(c);
            let glyph = id.with_scale_and_position(PxScale::from(px), ab_glyph::point(x, base));
            if let Some(o) = f.outline_glyph(glyph) {
                let bb = o.px_bounds();
                o.draw(|gx, gy, cov| {
                    let px_ = bb.min.x as i32 + gx as i32;
                    let py_ = bb.min.y as i32 + gy as i32;
                    if px_ >= 0 && py_ >= 0 && (px_ as u32) < w && (py_ as u32) < h {
                        let m = &mut mask[(py_ as u32 * w + px_ as u32) as usize];
                        *m = m.max(cov);
                    }
                });
            }
            x += f.as_scaled(PxScale::from(px)).h_advance(id);
        }
    }

    if b.stroke_width > 0.0 {
        let r = b.stroke_width;
        let ri = r.ceil() as i32;
        let mut offsets = Vec::new();
        for dy in -ri..=ri {
            for dx in -ri..=ri {
                let d = ((dx * dx + dy * dy) as f32).sqrt();
                if d <= r + 0.5 {
                    offsets.push((dx, dy, (r + 0.5 - d).clamp(0.0, 1.0)));
                }
            }
        }
        let mut outline = vec![0f32; mask.len()];
        for y in 0..h as i32 {
            for x in 0..w as i32 {
                let m = mask[(y as u32 * w + x as u32) as usize];
                if m <= 0.0 {
                    continue;
                }
                for &(dx, dy, edge) in &offsets {
                    let (nx, ny) = (x + dx, y + dy);
                    if nx >= 0 && ny >= 0 && nx < w as i32 && ny < h as i32 {
                        let o = &mut outline[(ny as u32 * w + nx as u32) as usize];
                        *o = o.max(m * edge);
                    }
                }
            }
        }
        for (i, p) in img.pixels_mut().enumerate() {
            if outline[i] > 0.0 {
                over(&mut p.0, b.stroke, outline[i]);
            }
        }
    }
    for (i, p) in img.pixels_mut().enumerate() {
        if mask[i] > 0.0 {
            over(&mut p.0, b.color, mask[i]);
        }
    }
    img
}

/// Pastes a rendered box onto a page at its position.
pub fn paste(page: &mut RgbaImage, layer: &RgbaImage, x: f32, y: f32) {
    let (ox, oy) = (x.round() as i64, y.round() as i64);
    for (lx, ly, p) in layer.enumerate_pixels() {
        if p.0[3] == 0 {
            continue;
        }
        let (px, py) = (ox + lx as i64, oy + ly as i64);
        if px < 0 || py < 0 || px >= page.width() as i64 || py >= page.height() as i64 {
            continue;
        }
        let dst = page.get_pixel_mut(px as u32, py as u32);
        over(&mut dst.0, p.0, 1.0);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn set_size_shrinks_to_half_at_most() {
        let fonts = FontStore::new("");
        let sh = Shaper { fonts: vec![&*fonts.default] };
        let text = "A rather long sentence that cannot fit";
        // Roomy box: the set size is kept.
        assert_eq!(fit(&sh, "Hi", 400.0, 200.0, false, 0.0, Some(40.0)).px, 40.0);
        // Tight box: shrinks, never below half the set size.
        let f = fit(&sh, text, 120.0, 60.0, false, 0.0, Some(40.0));
        assert!(f.px >= 20.0 && f.px < 40.0, "px {}", f.px);
        // Auto size grows to fill a big box.
        assert!(fit(&sh, "Hi", 400.0, 200.0, false, 0.0, None).px > 60.0);
    }
}
