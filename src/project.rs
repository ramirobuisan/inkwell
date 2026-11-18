//! A project is one folder: `pages/` holds the page pictures, `project.json` the text
//! boxes and settings. Saving is atomic (temp file, fsync, rename) with a backup, so a
//! crash at any moment leaves a readable project behind.

use std::{
    collections::hash_map::DefaultHasher,
    fs,
    hash::{Hash, Hasher},
    io::{self, Write},
    path::{Path, PathBuf},
    time::SystemTime,
};

use serde::{Deserialize, Serialize};

pub const FILE: &str = "project.json";
pub const IMPORT_MARKER: &str = "import.json";

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
pub enum Align {
    Left,
    #[default]
    Center,
    Right,
}

#[derive(Clone, Copy, Serialize, Deserialize, PartialEq, Eq, Debug, Default)]
pub enum CoverShape {
    #[default]
    Oval,
    Box,
}

/// One text box on a page, in page pixels.
#[derive(Clone, Serialize, Deserialize, Debug, PartialEq)]
#[serde(default)]
pub struct Block {
    pub id: u64,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub source: String,
    pub translation: String,
    /// Font file path; empty means the default font.
    pub font: String,
    /// Fixed size in page pixels; `None` fits the text to the box.
    pub size: Option<f32>,
    pub color: [u8; 4],
    pub stroke: [u8; 4],
    pub stroke_width: f32,
    pub align: Align,
    pub uppercase: bool,
    /// Paint over the original lettering before drawing the translation.
    pub cover: bool,
    pub cover_shape: CoverShape,
    pub cover_color: [u8; 4],
    pub visible: bool,
}

impl Default for Block {
    fn default() -> Self {
        Self {
            id: 0,
            x: 0.0,
            y: 0.0,
            w: 100.0,
            h: 60.0,
            source: String::new(),
            translation: String::new(),
            font: String::new(),
            size: None,
            color: [20, 20, 20, 255],
            stroke: [255, 255, 255, 255],
            stroke_width: 0.0,
            align: Align::Center,
            uppercase: false,
            cover: true,
            cover_shape: CoverShape::Oval,
            cover_color: [255, 255, 255, 255],
            visible: true,
        }
    }
}

impl Block {
    /// Changes whenever the box's picture would change (position aside).
    pub fn render_key(&self) -> u64 {
        let mut h = DefaultHasher::new();
        self.w.to_bits().hash(&mut h);
        self.h.to_bits().hash(&mut h);
        self.translation.hash(&mut h);
        self.font.hash(&mut h);
        self.size.map(f32::to_bits).hash(&mut h);
        self.color.hash(&mut h);
        self.stroke.hash(&mut h);
        self.stroke_width.to_bits().hash(&mut h);
        (self.align as u8).hash(&mut h);
        self.uppercase.hash(&mut h);
        self.cover.hash(&mut h);
        (self.cover_shape as u8).hash(&mut h);
        self.cover_color.hash(&mut h);
        h.finish()
    }

