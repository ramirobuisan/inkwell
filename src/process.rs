//! Runs the chosen steps over pages in the background and sends the results back to the
//! window, which applies them to the project (and saves) as they arrive.

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
    Error(String),
    Finished {
        cancelled: bool,
    },
}

pub struct Running {
    pub rx: Receiver<Msg>,
    pub cancel: Arc<AtomicBool>,
    pub done: usize,
    pub total: usize,
    pub label: String,
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
            let mut history: Vec<(String, String)> = Vec::new();
            for (n, job) in jobs.into_iter().enumerate() {
                if stop.load(Ordering::Relaxed) {
                    send(Msg::Finished { cancelled: true });
                    return;
                }
                send(Msg::Progress {
                    done: n,
                    total,
                    label: format!("Page {} of {total}", n + 1),
                });
                let mut blocks = job.blocks;
                let needs_picture = steps.clean
                    || (steps.find && blocks.is_empty())
                    || (steps.read
                        && blocks
                            .iter()
                            .any(|b| force_read || b.source.trim().is_empty()));
                let picture = if needs_picture {
                    match image::open(&job.path) {
                        Ok(p) => Some(p.to_rgba8()),
                        Err(e) => {
                            send(Msg::Error(format!("{}: {e}", job.path.display())));
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
                                if !found.is_empty() {
                                    send(Msg::Found {
                                        index: job.index,
                                        blocks: found.clone(),
                                    });
                                }
                                blocks = found;
                            }
                            Err(e) => {
                                send(Msg::Error(e));
                                send(Msg::Finished { cancelled: false });
                                return;
                            }
                        }
                    }
                }
                if steps.read {
                    if let Some(p) = &picture {
                        for b in blocks.iter_mut() {
                            if !force_read && !b.source.trim().is_empty() {
                                continue;
                            }
                            if stop.load(Ordering::Relaxed) {
                                break;
                            }
                            match ai::read_box(p, b, &settings.ocr_lang, &langs) {
                                Ok(text) => {
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
                                    send(Msg::Error(e));
                                    send(Msg::Finished { cancelled: false });
                                    return;
                                }
                            }
                        }
                    }
                }
                if steps.clean {
                    if let Some(p) = &picture {
                        for b in &blocks {
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
                if steps.translate {
                    let todo: Vec<&Block> = blocks
                        .iter()
                        .filter(|b| !b.source.trim().is_empty())
                        .collect();
                    if !todo.is_empty() {
                        let texts: Vec<String> = todo.iter().map(|b| b.source.clone()).collect();
                        let ctx_pages = &history[history.len().saturating_sub(2)..];
                        match ai::translate(&settings, &texts, ctx_pages) {
                            Ok((out, exchange)) => {
                                for (b, t) in todo.iter().zip(out) {
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
                                send(Msg::Error(e));
                                send(Msg::Finished { cancelled: false });
                                return;
                            }
                        }
                    }
                }
            }
            send(Msg::Progress {
                done: total,
                total,
                label: "Done".into(),
            });
            send(Msg::Finished { cancelled: false });
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
