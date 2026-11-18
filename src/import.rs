//! Unpacking comics into the library. Each dropped CBZ/ZIP (or folder of pictures) gets
//! its own folder: `<library>/<name>/pages/0001.jpg…` plus `project.json`. While it
//! unpacks, `import.json` marks the folder, so an import cut short by a crash resumes on
//! the next start and skips the pages already written.

use std::{
    fs,
    io::{self, BufReader, BufWriter},
    path::{Path, PathBuf},
    sync::mpsc::{channel, Receiver, Sender},
    thread,
};

use eframe::egui;
use serde::{Deserialize, Serialize};

use crate::project::{self, natural_cmp, Page, Project};

pub const ARCHIVES: [&str; 2] = ["cbz", "zip"];
pub const PICTURES: [&str; 4] = ["png", "jpg", "jpeg", "webp"];

/// The files an OS leaves in a folder it archives, which aren't pages.
pub fn is_metadata(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let file = lower.rsplit('/').next().unwrap_or(&lower);
    lower.starts_with("__macosx/")
        || lower.contains("/__macosx/")
        || file.starts_with("._")
        || file == ".ds_store"
        || file == "thumbs.db"
        || file == "desktop.ini"
}

pub fn extension(path: &Path) -> String {
    path.extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default()
}

pub fn is_picture_name(name: &str) -> bool {
    PICTURES.contains(&extension(Path::new(name)).as_str())
}

pub fn is_archive(path: &Path) -> bool {
    ARCHIVES.contains(&extension(path).as_str())
}

#[derive(Serialize, Deserialize)]
struct Marker {
    source: PathBuf,
    folder: bool,
}

pub enum Request {
    /// A CBZ/ZIP, or a folder of pictures, into a new project folder (or the given one,
    /// to resume).
    New {
        source: PathBuf,
        resume: Option<PathBuf>,
    },
    /// Loose pictures added after the open project's last page.
    Append {
        files: Vec<PathBuf>,
        project: PathBuf,
        first_number: usize,
    },
}

pub enum Msg {
    Started { id: u64, name: String },
    Progress { id: u64, done: usize, total: usize },
    Finished { id: u64, dir: PathBuf },
    Appended { project: PathBuf, pages: Vec<Page> },
    Failed { id: u64, error: String },
}

pub struct Importer {
    tx: Sender<(u64, Request)>,
    pub rx: Receiver<Msg>,
    next: u64,
    library: PathBuf,
}

impl Importer {
    /// One background thread unpacks comics one after another, so the disk and memory
    /// stay calm on a laptop.
    pub fn new(ctx: egui::Context, library: PathBuf) -> Self {
        let (tx, jobs) = channel::<(u64, Request)>();
        let (out, rx) = channel::<Msg>();
        let lib = library.clone();
        thread::Builder::new()
            .name("importer".into())
            .spawn(move || {
                let library = lib;
                for (id, req) in jobs {
                    let result = match req {
                        Request::New { source, resume } => {
                            import_new(id, &source, &library, resume, &out, &ctx)
                                .map(|dir| Msg::Finished { id, dir })
                        }
                        Request::Append {
                            files,
                            project,
                            first_number,
                        } => append(&files, &project, first_number)
                            .map(|pages| Msg::Appended { project, pages }),
                    };
                    let _ = out.send(result.unwrap_or_else(|error| Msg::Failed { id, error }));
                    ctx.request_repaint();
                }
            })
            .expect("start the importer");
        Self {
            tx,
            rx,
            next: 1,
            library,
        }
    }

    pub fn library(&self) -> &Path {
        &self.library
    }

    pub fn send(&mut self, req: Request) -> u64 {
        let id = self.next;
        self.next += 1;
        let _ = self.tx.send((id, req));
        id
    }