    pub fn label(&self) -> String {
        let text = if self.translation.trim().is_empty() {
            &self.source
        } else {
            &self.translation
        };
        let one_line: String = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.is_empty() {
            "(empty box)".to_owned()
        } else {
            one_line
        }
    }
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Page {
    /// Picture path relative to the project folder.
    pub file: String,
    pub name: String,
    pub width: u32,
    pub height: u32,
    pub blocks: Vec<Block>,
}

#[derive(Clone, Serialize, Deserialize, Debug, Default)]
#[serde(default)]
pub struct Project {
    pub version: u32,
    pub name: String,
    pub source: Option<PathBuf>,
    pub pages: Vec<Page>,
    pub current: usize,
    pub next_id: u64,
}

impl Project {
    pub fn new(name: String, source: Option<PathBuf>, pages: Vec<Page>) -> Self {
        Self {
            version: 1,
            name,
            source,
            pages,
            current: 0,
            next_id: 1,
        }
    }
}

/// Writes `bytes` so that `path` always holds either the old or the new contents: a temp
/// file is written and flushed to disk, the old file becomes `.bak`, the temp file takes
/// its place.
pub fn atomic_write(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let tmp = with_suffix(path, ".tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    if path.exists() {
        let _ = fs::rename(path, with_suffix(path, ".bak"));
    }
    fs::rename(&tmp, path)?;
    if let Some(dir) = path.parent() {
        if let Ok(d) = fs::File::open(dir) {
            let _ = d.sync_all();
        }
    }
    Ok(())
}

pub fn with_suffix(path: &Path, suffix: &str) -> PathBuf {
    let mut s = path.as_os_str().to_owned();
    s.push(suffix);
    PathBuf::from(s)
}

/// Loads a project folder. When `project.json` is missing or damaged (a crash in the
/// middle of a save), the temp copy and then the backup are tried; the note says so.
pub fn load(dir: &Path) -> Result<(Project, Option<String>), String> {
    let main = dir.join(FILE);
    let candidates = [
        (main.clone(), None),
        (
            with_suffix(&main, ".tmp"),
            Some("The last save was interrupted; recovered the newest copy."),
        ),
        (
            with_suffix(&main, ".bak"),
            Some("The project file was damaged; restored the previous save."),
        ),
    ];
    let mut last_err = String::from("no project file");
    for (path, note) in candidates {
        match fs::read(&path) {
            Ok(bytes) => match serde_json::from_slice::<Project>(&bytes) {
                Ok(project) => return Ok((project, note.map(str::to_owned))),
                Err(e) => last_err = format!("{}: {e}", path.display()),
            },
            Err(e) if path == main => last_err = format!("{}: {e}", path.display()),
            Err(_) => {}
        }
    }
    Err(last_err)
}

pub fn to_json(project: &Project) -> String {
    serde_json::to_string_pretty(project).unwrap_or_default()
}

pub fn save_json(dir: &Path, json: &str) -> io::Result<()> {
    atomic_write(&dir.join(FILE), json.as_bytes())
}

pub struct LibraryEntry {
    pub dir: PathBuf,
    pub name: String,
    pub pages: usize,
    pub modified: Option<SystemTime>,
    /// An import that didn't finish (the program closed while unpacking).
    pub interrupted: bool,
}

/// Every project folder in the library: the last changed first, ties in name order with
/// numbers by value.
pub fn list_library(root: &Path) -> Vec<LibraryEntry> {
    let Ok(read) = fs::read_dir(root) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    for entry in read.flatten() {
        let dir = entry.path();
        if !dir.is_dir() {
            continue;
        }
        let interrupted = dir.join(IMPORT_MARKER).exists();
        let file = dir.join(FILE);
        if !file.exists() && !interrupted && !with_suffix(&file, ".bak").exists() {
            continue;
        }
        let modified = fs::metadata(&file)
            .or_else(|_| fs::metadata(&dir))
            .and_then(|m| m.modified())
            .ok();
        let pages = if interrupted {
            0
        } else {
            load(&dir).map(|(p, _)| p.pages.len()).unwrap_or(0)
        };
        let name = dir
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push(LibraryEntry {
            dir,
            name,
            pages,
            modified,
            interrupted,
        });
    }
    out.sort_by(|a, b| {
        b.modified
            .cmp(&a.modified)
            .then_with(|| natural_cmp(&a.name, &b.name))
    });
    out
}

/// Compares names the way people count: "page2" before "page10".
pub fn natural_cmp(a: &str, b: &str) -> std::cmp::Ordering {
    use std::cmp::Ordering;
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let mut na = String::new();
                while let Some(c) = ai.peek().copied().filter(char::is_ascii_digit) {
                    na.push(c);
                    ai.next();
                }
                let mut nb = String::new();
                while let Some(c) = bi.peek().copied().filter(char::is_ascii_digit) {
                    nb.push(c);
                    bi.next();
                }
                let ta = na.trim_start_matches('0');
                let tb = nb.trim_start_matches('0');
                let ord = ta.len().cmp(&tb.len()).then_with(|| ta.cmp(tb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x
                    .to_lowercase()
                    .cmp(y.to_lowercase())
                    .then_with(|| x.cmp(&y));
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_sort_by_value() {
        let mut v = vec!["page10.jpg", "page2.jpg", "Page1.jpg", "page02b.jpg"];
        v.sort_by(|a, b| natural_cmp(a, b));
        assert_eq!(v, ["Page1.jpg", "page2.jpg", "page02b.jpg", "page10.jpg"]);
    }

    #[test]
    fn damaged_save_falls_back_to_backup() {
        let dir = std::env::temp_dir().join(format!("inkwell-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let mut p = Project::new("A".into(), None, vec![Page::default()]);
        save_json(&dir, &to_json(&p)).unwrap();
        p.name = "B".into();
        save_json(&dir, &to_json(&p)).unwrap();
        // A crash left a half-written main file.
        fs::write(dir.join(FILE), b"{\"version\": 1, \"na").unwrap();
        let (loaded, note) = load(&dir).unwrap();
        assert_eq!(loaded.name, "A");
        assert!(note.is_some());
        let _ = fs::remove_dir_all(&dir);
    }
}
