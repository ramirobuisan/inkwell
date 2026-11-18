//! Runs the chosen steps over pages in the background and sends the results back to the
//! window, which applies them to the project (and saves) as they arrive. Before a step
//! starts, its tool is checked (Tesseract's language pack, the translation server), so a
//! missing piece is explained instead of the run ending silently.

use std::{
    path::PathBuf,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc::{channel, Receiver},
        Arc,
    },
    thread,
};

use eframe::egui;

use crate::{ai, project::Block, settings::Settings};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Steps {
    pub find: bool,
    pub read: bool,
    pub translate: bool,
    pub clean: bool,
}

impl Default for Steps {
    fn default() -> Self {
        Self {
            find: true,
            read: true,
            translate: true,
            clean: true,
        }
    }
}

pub struct PageJob {
    pub index: usize,
    pub path: PathBuf,
    pub blocks: Vec<Block>,
}

pub enum Msg {
    Progress {
        done: usize,
        total: usize,
        label: String,
    },
    /// Boxes found on a page that had none.
    Found {
        index: usize,
        blocks: Vec<Block>,
    },
    /// A box's new texts or cover colour.
    Update {
        index: usize,
        id: u64,
        source: Option<String>,
        translation: Option<String>,
        cover: Option<[u8; 4]>,
    },
    /// A problem the person should read (shown in a window, not a passing note).
    Error(String),
    Finished {
        cancelled: bool,
        /// What the run did, in plain words.
        summary: String,
    },
}

pub struct Running {
    pub rx: Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
    pub done: usize,
    pub total: usize,
    pub label: String,
}

#[derive(Default)]
struct Tally {
    found: usize,
    read: usize,
    cleaned: usize,
    translated: usize,
    boxes: usize,
    with_text: usize,
}

fn summary(t: &Tally, pages: usize, steps: Steps, had_problems: bool) -> String {
    let where_ = if pages == 1 {
        "this page".to_owned()
    } else {
        format!("{pages} pages")
    };
    let mut did = Vec::new();
    if t.found > 0 {
        did.push(format!("found {} text area(s)", t.found));
    }
    if t.read > 0 {
        did.push(format!("read {} box(es)", t.read));
    }
    if t.cleaned > 0 {
        did.push(format!("cleaned {} box(es)", t.cleaned));
    }
    if t.translated > 0 {
        did.push(format!("translated {} box(es)", t.translated));
    }
    let mut s = if did.is_empty() {
        format!("Nothing was done on {where_}.")
    } else {
        format!("On {where_}: {}.", did.join(", "))
    };
    if did.is_empty() && !had_problems {
        if t.boxes == 0 {
            s.push_str(if steps.find {
                " No text was found. Draw text boxes with the Text tool (T), then press Run again."
            } else {
                " There are no text boxes yet. Draw them with the Text tool (T), or turn on \"Find text\" in the Translate tab."
            });
        } else if t.with_text == 0 {
            s.push_str(" The boxes have no original text yet: type it in, or turn on \"Read the original text\".");
        } else {
            s.push_str(" Turn on the steps you want in the Translate tab.");
        }
    }
    s
}

