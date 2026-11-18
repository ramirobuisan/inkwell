//! A page as a layered Photoshop document, as the original's koharu-psd does: the
//! artwork at the bottom, the cleanup above it, then every text box, each its own
//! layer named by its text (hidden boxes hidden). Layers here are pixel layers; the
//! original also writes editable type layers, which this rebuild doesn't.
//! Format: Adobe's PSD specification (header, layer records, raw channel data, the
//! merged image), version 1, 8 bits per channel, RGB.

use image::RgbaImage;

/// A PSD's width and height range from 1 to 30,000 (larger needs PSB), as the original
/// refuses larger pages.
pub const MAX_SIDE: u32 = 30_000;

pub struct Layer {
    pub name: String,
    pub left: i32,
    pub top: i32,
    pub pixels: RgbaImage,
    pub visible: bool,
}

/// The smallest rectangle holding the image's visible pixels, or `None` when it's empty.
pub fn trim(img: &RgbaImage) -> Option<(u32, u32, RgbaImage)> {
    let (w, h) = img.dimensions();
    let (mut x0, mut y0, mut x1, mut y1) = (w, h, 0, 0);
    for (x, y, p) in img.enumerate_pixels() {
        if p.0[3] > 0 {
            x0 = x0.min(x);
            y0 = y0.min(y);
            x1 = x1.max(x + 1);
            y1 = y1.max(y + 1);
        }
    }
    (x1 > x0).then(|| (x0, y0, image::imageops::crop_imm(img, x0, y0, x1 - x0, y1 - y0).to_image()))
}

fn u16be(out: &mut Vec<u8>, v: u16) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn u32be(out: &mut Vec<u8>, v: u32) {
    out.extend_from_slice(&v.to_be_bytes());
}
fn i32be(out: &mut Vec<u8>, v: i32) {
    out.extend_from_slice(&v.to_be_bytes());
}

/// A layer name as a Pascal string padded to a multiple of 4 (ASCII, `?` for the rest),
/// followed by the `luni` block holding the full Unicode name.
fn names(out: &mut Vec<u8>, name: &str) {
    let ascii: Vec<u8> = name.chars().take(255).map(|c| if c.is_ascii() && !c.is_control() { c as u8 } else { b'?' }).collect();
    let start = out.len();
    out.push(ascii.len() as u8);
    out.extend_from_slice(&ascii);
    while (out.len() - start) % 4 != 0 {
        out.push(0);
    }
    let utf16: Vec<u16> = name.encode_utf16().collect();
    let mut data = Vec::new();
    u32be(&mut data, utf16.len() as u32);
    for c in &utf16 {
        u16be(&mut data, *c);
    }
    while data.len() % 4 != 0 {
        data.push(0);
    }
    out.extend_from_slice(b"8BIMluni");
    u32be(out, data.len() as u32);
    out.extend_from_slice(&data);
}

/// Planar channel bytes of `img` in the order R, G, B, A.
fn planes(img: &RgbaImage) -> [Vec<u8>; 4] {
    let n = (img.width() * img.height()) as usize;
    let mut p = [Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n), Vec::with_capacity(n)];
    for px in img.pixels() {
        for c in 0..4 {
            p[c].push(px.0[c]);
        }
    }
    p
}

/// The document: `layers` bottom first, and `merged`, the flattened page.
pub fn write(merged: &RgbaImage, layers: &[Layer]) -> Result<Vec<u8>, String> {
    let (w, h) = merged.dimensions();
    if w == 0 || h == 0 || w > MAX_SIDE || h > MAX_SIDE {
        return Err(format!("A PSD page can be at most {MAX_SIDE} pixels on a side; this one is {w}×{h}."));
    }
    let mut out = Vec::new();
    out.extend_from_slice(b"8BPS");
    u16be(&mut out, 1);
    out.extend_from_slice(&[0; 6]);
    u16be(&mut out, 3); // merged image: RGB
    u32be(&mut out, h);
    u32be(&mut out, w);
    u16be(&mut out, 8);
    u16be(&mut out, 3); // RGB colour mode
    u32be(&mut out, 0); // colour mode data
    u32be(&mut out, 0); // image resources

    // Layer info: records, then each layer's channel data.
    let mut info = Vec::new();
    u16be(&mut info, layers.len() as u16);
    let mut channel_data = Vec::new();
    for l in layers {
        let (lw, lh) = l.pixels.dimensions();
        i32be(&mut info, l.top);
        i32be(&mut info, l.left);
        i32be(&mut info, l.top + lh as i32);
        i32be(&mut info, l.left + lw as i32);
        u16be(&mut info, 4);
        let p = planes(&l.pixels);
        for (id, plane) in [(-1i16, &p[3]), (0, &p[0]), (1, &p[1]), (2, &p[2])] {
            info.extend_from_slice(&id.to_be_bytes());
            u32be(&mut info, 2 + plane.len() as u32);
            u16be(&mut channel_data, 0); // raw
            channel_data.extend_from_slice(plane);
        }
        info.extend_from_slice(b"8BIMnorm");
        info.push(255); // opacity
        info.push(0); // clipping
        info.push(if l.visible { 0 } else { 2 }); // flags: bit 1 = hidden
        info.push(0);
        let mut extra = Vec::new();
        u32be(&mut extra, 0); // layer mask data
        u32be(&mut extra, 0); // blending ranges
        names(&mut extra, &l.name);
        u32be(&mut info, extra.len() as u32);
        info.extend_from_slice(&extra);
    }
    info.extend_from_slice(&channel_data);
    if info.len() % 2 != 0 {
        info.push(0);
    }
    let mut section = Vec::new();
    u32be(&mut section, info.len() as u32);
    section.extend_from_slice(&info);
    u32be(&mut section, 0); // global layer mask info
    u32be(&mut out, section.len() as u32);
    out.extend_from_slice(&section);

    // Merged image, raw, planar R, G, B.
    u16be(&mut out, 0);
    let p = planes(merged);
    for plane in &p[..3] {
        out.extend_from_slice(plane);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn writes_a_readable_header_and_layers() {
        let merged = RgbaImage::from_pixel(8, 6, image::Rgba([255, 255, 255, 255]));
        let layers = vec![
            Layer { name: "Original artwork".into(), left: 0, top: 0, pixels: merged.clone(), visible: true },
            Layer { name: "こんにちは".into(), left: 2, top: 1, pixels: RgbaImage::from_pixel(3, 2, image::Rgba([0, 0, 0, 255])), visible: false },
        ];
        let psd = write(&merged, &layers).unwrap();
        assert_eq!(&psd[..4], b"8BPS");
        assert_eq!(u32::from_be_bytes(psd[14..18].try_into().unwrap()), 6); // height
        assert_eq!(u32::from_be_bytes(psd[18..22].try_into().unwrap()), 8); // width
        // Section lengths add up to the whole file.
        let section = u32::from_be_bytes(psd[34..38].try_into().unwrap()) as usize;
        assert_eq!(38 + section + 2 + 8 * 6 * 3, psd.len());
        assert_eq!(u16::from_be_bytes(psd[42..44].try_into().unwrap()), 2); // layer count
        assert!(trim(&RgbaImage::new(4, 4)).is_none());
    }
}