    /// Imports that a crash cut short, found by their marker.
    pub fn resume_interrupted(&mut self) -> usize {
        let mut n = 0;
        let Ok(read) = fs::read_dir(&self.library) else {
            return 0;
        };
        let dirs: Vec<PathBuf> = read.flatten().map(|e| e.path()).collect();
        for dir in dirs {
            let marker = dir.join(project::IMPORT_MARKER);
            let Ok(bytes) = fs::read(&marker) else {
                continue;
            };
            let Ok(m) = serde_json::from_slice::<Marker>(&bytes) else {
                continue;
            };
            if m.source.exists() {
                self.send(Request::New {
                    source: m.source,
                    resume: Some(dir),
                });
                n += 1;
            }
        }
        n
    }
}

/// `<library>/<name>`, or `<name> (2)`, `(3)`… when taken.
fn unique_dir(library: &Path, name: &str) -> PathBuf {
    let clean: String = name
        .chars()
        .map(|c| if "/\\:*?\"<>|".contains(c) { '_' } else { c })
        .collect();
    let clean = if clean.trim().is_empty() {
        "Comic".to_owned()
    } else {
        clean.trim().to_owned()
    };
    let mut dir = library.join(&clean);
    let mut n = 2;
    while dir.exists() {
        dir = library.join(format!("{clean} ({n})"));
        n += 1;
    }
    dir
}

fn import_new(
    id: u64,
    source: &Path,
    library: &Path,
    resume: Option<PathBuf>,
    out: &Sender<Msg>,
    ctx: &egui::Context,
) -> Result<PathBuf, String> {
    let stem = source
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| "Comic".into());
    let folder = source.is_dir();
    let _ = out.send(Msg::Started {
        id,
        name: stem.clone(),
    });
    ctx.request_repaint();
    fs::create_dir_all(library).map_err(|e| format!("Can't create the library folder: {e}"))?;
    let fresh = resume.is_none();
    let dest = resume.unwrap_or_else(|| unique_dir(library, &stem));
    let pages_dir = dest.join("pages");
    fs::create_dir_all(&pages_dir).map_err(|e| e.to_string())?;
    let marker = serde_json::to_vec(&Marker {
        source: source.to_owned(),
        folder,
    })
    .unwrap_or_default();
    fs::write(&dest.join(project::IMPORT_MARKER), &marker)
        .map_err(|e| e.to_string())?;

    let progress = |done: usize, total: usize| {
        let _ = out.send(Msg::Progress { id, done, total });
        ctx.request_repaint();
    };
    let written = if folder {
        unpack_folder(source, &pages_dir, progress)
    } else {
        unpack_zip(source, &pages_dir, progress)
    };
    let written = match written {
        Ok(w) if !w.is_empty() => w,
        Ok(_) | Err(_) => {
            let error = match written {
                Err(e) => e,
                Ok(_) => format!("{} holds no images", source.display()),
            };
            if fresh {
                // Only the folder this import just made, nothing else.
                let _ = fs::remove_dir_all(&dest);
            }
            return Err(error);
        }
    };
    let pages = written
        .into_iter()
        .map(|(file, name)| page_for(&dest, file, name))
        .collect();
    let name = dest
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or(stem);
    let project = Project::new(name, Some(source.to_owned()), pages);
    project::save_json(&dest, &project::to_json(&project)).map_err(|e| e.to_string())?;
    let _ = fs::remove_file(dest.join(project::IMPORT_MARKER));
    Ok(dest)
}

fn page_for(dest: &Path, file: String, name: String) -> Page {
    let (width, height) = image::image_dimensions(dest.join(&file)).unwrap_or((0, 0));
    Page {
        file,
        name,
        width,
        height,
        blocks: Vec::new(),
    }
}

fn digits(total: usize) -> usize {
    total.to_string().len().max(4)
}

/// Copies `reader` to `path` through a `.part` file, so a half-written page never looks
/// finished.
fn write_page(reader: &mut impl io::Read, path: &Path) -> Result<(), String> {
    let part = project::with_suffix(path, ".part");
    {
        let file = fs::File::create(&part).map_err(|e| e.to_string())?;
        let mut w = BufWriter::new(file);
        io::copy(reader, &mut w).map_err(|e| e.to_string())?;
        w.into_inner()
            .map_err(|e| e.to_string())?
            .sync_all()
            .map_err(|e| e.to_string())?;
    }
    fs::rename(&part, path).map_err(|e| e.to_string())
}

