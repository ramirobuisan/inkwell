//! Background picture loading: a few worker threads (half the cores, so the laptop stays
//! cool and responsive) decode pages and thumbnails. Full pages go before thumbnails.

use std::{
    collections::VecDeque,
    path::PathBuf,
    sync::{
        mpsc::{channel, Receiver, Sender},
        Arc, Condvar, Mutex,
    },
    thread,
};

use eframe::egui;
use image::imageops::FilterType;

pub const THUMB: u32 = 160;

pub enum Job {
    Page {
        gen: u64,
        index: usize,
        path: PathBuf,
        cap: u32,
    },
    Thumb {
        gen: u64,
        index: usize,
        path: PathBuf,
        cache: PathBuf,
    },
}

pub enum Done {
    Page {
        gen: u64,
        index: usize,
        size: [usize; 2],
        rgba: Vec<u8>,
    },
    Thumb {
        gen: u64,
        index: usize,
        size: [usize; 2],
        rgba: Vec<u8>,
    },
    Failed {
        gen: u64,
        index: usize,
        error: String,
    },
}

#[derive(Default)]
struct Queue {
    pages: VecDeque<Job>,
    thumbs: VecDeque<Job>,
}

pub struct Pool {
    queue: Arc<(Mutex<Queue>, Condvar)>,
    pub rx: Receiver<Done>,
}

impl Pool {
    pub fn new(ctx: egui::Context, workers: usize) -> Self {
        let queue: Arc<(Mutex<Queue>, Condvar)> = Arc::default();
        let (tx, rx) = channel();
        for n in 0..workers.max(1) {
            let queue = queue.clone();
            let tx: Sender<Done> = tx.clone();
            let ctx = ctx.clone();
            thread::Builder::new()
                .name(format!("pictures-{n}"))
                .spawn(move || loop {
                    let job = {
                        let (lock, cv) = &*queue;
                        let mut q = lock.lock().expect("queue");
                        loop {
                            if let Some(j) = q.pages.pop_front().or_else(|| q.thumbs.pop_front()) {
                                break j;
                            }
                            q = cv.wait(q).expect("queue");
                        }
                    };
                    let done = run(job);
                    if tx.send(done).is_err() {
                        return;
                    }
                    ctx.request_repaint();
                })
                .expect("start a picture worker");
        }
        Self { queue, rx }
    }

    pub fn push(&self, job: Job) {
        let (lock, cv) = &*self.queue;
        let mut q = lock.lock().expect("queue");
        match job {
            Job::Page { .. } => q.pages.push_back(job),
            Job::Thumb { .. } => q.thumbs.push_back(job),
        }
        cv.notify_one();
    }

    /// Drops queued work of an older project.
    pub fn clear(&self) {
        let (lock, _) = &*self.queue;
        let mut q = lock.lock().expect("queue");
        q.pages.clear();
        q.thumbs.clear();
    }
}

fn run(job: Job) -> Done {
    match job {
        Job::Page {
            gen,
            index,
            path,
            cap,
        } => match image::open(&path) {
            Ok(img) => {
                let img = if img.width().max(img.height()) > cap {
                    img.resize(cap, cap, FilterType::Triangle)
                } else {
                    img
                };
                let rgba = img.to_rgba8();
                Done::Page {
                    gen,
                    index,
                    size: [rgba.width() as usize, rgba.height() as usize],
                    rgba: rgba.into_raw(),
                }
            }
            Err(e) => Done::Failed {
                gen,
                index,
                error: format!("{}: {e}", path.display()),
            },
        },
        Job::Thumb {
            gen,
            index,
            path,
            cache,
        } => {
            let img = image::open(&cache).ok().or_else(|| {
                let full = image::open(&path).ok()?;
                let t = full.thumbnail(THUMB, THUMB);
                if let Some(dir) = cache.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                let _ = t.to_rgb8().save(&cache);
                Some(t)
            });
            match img {
                Some(t) => {
                    let rgba = t.to_rgba8();
                    Done::Thumb {
                        gen,
                        index,
                        size: [rgba.width() as usize, rgba.height() as usize],
                        rgba: rgba.into_raw(),
                    }
                }
                None => Done::Failed {
                    gen,
                    index,
                    error: format!("can't read {}", path.display()),
                },
            }
        }
    }
}