pub fn start(
    ctx: egui::Context,
    settings: Settings,
    langs: Vec<String>,
    steps: Steps,
    jobs: Vec<PageJob>,
    mut next_id: u64,
    force_read: bool,
) -> Running {
    let (tx, rx) = channel();
    let cancel = Arc::new(AtomicBool::new(false));
    let stop = cancel.clone();
    let total = jobs.len();
    thread::Builder::new()
        .name("process".into())
        .spawn(move || {
            let send = |m: Msg| {
                let _ = tx.send(m);
                ctx.request_repaint();
            };
            let asked = steps;
            let mut steps = steps;
            let mut problems: Vec<String> = Vec::new();
            // Reading text needs Tesseract and the chosen language pack.
            if (steps.find || steps.read) && !langs.contains(&settings.ocr_lang) {
                problems.push(ai::missing_language_help(&settings.ocr_lang, langs.is_empty()));
                steps.find = false;
                steps.read = false;
            }
            let mut tally = Tally::default();
            let mut server_checked = false;
            let mut history: Vec<(String, String)> = Vec::new();
            let mut cancelled = false;
            'pages: for (n, job) in jobs.into_iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    cancelled = true;
                    break;
                }
                send(Msg::Progress {
                    done: n,
                    total,
                    label: format!("Page {} of {total}", n + 1),
                });
                let mut blocks = job.blocks;
                let needs_picture = (steps.clean && !blocks.is_empty())
                    || (steps.find && blocks.is_empty())
                    || (steps.read
                        && blocks
                            .iter()
                            .any(|b| force_read || b.source.trim().is_empty()));
                let picture = if needs_picture {
                    match image::open(&job.path) {
                        Ok(p) => Some(p.to_rgba8()),
                        Err(e) => {
                            problems.push(format!("Couldn't open page {}: {e}", job.index + 1));
                            continue;
                        }
                    }
                } else {
                    None
                };
                if steps.find && blocks.is_empty() {
                    if let Some(p) = &picture {
                        match ai::find_text(p, &settings.ocr_lang) {
                            Ok(mut found) => {
                                for b in &mut found {
                                    b.id = next_id;
                                    next_id += 1;
                                }
                                tally.found += found.len();
                                if !found.is_empty() {
                                    send(Msg::Found {
                                        index: job.index,
                                        blocks: found.clone(),
                                    });
                                }
                                blocks = found;
                            }
                            Err(e) => {
                                problems.push(e);
                                steps.find = false;
                                steps.read = false;
                            }
                        }
                    }
                }
                tally.boxes += blocks.len();
                if steps.read {
                    if let Some(p) = &picture {
                        for b in blocks.iter_mut() {
                            if !force_read && !b.source.trim().is_empty() {
                                continue;
                            }
                            if stop.load(Ordering::Relaxed) {
                                cancelled = true;
                                break 'pages;
                            }
                            match ai::read_box(p, b, &settings.ocr_lang, &langs) {
                                Ok(text) => {
                                    if !text.trim().is_empty() {
                                        tally.read += 1;
                                    }
                                    b.source = text.clone();
                                    send(Msg::Update {
                                        index: job.index,
                                        id: b.id,
                                        source: Some(text),
                                        translation: None,
                                        cover: None,
                                    });
                                }
                                Err(e) => {
                                    problems.push(e);
                                    steps.read = false;
                                    break;
                                }
                            }
                        }
                    }
                }
                if steps.clean {
                    if let Some(p) = &picture {
                        for b in &blocks {
                            tally.cleaned += 1;
                            send(Msg::Update {
                                index: job.index,
                                id: b.id,
                                source: None,
                                translation: None,
                                cover: Some(ai::edge_color(p, b)),
                            });
                        }
                    }
                }
                let todo: Vec<&Block> = blocks
                    .iter()
                    .filter(|b| !b.source.trim().is_empty())
                    .collect();
                tally.with_text += todo.len();
                if steps.translate && !todo.is_empty() {
                    if !server_checked {
                        server_checked = true;
                        let prepared = ai::prepare_server(&settings, |label, _| {
                            send(Msg::Progress { done: n, total, label })
                        });
                        if let Err(e) = prepared {
                            problems.push(e);
                            steps.translate = false;
                            continue;
                        }
                    }
                    send(Msg::Progress {
                        done: n,
                        total,
                        label: format!("Translating page {} of {total}", n + 1),
                    });
                    let texts: Vec<String> = todo.iter().map(|b| b.source.clone()).collect();
                    let ctx_pages = &history[history.len().saturating_sub(2)..];
                    match ai::translate(&settings, &texts, ctx_pages) {
                        Ok((out, exchange)) => {
                            for (b, t) in todo.iter().zip(out) {
                                if !t.trim().is_empty() {
                                    tally.translated += 1;
                                }
                                send(Msg::Update {
                                    index: job.index,
                                    id: b.id,
                                    source: None,
                                    translation: Some(t),
                                    cover: None,
                                });
                            }
                            history.push(exchange);
                        }
                        Err(e) => {
                            problems.push(e);
                            steps.translate = false;
                        }
                    }
                }
            }
            if !problems.is_empty() {
                send(Msg::Error(problems.join("\n\n")));
            }
            send(Msg::Progress {
                done: total,
                total,
                label: "Done".into(),
            });
            send(Msg::Finished {
                cancelled,
                summary: summary(&tally, total, asked, !problems.is_empty()),
            });
        })
        .expect("start processing");
    Running {
        rx,
        cancel,
        done: 0,
        total,
        label: "Starting…".into(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn collect(r: Running) -> (Vec<String>, Option<String>) {
        let mut errors = Vec::new();
        let mut summary = None;
        for m in r.rx.iter() {
            match m {
                Msg::Error(e) => errors.push(e),
                Msg::Finished { summary: s, .. } => {
                    summary = Some(s);
                    break;
                }
                _ => {}
            }
        }
        (errors, summary)
    }

    #[test]
    fn a_run_that_cannot_work_says_why() {
        let dir = std::env::temp_dir().join(format!("inkwell-run-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let page = dir.join("p.png");
        image::RgbaImage::from_pixel(200, 200, image::Rgba([255, 255, 255, 255])).save(&page).unwrap();
        let settings = Settings {
            ocr_lang: "xyz_missing".into(),
            endpoint: "http://127.0.0.1:9/v1".into(),
            ..Settings::default()
        };
        // 1) Missing language pack, page without boxes.
        let r = start(egui::Context::default(), settings.clone(), vec!["eng".into()], Steps::default(),
            vec![PageJob { index: 0, path: page.clone(), blocks: vec![] }], 1, false);
        let (errors, summary) = collect(r);
        assert!(errors.iter().any(|e| e.contains("language pack")), "{errors:?}");
        assert!(summary.unwrap().contains("Nothing was done"));
        // 2) Boxes with text, no translation server.
        let b = Block { id: 7, x: 10.0, y: 10.0, w: 50.0, h: 50.0, source: "hello".into(), ..Block::default() };
        let steps = Steps { find: false, read: false, translate: true, clean: false };
        let r = start(egui::Context::default(), settings, vec!["eng".into()], steps,
            vec![PageJob { index: 0, path: page, blocks: vec![b] }], 1, false);
        let (errors, _) = collect(r);
        assert!(errors.iter().any(|e| e.contains("No translation server")), "{errors:?}");
        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[cfg(test)]
mod missing_pack_tests {
    use super::*;

    /// With only English installed and Japanese chosen, a run reports how to install the
    /// Japanese pack instead of Tesseract's raw "Failed loading language" error.
    #[test]
    fn missing_language_pack_gives_install_help() {
        let settings = Settings {
            ocr_lang: "jpn".into(),
            ..Settings::default()
        };
        let steps = Steps { find: true, read: true, translate: false, clean: false };
        let job = PageJob {
            index: 0,
            path: std::env::temp_dir().join("inkwell-no-such-page.png"),
            blocks: Vec::new(),
        };
        let run = start(egui::Context::default(), settings, vec!["eng".into()], steps, vec![job], 1, false);
        let mut error = None;
        while let Ok(m) = run.rx.recv_timeout(std::time::Duration::from_secs(20)) {
            match m {
                Msg::Error(e) => error = Some(e),
                Msg::Finished { .. } => break,
                _ => {}
            }
        }
        let e = error.expect("a problem is reported");
        assert!(e.contains("tesseract-data-jpn"), "{e}");
        assert!(!e.contains("TESSDATA_PREFIX"), "{e}");
    }
}
