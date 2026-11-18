//! The helpers that replace the original's GPU models: Tesseract finds and reads text,
//! and any OpenAI-compatible chat server (Ollama, LM Studio, a hosted API) translates.

use std::{
    path::Path,
    process::Command,
    sync::atomic::{AtomicU64, Ordering},
    time::Duration,
};

use image::{DynamicImage, RgbaImage};
use serde_json::{json, Map, Value};

use crate::{project::Block, settings::Settings};

static TEMP: AtomicU64 = AtomicU64::new(0);

/// Languages Tesseract has installed, or `None` when it isn't installed.
pub fn tesseract_langs() -> Option<Vec<String>> {
    let out = Command::new("tesseract")
        .arg("--list-langs")
        .output()
        .ok()?;
    let text =
        String::from_utf8_lossy(&out.stdout).into_owned() + &String::from_utf8_lossy(&out.stderr);
    Some(
        text.lines()
            .skip_while(|l| !l.contains("available languages"))
            .skip(1)
            .map(|l| l.trim().to_owned())
            .filter(|l| !l.is_empty())
            .collect(),
    )
}

fn is_cjk_lang(lang: &str) -> bool {
    ["jpn", "chi", "kor"].iter().any(|p| lang.starts_with(p))
}

fn temp_png(img: &DynamicImage) -> Result<std::path::PathBuf, String> {
    let n = TEMP.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("inkwell-{}-{n}.png", std::process::id()));
    img.save(&path).map_err(|e| e.to_string())?;
    Ok(path)
}

fn tesseract(path: &Path, lang: &str, psm: &str, tsv: bool) -> Result<String, String> {
    let mut cmd = Command::new("tesseract");
    cmd.arg(path).arg("stdout").args(["-l", lang, "--psm", psm]);
    if tsv {
        cmd.arg("tsv");
    }
    let out = cmd.output().map_err(|e| {
        format!("Tesseract isn't available ({e}). Install the 'tesseract' package.")
    })?;
    if !out.status.success() && out.stdout.is_empty() {
        return Err(String::from_utf8_lossy(&out.stderr).trim().to_owned());
    }
    Ok(String::from_utf8_lossy(&out.stdout).into_owned())
}

/// Reads one box. Tall boxes in Japanese/Chinese are read as vertical text.
pub fn read_box(
    page: &RgbaImage,
    b: &Block,
    lang: &str,
    langs: &[String],
) -> Result<String, String> {
    let x = b.x.max(0.0) as u32;
    let y = b.y.max(0.0) as u32;
    let w = (b.w as u32).min(page.width().saturating_sub(x));
    let h = (b.h as u32).min(page.height().saturating_sub(y));
    if w < 4 || h < 4 {
        return Ok(String::new());
    }
    let crop = image::imageops::crop_imm(page, x, y, w, h).to_image();
    let vertical = h as f32 > w as f32 * 1.2 && is_cjk_lang(lang);
    let (lang, psm) = if vertical {
        let vert = format!("{lang}_vert");
        if langs.contains(&vert) {
            (vert, "5")
        } else {
            (lang.to_owned(), "5")
        }
    } else {
        (lang.to_owned(), "6")
    };
    let path = temp_png(&DynamicImage::ImageRgba8(crop))?;
    let result = tesseract(&path, &lang, psm, false);
    let _ = std::fs::remove_file(&path);
    Ok(clean(&result?, is_cjk_lang(&lang)))
}