/// Unpacks a ZIP/CBZ's pictures in name order. Returns (relative file, display name).
fn unpack_zip(
    source: &Path,
    pages_dir: &Path,
    progress: impl Fn(usize, usize),
) -> Result<Vec<(String, String)>, String> {
    let file =
        fs::File::open(source).map_err(|e| format!("Can't open {}: {e}", source.display()))?;
    let mut zip = zip::ZipArchive::new(BufReader::new(file))
        .map_err(|e| format!("{} isn't an image or an archive: {e}", source.display()))?;
    let mut members: Vec<(String, usize, u64)> = Vec::new();
    for i in 0..zip.len() {
        let Ok(f) = zip.by_index_raw(i) else { continue };
        if f.is_dir() {
            continue;
        }
        let name = f.name().to_owned();
        if is_picture_name(&name) && !is_metadata(&name) {
            members.push((name, i, f.size()));
        }
    }
    members.sort_by(|a, b| natural_cmp(&a.0, &b.0));
    let total = members.len();
    let width = digits(total);
    let mut out = Vec::with_capacity(total);
    for (n, (name, index, size)) in members.into_iter().enumerate() {
        let ext = extension(Path::new(&name));
        let file = format!("{:0width$}.{ext}", n + 1);
        let path = pages_dir.join(&file);
        let done = fs::metadata(&path)
            .map(|m| m.len() == size)
            .unwrap_or(false);
        if !done {
            let mut member = zip.by_index(index).map_err(|e| e.to_string())?;
            write_page(&mut member, &path)?;
        }
        let display = name.rsplit('/').next().unwrap_or(&name).to_owned();
        out.push((format!("pages/{file}"), display));
        progress(n + 1, total);
    }
    Ok(out)
}

fn unpack_folder(
    source: &Path,
    pages_dir: &Path,
    progress: impl Fn(usize, usize),
) -> Result<Vec<(String, String)>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(source)
        .map_err(|e| e.to_string())?
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.is_file() && is_picture_name(&p.to_string_lossy()))
        .collect();
    files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    let total = files.len();
    let width = digits(total);
    let mut out = Vec::new();
    for (n, src) in files.into_iter().enumerate() {
        let file = format!("{:0width$}.{}", n + 1, extension(&src));
        let path = pages_dir.join(&file);
        let size = fs::metadata(&src).map(|m| m.len()).unwrap_or(0);
        if fs::metadata(&path).map(|m| m.len()).ok() != Some(size) {
            let mut r = fs::File::open(&src).map_err(|e| e.to_string())?;
            write_page(&mut r, &path)?;
        }
        let display = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        out.push((format!("pages/{file}"), display));
        progress(n + 1, total);
    }
    Ok(out)
}

fn append(files: &[PathBuf], project_dir: &Path, first_number: usize) -> Result<Vec<Page>, String> {
    let pages_dir = project_dir.join("pages");
    fs::create_dir_all(&pages_dir).map_err(|e| e.to_string())?;
    let mut files = files.to_vec();
    files.sort_by(|a, b| natural_cmp(&a.to_string_lossy(), &b.to_string_lossy()));
    let mut pages = Vec::new();
    for (i, src) in files.iter().enumerate() {
        let mut number = first_number + i;
        let ext = extension(src);
        let mut file = format!("{number:04}.{ext}");
        while pages_dir.join(&file).exists() {
            number += 1000;
            file = format!("{number:04}.{ext}");
        }
        let mut r = fs::File::open(src).map_err(|e| e.to_string())?;
        write_page(&mut r, &pages_dir.join(&file))?;
        let name = src
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        pages.push(page_for(project_dir, format!("pages/{file}"), name));
    }
    Ok(pages)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn os_leftovers_are_not_pages() {
        assert!(is_metadata("__MACOSX/a/._p1.jpg"));
        assert!(is_metadata("vol/Thumbs.db"));
        assert!(is_metadata("vol/._p1.jpg"));
        assert!(!is_metadata("vol/p1.jpg"));
        assert!(is_picture_name("a/B.JPG") && is_picture_name("x.webp") && !is_picture_name("x.txt"));
    }
}
