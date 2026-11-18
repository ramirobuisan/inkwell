//! The cleanup layer: a picture the size of the page, over the artwork and under the
//! text boxes, painted with the brush and cleared with the eraser (the original's brush
//! and eraser tools, which paint its "Cleanup" layer). Each page's layer is kept as a PNG
//! in the comic's `paint/` folder, written atomically like everything else.

use std::path::Path;

use image::RgbaImage;

/// The pixels a stroke replaced, to undo it (and, swapped back, to redo it).
#[derive(Clone)]
pub struct Patch {
    pub x: u32,
    pub y: u32,
    pub img: RgbaImage,
}

/// A rectangle of pixels, `[x0, y0, x1, y1)`.
pub type Bounds = [u32; 4];

pub fn union(a: Option<Bounds>, b: Bounds) -> Bounds {
    match a {
        None => b,
        Some(a) => [a[0].min(b[0]), a[1].min(b[1]), a[2].max(b[2]), a[3].max(b[3])],
    }
}

/// Paints (or erases) a round brush of `radius` along the segment `a`→`b`, soft by half a
/// pixel at its edge. Returns the pixels touched.
pub fn stroke(img: &mut RgbaImage, a: (f32, f32), b: (f32, f32), radius: f32, color: [u8; 4], erase: bool) -> Option<Bounds> {
    let (w, h) = (img.width() as f32, img.height() as f32);
    let r = radius.max(0.5);
    let x0 = (a.0.min(b.0) - r - 1.0).floor().clamp(0.0, w);
    let y0 = (a.1.min(b.1) - r - 1.0).floor().clamp(0.0, h);
    let x1 = (a.0.max(b.0) + r + 1.0).ceil().clamp(0.0, w);
    let y1 = (a.1.max(b.1) + r + 1.0).ceil().clamp(0.0, h);
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    let (dx, dy) = (b.0 - a.0, b.1 - a.1);
    let len2 = dx * dx + dy * dy;
    for y in y0 as u32..y1 as u32 {
        for x in x0 as u32..x1 as u32 {
            let (px, py) = (x as f32 + 0.5, y as f32 + 0.5);
            let t = if len2 > 0.0 { (((px - a.0) * dx + (py - a.1) * dy) / len2).clamp(0.0, 1.0) } else { 0.0 };
            let (cx, cy) = (a.0 + t * dx, a.1 + t * dy);
            let d = ((px - cx).powi(2) + (py - cy).powi(2)).sqrt();
            let cov = (r - d + 0.5).clamp(0.0, 1.0);
            if cov <= 0.0 {
                continue;
            }
            let p = img.get_pixel_mut(x, y);
            if erase {
                p.0[3] = (p.0[3] as f32 * (1.0 - cov)).round() as u8;
            } else {
                let sa = color[3] as f32 / 255.0 * cov;
                let da = p.0[3] as f32 / 255.0;
                let oa = sa + da * (1.0 - sa);
                for i in 0..3 {
                    let c = (color[i] as f32 * sa + p.0[i] as f32 * da * (1.0 - sa)) / oa.max(1e-6);
                    p.0[i] = c.round().clamp(0.0, 255.0) as u8;
                }
                p.0[3] = (oa * 255.0).round() as u8;
            }
        }
    }
    Some([x0 as u32, y0 as u32, x1 as u32, y1 as u32])
}

pub fn crop(img: &RgbaImage, b: Bounds) -> RgbaImage {
    image::imageops::crop_imm(img, b[0], b[1], b[2] - b[0], b[3] - b[1]).to_image()
}

/// Puts `patch` into `img` and returns the pixels it covered before.
pub fn swap(img: &mut RgbaImage, patch: &Patch) -> Patch {
    let b = [patch.x, patch.y, patch.x + patch.img.width(), patch.y + patch.img.height()];
    let old = Patch { x: patch.x, y: patch.y, img: crop(img, b) };
    image::imageops::replace(img, &patch.img, patch.x as i64, patch.y as i64);
    old
}

/// The layer's file, relative to the comic's folder, for a page picture `file`.
pub fn file_for(page_file: &str) -> String {
    let stem = Path::new(page_file)
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "page".into());
    format!("paint/{stem}.png")
}

pub fn load(dir: &Path, rel: &str) -> Option<RgbaImage> {
    image::open(dir.join(rel)).ok().map(|i| i.to_rgba8())
}

pub fn save(dir: &Path, rel: &str, img: &RgbaImage) -> Result<(), String> {
    let path = dir.join(rel);
    if let Some(d) = path.parent() {
        std::fs::create_dir_all(d).map_err(|e| e.to_string())?;
    }
    let mut bytes = std::io::Cursor::new(Vec::new());
    img.write_to(&mut bytes, image::ImageFormat::Png).map_err(|e| e.to_string())?;
    crate::project::atomic_write(&path, bytes.get_ref()).map_err(|e| e.to_string())?;
    let _ = std::fs::remove_file(crate::project::with_suffix(&path, ".bak"));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stroke_paints_erases_and_undoes() {
        let mut img = RgbaImage::new(100, 100);
        let before = img.clone();
        let b = stroke(&mut img, (10.0, 50.0), (90.0, 50.0), 5.0, [255, 255, 255, 255], false).unwrap();
        assert_eq!(img.get_pixel(50, 50).0, [255, 255, 255, 255]);
        assert_eq!(img.get_pixel(50, 10).0[3], 0);
        let patch = Patch { x: b[0], y: b[1], img: crop(&before, b) };
        let redo = swap(&mut img, &patch);
        assert_eq!(img.get_pixel(50, 50).0[3], 0, "undo restores");
        swap(&mut img, &redo);
        assert_eq!(img.get_pixel(50, 50).0[3], 255, "redo repaints");
        stroke(&mut img, (50.0, 40.0), (50.0, 60.0), 6.0, [0; 4], true);
        assert_eq!(img.get_pixel(50, 50).0[3], 0, "eraser clears");
    }
}