fn clean(text: &str, cjk: bool) -> String {
    if cjk {
        text.chars().filter(|c| !c.is_whitespace()).collect()
    } else {
        text.lines()
            .map(str::trim)
            .filter(|l| !l.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }
}

/// Finds text areas on a whole page: Tesseract's paragraphs with confident words.
pub fn find_text(page: &RgbaImage, lang: &str) -> Result<Vec<Block>, String> {
    let path = temp_png(&DynamicImage::ImageRgba8(page.clone()))?;
    let result = tesseract(&path, lang, "11", true);
    let _ = std::fs::remove_file(&path);
    let tsv = result?;
    let cjk = is_cjk_lang(lang);
    // (block, par) -> (x0, y0, x1, y1, words by line)
    let mut groups: Vec<((u32, u32), [i64; 4], Vec<(u32, String)>)> = Vec::new();
    for line in tsv.lines().skip(1) {
        let f: Vec<&str> = line.split('\t').collect();
        if f.len() < 12 || f[0] != "5" {
            continue;
        }
        let conf: f32 = f[10].parse().unwrap_or(-1.0);
        let text = f[11].trim();
        if conf < 40.0 || text.is_empty() || !text.chars().any(char::is_alphanumeric) {
            continue;
        }
        let n = |i: usize| f[i].parse::<i64>().unwrap_or(0);
        let key = (n(2) as u32, n(3) as u32);
        let (l, t, w, h) = (n(6), n(7), n(8), n(9));
        let line_no = n(4) as u32;
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => {
                g.1 = [
                    g.1[0].min(l),
                    g.1[1].min(t),
                    g.1[2].max(l + w),
                    g.1[3].max(t + h),
                ];
                g.2.push((line_no, text.to_owned()));
            }
            None => groups.push((key, [l, t, l + w, t + h], vec![(line_no, text.to_owned())])),
        }
    }
    let mut blocks = Vec::new();
    for (_, r, words) in groups {
        let (w, h) = ((r[2] - r[0]) as f32, (r[3] - r[1]) as f32);
        if w < 10.0 || h < 8.0 {
            continue;
        }
        let mut text = String::new();
        let mut last = None;
        for (line, word) in words {
            if last.is_some() && last != Some(line) {
                text.push('\n');
            } else if last.is_some() && !cjk {
                text.push(' ');
            }
            text.push_str(&word);
            last = Some(line);
        }
        if text.chars().filter(|c| c.is_alphanumeric()).count() < 2 {
            continue;
        }
        let pad = 6.0;
        blocks.push(Block {
            x: (r[0] as f32 - pad).max(0.0),
            y: (r[1] as f32 - pad).max(0.0),
            w: w + 2.0 * pad,
            h: h + 2.0 * pad,
            source: text,
            ..Block::default()
        });
    }
    Ok(blocks)
}

/// The bubble's own colour: the median colour along the inside edge of the box.
pub fn edge_color(page: &RgbaImage, b: &Block) -> [u8; 4] {
    let (pw, ph) = (page.width() as i64, page.height() as i64);
    let x0 = (b.x as i64 + 2).clamp(0, pw - 1);
    let y0 = (b.y as i64 + 2).clamp(0, ph - 1);
    let x1 = ((b.x + b.w) as i64 - 3).clamp(0, pw - 1);
    let y1 = ((b.y + b.h) as i64 - 3).clamp(0, ph - 1);
    let mut px: Vec<[u8; 4]> = Vec::new();
    for x in x0..=x1 {
        px.push(page.get_pixel(x as u32, y0 as u32).0);
        px.push(page.get_pixel(x as u32, y1 as u32).0);
    }
    for y in y0..=y1 {
        px.push(page.get_pixel(x0 as u32, y as u32).0);
        px.push(page.get_pixel(x1 as u32, y as u32).0);
    }
    if px.is_empty() {
        return [255, 255, 255, 255];
    }
    px.sort_by_key(|p| p[0] as u32 * 299 + p[1] as u32 * 587 + p[2] as u32 * 114);
    let mut m = px[px.len() / 2];
    m[3] = 255;
    m
}

fn chat_url(endpoint: &str) -> String {
    let e = endpoint.trim().trim_end_matches('/');
    if e.ends_with("/chat/completions") {
        e.to_owned()
    } else {
        format!("{e}/chat/completions")
    }
}

/// The original's instruction, in the same shape: texts numbered in reading order as a
/// JSON object, values translated, keys kept, only JSON back.
fn system_prompt(s: &Settings) -> String {
    let base = format!(
        "Translate the manga text in each message into {}. Each message is one page's texts as a JSON object, numbered in reading order: translate only the values and keep the keys and structure unchanged. Reply with the translated JSON only, no explanation.",
        s.target_lang
    );
    if s.instructions.trim().is_empty() {
        base
    } else {
        format!("{base}\n\n{}", s.instructions.trim())
    }
}

/// Translates one page's texts. `history` holds earlier pages (request, reply) for
/// context. Returns the translations in the same order, and the exchange to remember.
pub fn translate(
    s: &Settings,
    texts: &[String],
    history: &[(String, String)],
) -> Result<(Vec<String>, (String, String)), String> {
    let mut obj = Map::new();
    for (i, t) in texts.iter().enumerate() {
        obj.insert((i + 1).to_string(), Value::String(t.clone()));
    }
    let request = Value::Object(obj).to_string();
    let mut messages = vec![json!({"role": "system", "content": system_prompt(s)})];
    for (q, a) in history {
        messages.push(json!({"role": "user", "content": q}));
        messages.push(json!({"role": "assistant", "content": a}));
    }
    messages.push(json!({"role": "user", "content": request}));
    let body = json!({
        "model": s.model,
        "messages": messages,
        "temperature": 0.2,
        "stream": false,
    });
    let mut req = ureq::post(&chat_url(&s.endpoint))
        .timeout(Duration::from_secs(600))
        .set("Content-Type", "application/json");
    if !s.api_key.trim().is_empty() {
        req = req.set("Authorization", &format!("Bearer {}", s.api_key.trim()));
    }
    let resp: Value = req
        .send_json(body)
        .map_err(|e| match e {
            ureq::Error::Status(code, r) => {
                format!("The translation server answered {code}: {}", r.into_string().unwrap_or_default())
            }
            other => format!(
                "Can't reach the translation server at {} ({other}). Start Ollama or check Settings.",
                s.endpoint
            ),
        })?
        .into_json()
        .map_err(|e| format!("The translation server's answer isn't JSON: {e}"))?;
    let content = resp["choices"][0]["message"]["content"]
        .as_str()
        .ok_or("The translation server sent no text back.")?;
    let parsed = parse_reply(content).ok_or_else(|| {
        format!(
            "The model didn't answer with JSON: {}",
            content.chars().take(200).collect::<String>()
        )
    })?;
    let out = (0..texts.len())
        .map(|i| {
            parsed
                .get(&(i + 1).to_string())
                .and_then(|v| v.as_str().map(str::to_owned))
                .unwrap_or_default()
        })
        .collect();
    Ok((out, (request, content.to_owned())))
}

/// The JSON object in a reply, skipping thinking tags and code fences.
fn parse_reply(content: &str) -> Option<Map<String, Value>> {
    let text = match content.rfind("</think>") {
        Some(i) => &content[i + 8..],
        None => content,
    };
    let start = text.find('{')?;
    let end = text.rfind('}')?;
    serde_json::from_str::<Value>(&text[start..=end])
        .ok()?
        .as_object()
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::textfit::{self, FontStore};

    #[test]
    fn reply_parsing_skips_thinking_and_fences() {
        let r = parse_reply("<think>hmm {\"x\":1}</think>\n```json\n{\"1\": \"Hello\", \"2\": \"Bye\"}\n```").unwrap();
        assert_eq!(r["1"], "Hello");
        assert_eq!(r["2"], "Bye");
    }

    #[test]
    fn chat_url_is_completed() {
        assert_eq!(chat_url("http://localhost:11434/v1/"), "http://localhost:11434/v1/chat/completions");
        assert_eq!(chat_url("https://x/v1/chat/completions"), "https://x/v1/chat/completions");
    }

    /// Draws English text on a white page with the app's own text renderer, then has
    /// Tesseract find and read it.
    #[test]
    fn finds_and_reads_english_text() {
        if tesseract_langs().map_or(true, |l| !l.iter().any(|x| x == "eng")) {
            eprintln!("tesseract with eng not installed; skipped");
            return;
        }
        let fonts = FontStore::new("");
        let mut page = RgbaImage::from_pixel(900, 600, image::Rgba([255, 255, 255, 255]));
        let b = Block {
            x: 100.0,
            y: 150.0,
            w: 600.0,
            h: 160.0,
            translation: "HELLO WORLD".into(),
            size: Some(64.0),
            cover: false,
            ..Block::default()
        };
        let layer = textfit::render_block(&b, &fonts);
        textfit::paste(&mut page, &layer, b.x, b.y);
        let found = find_text(&page, "eng").expect("tesseract runs");
        assert!(!found.is_empty(), "no text found");
        let all: String = found.iter().map(|f| f.source.to_uppercase()).collect();
        assert!(all.contains("HELLO"), "found {all:?}");
        let f = &found[0];
        assert!(f.x < 400.0 && f.y > 100.0 && f.y < 300.0, "box at {},{}", f.x, f.y);
        let read = read_box(&page, &b, "eng", &["eng".into()]).unwrap();
        assert!(read.to_uppercase().contains("WORLD"), "read {read:?}");
        assert_eq!(edge_color(&page, &b), [255, 255, 255, 255]);
    }
}
