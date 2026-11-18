//! The window: start view with the drop zone and library, and the editor with the pages
//! list, the canvas and the properties panel. Every edit marks the project dirty; it is
//! written to disk shortly after (and on page change, on close, and on a crash).

use std::{
    collections::{HashMap, HashSet, VecDeque},
    path::{Path, PathBuf},
    sync::{
        atomic::Ordering,
        mpsc::{channel, Receiver},
        Arc,
    },
    time::{Duration, Instant, SystemTime},
};

use eframe::egui::{
    self, pos2, vec2, Align2, Color32, CornerRadius, FontId, Key, Modifiers, Pos2, Rect, RichText,
    Sense, Stroke, StrokeKind, Vec2,
};

use crate::{
    ai,
    export::{self, Exporting},
    import::{self, Importer},
    process::{self, PageJob, Running, Steps},
    project::{self, Align, Block, CoverShape, LibraryEntry, Project},
    session::{self, Session},
    settings::{Machine, Settings},
    textfit::{self, FontStore},
    work::{Done, Job, Pool},
};

// ---------- theme ----------
// Light colours from the original's light theme, dark ones from its `.dark` block
// (packages/ui/src/styles: background oklch(.145), card/sidebar oklch(.205),
// secondary oklch(.269), muted text oklch(.708), primary rose-400, accent rose-950).

static DARK: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn dark() -> bool {
    DARK.load(Ordering::Relaxed)
}

macro_rules! themed {
    ($($name:ident = $light:expr, $dark:expr;)*) => {
        $(fn $name() -> Color32 { if dark() { $dark } else { $light } })*
    };
}

const fn rgb(hex: u32) -> Color32 {
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

themed! {
    rose = rgb(0xf43f5e), rgb(0xfb7185);
    rose_soft = rgb(0xffe4e6), rgb(0x4c0519);
    rose_mid = rgb(0xfecdd3), rgb(0x881337);
    on_rose = Color32::WHITE, rgb(0x0a0a0a);
    surface = Color32::WHITE, rgb(0x0a0a0a);
    sidebar = rgb(0xfafafa), rgb(0x171717);
    border = rgb(0xe5e5e5), rgb(0x2a2a2a);
    text_c = rgb(0x252525), rgb(0xfafafa);
    muted = rgb(0x737373), rgb(0xa1a1a1);
    canvas_c = rgb(0xf4f4f5), rgb(0x18181b);
    chip = rgb(0xf5f5f5), rgb(0x262626);
    hover_c = rgb(0xf3f3f3), rgb(0x222222);
    placeholder = rgb(0xececec), rgb(0x2a2a2a);
    menubar = rgb(0xfff5f6), rgb(0x1a0f12);
    banner_bg = rgb(0xfff7ed), rgb(0x431407);
    banner_line = rgb(0xfdba74), rgb(0x9a3412);
    banner_text = rgb(0x9a3412), rgb(0xfed7aa);
    err_bg = rgb(0xfef2f2), rgb(0x450a0a);
    err_text = rgb(0xb91c1c), rgb(0xfecaca);
    ok_text = rgb(0x16a34a), rgb(0x4ade80);
}

/// A translucent rose (selection fills, drop overlay).
fn rose_a(alpha: u8) -> Color32 {
    let c = rose();
    Color32::from_rgba_unmultiplied(c.r(), c.g(), c.b(), alpha)
}

const ROW_H: f32 = 84.0;
/// History steps kept before the oldest is dropped: koharu-scene session.rs:18 (`STEPS`).
const UNDO_STEPS: usize = 20;

const PAGE_TEX: egui::TextureOptions = egui::TextureOptions {
    magnification: egui::TextureFilter::Linear,
    minification: egui::TextureFilter::Linear,
    wrap_mode: egui::TextureWrapMode::ClampToEdge,
    mipmap_mode: Some(egui::TextureFilter::Linear),
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tool {
    Select,
    Text,
    Pan,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Tab {
    Properties,
    Translate,
}

enum Drag {
    Create { start: Pos2 },
    Move { id: u64, grab: Vec2 },
    Resize { id: u64 },
    Pan,
}

enum ImportState {
    Running,
    Done,
    Failed(String),
}

struct ImportRow {
    id: u64,
    name: String,
    done: usize,
    total: usize,
    state: ImportState,
}

struct Open {
    dir: PathBuf,
    data: Project,
    gen: u64,
    pages: HashMap<usize, egui::TextureHandle>,
    lru: VecDeque<usize>,
    page_pending: HashSet<usize>,
    failed: HashMap<usize, String>,
    thumbs: HashMap<usize, egui::TextureHandle>,
    thumb_pending: HashSet<usize>,
    block_tex: HashMap<u64, (u64, egui::TextureHandle)>,
    selected: Option<u64>,
    zoom: f32,
    pan: Vec2,
    fit: bool,
    undo: Vec<(usize, Vec<Block>)>,
    redo: Vec<(usize, Vec<Block>)>,
    coalesce: Option<(u64, u8)>,
    dirty: bool,
    last_change: Instant,
    saved_at: Option<Instant>,
    save_error: Option<String>,
    drag: Option<Drag>,
    focus_translation: bool,
    scroll_list: bool,
}

impl Open {
    fn new(dir: PathBuf, data: Project, gen: u64) -> Self {
        Self {
            dir,
            data,
            gen,
            pages: HashMap::new(),
            lru: VecDeque::new(),
            page_pending: HashSet::new(),
            failed: HashMap::new(),
            thumbs: HashMap::new(),
            thumb_pending: HashSet::new(),
            block_tex: HashMap::new(),
            selected: None,
            zoom: 1.0,
            pan: Vec2::ZERO,
            fit: true,
            undo: Vec::new(),
            redo: Vec::new(),
            coalesce: None,
            dirty: false,
            last_change: Instant::now(),
            saved_at: None,
            save_error: None,
            drag: None,
            focus_translation: false,
            scroll_list: true,
        }
    }

    fn block(&self, id: u64) -> Option<&Block> {
        self.data
            .pages
            .get(self.data.current)?
            .blocks
            .iter()
            .find(|b| b.id == id)
    }

    fn block_mut(&mut self, id: u64) -> Option<&mut Block> {
        let cur = self.data.current;
        self.data
            .pages
            .get_mut(cur)?
            .blocks
            .iter_mut()
            .find(|b| b.id == id)
    }

    /// One undo step for page `i`; a run of edits to the same field of the same box
    /// counts as one.
    fn snapshot_page(&mut self, i: usize, key: Option<(u64, u8)>) {
        if key.is_some() && key == self.coalesce {
            return;
        }
        self.coalesce = key;
        if let Some(p) = self.data.pages.get(i) {
            self.undo.push((i, p.blocks.clone()));
            if self.undo.len() > UNDO_STEPS {
                self.undo.remove(0);
            }
            self.redo.clear();
        }
    }

    fn snapshot(&mut self, key: Option<(u64, u8)>) {
        self.snapshot_page(self.data.current, key);
    }

    /// Marks the project changed and hands its state to the crash hook.
    fn changed(&mut self) {
        self.dirty = true;
        self.last_change = Instant::now();
        session::set_pending(&self.dir, project::to_json(&self.data));
    }

    fn step(&mut self, back: bool) {
        let (from, to) = if back {
            (&mut self.undo, &mut self.redo)
        } else {
            (&mut self.redo, &mut self.undo)
        };
        if let Some((i, blocks)) = from.pop() {
            if let Some(p) = self.data.pages.get_mut(i) {
                let now = std::mem::replace(&mut p.blocks, blocks);
                to.push((i, now));
                if self.data.current != i {
                    self.data.current = i;
                    self.fit = true;
                    self.block_tex.clear();
                }
                self.coalesce = None;
                self.changed();
            }
        }
    }

    fn save(&mut self) {
        if !self.dirty {
            return;
        }
        let json = project::to_json(&self.data);
        match project::save_json(&self.dir, &json) {
            Ok(()) => {
                self.dirty = false;
                self.saved_at = Some(Instant::now());
                self.save_error = None;
                session::clear_pending();
            }
            Err(e) => self.save_error = Some(e.to_string()),
        }
    }
}

pub struct App {
    settings: Settings,
    machine: Machine,
    session: Session,
    fonts: FontStore,
    pool: Pool,
    importer: Importer,
    imports: Vec<ImportRow>,
    open: Option<Open>,
    gen: u64,
    library: Vec<LibraryEntry>,
    library_stale: bool,
    banner: Option<String>,
    toasts: Vec<(String, bool, Instant)>,
    tool: Tool,
    tab: Tab,
    steps: Steps,
    all_pages: bool,
    running: Option<Running>,
    touched: HashSet<usize>,
    exporting: Option<Exporting>,
    last_export: Option<PathBuf>,
    filter: String,
    font_filter: String,
    langs: Option<Vec<String>>,
    confirm_delete: Option<PathBuf>,
    settings_open: bool,
    about_open: bool,
    screenshot: Option<PathBuf>,
    frames: u64,
    shot_sent: bool,
    picker: Option<Receiver<Vec<PathBuf>>>,
    folder_picker: Option<Receiver<PathBuf>>,
    open_after_import: bool,
    applied_dark: Option<bool>,
    run_problem: Option<String>,
}

fn setup_style(ctx: &egui::Context, fonts: &FontStore) {
    let mut defs = egui::FontDefinitions::default();
    if let Some(p) = &fonts.cjk_path {
        if let Ok(bytes) = std::fs::read(p) {
            defs.font_data
                .insert("cjk".into(), Arc::new(egui::FontData::from_owned(bytes)));
            for fam in [egui::FontFamily::Proportional, egui::FontFamily::Monospace] {
                defs.families.entry(fam).or_default().push("cjk".into());
            }
        }
    }
    ctx.set_fonts(defs);
    ctx.all_styles_mut(|style| {
        style.spacing.item_spacing = vec2(8.0, 6.0);
        style.spacing.button_padding = vec2(10.0, 5.0);
        style.spacing.interact_size.y = 26.0;
        use egui::TextStyle::*;
        style.text_styles.insert(Body, FontId::proportional(14.0));
        style.text_styles.insert(Button, FontId::proportional(14.0));
        style.text_styles.insert(Heading, FontId::proportional(20.0));
        style.text_styles.insert(Small, FontId::proportional(11.5));
    });
}

/// Switches every colour of the window to the light or the dark theme.
fn apply_theme(ctx: &egui::Context, is_dark: bool) {
    DARK.store(is_dark, Ordering::Relaxed);
    let theme = if is_dark { egui::Theme::Dark } else { egui::Theme::Light };
    ctx.set_theme(theme);
    let mut v = if is_dark { egui::Visuals::dark() } else { egui::Visuals::light() };
    v.panel_fill = surface();
    v.window_fill = surface();
    v.extreme_bg_color = sidebar();
    v.faint_bg_color = sidebar();
    v.override_text_color = Some(text_c());
    v.hyperlink_color = rose();
    v.selection.bg_fill = rose_mid();
    v.selection.stroke = Stroke::new(1.0_f32, rose());
    v.window_corner_radius = CornerRadius::same(12);
    v.window_stroke = Stroke::new(1.0_f32, border());
    for w in [
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
        &mut v.widgets.noninteractive,
    ] {
        w.corner_radius = CornerRadius::same(8);
    }
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, border());
    v.widgets.inactive.weak_bg_fill = chip();
    v.widgets.inactive.bg_fill = hover_c();
    v.widgets.inactive.fg_stroke = Stroke::new(1.0_f32, text_c());
    v.widgets.noninteractive.fg_stroke = Stroke::new(1.0_f32, text_c());
    v.widgets.hovered.fg_stroke = Stroke::new(1.5_f32, text_c());
    v.widgets.active.fg_stroke = Stroke::new(1.5_f32, text_c());
    v.widgets.hovered.weak_bg_fill = rose_soft();
    v.widgets.hovered.bg_fill = rose_soft();
    v.widgets.hovered.bg_stroke = Stroke::new(1.0_f32, rose_mid());
    v.widgets.active.weak_bg_fill = rose_mid();
    v.widgets.active.bg_fill = rose_mid();
    v.widgets.active.bg_stroke = Stroke::new(1.0_f32, rose());
    v.widgets.open.weak_bg_fill = rose_soft();
    ctx.set_visuals_of(theme, v);
}

fn ago(t: Option<SystemTime>) -> String {
    let Some(t) = t else { return String::new() };
    let s = SystemTime::now()
        .duration_since(t)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    match s {
        0..=59 => "just now".into(),
        60..=3599 => format!("{} min ago", s / 60),
        3600..=86_399 => format!("{} h ago", s / 3600),
        _ => format!("{} days ago", s / 86_400),
    }
}

fn open_in_files(path: &Path) {
    let _ = std::process::Command::new("xdg-open").arg(path).spawn();
}

fn rose_button(text: &str) -> egui::Button<'static> {
    egui::Button::new(
        RichText::new(text.to_owned())
            .color(on_rose())
            .strong(),
    )
    .fill(rose())
    .corner_radius(CornerRadius::same(10))
}

fn section(ui: &mut egui::Ui, icon: &str, title: &str, count: Option<usize>) {
    ui.add_space(6.0);
    ui.horizontal(|ui| {
        ui.label(RichText::new(icon).color(rose()).size(16.0));
        ui.label(RichText::new(title).size(15.0).strong());
        if let Some(n) = count {
            ui.label(RichText::new(n.to_string()).color(muted()));
        }
    });
    ui.add_space(2.0);
}

fn caps(ui: &mut egui::Ui, text: &str) {
    ui.label(RichText::new(text).size(11.0).color(muted()).strong());
}

fn dashed_rect(painter: &egui::Painter, r: Rect, stroke: Stroke, dash: f32, gap: f32) {
    let pts = [
        r.left_top(),
        r.right_top(),
        r.right_bottom(),
        r.left_bottom(),
        r.left_top(),
    ];
    painter.extend(egui::Shape::dashed_line(&pts, stroke, dash, gap));
}

impl App {
    pub fn new(
        cc: &eframe::CreationContext<'_>,
        screenshot: Option<PathBuf>,
        files: Vec<PathBuf>,
    ) -> Self {
        let ctx = cc.egui_ctx.clone();
        let settings = Settings::load();
        let machine = Machine::detect();
        let fonts = FontStore::new(&settings.default_font);
        setup_style(&ctx, &fonts);
        let mut session = Session::load();
        let crashed = !session.clean_exit;
        if screenshot.is_none() {
            session.clean_exit = false;
            session.save();
        }
        let _ = std::fs::create_dir_all(&settings.library);
        let mut importer = Importer::new(ctx.clone(), settings.library.clone());
        let resumed = importer.resume_interrupted();
        let mut app = Self {
            pool: Pool::new(ctx.clone(), settings.workers),
            library: project::list_library(&settings.library),
            settings,
            machine,
            session,
            fonts,
            importer,
            imports: Vec::new(),
            open: None,
            gen: 0,
            library_stale: false,
            banner: None,
            toasts: Vec::new(),
            tool: Tool::Select,
            tab: Tab::Properties,
            steps: Steps::default(),
            all_pages: false,
            running: None,
            touched: HashSet::new(),
            exporting: None,
            last_export: None,
            filter: String::new(),
            font_filter: String::new(),
            langs: ai::tesseract_langs(),
            confirm_delete: None,
            settings_open: false,
            about_open: false,
            screenshot,
            frames: 0,
            shot_sent: false,
            picker: None,
            folder_picker: None,
            open_after_import: false,
            applied_dark: None,
            run_problem: None,
        };
        let mut notes = Vec::new();
        if crashed {
            if let Some(dir) = app.session.last_project.clone().filter(|d| d.exists()) {
                let page = app.session.page;
                app.open_project(dir, Some(page));
                notes.push(format!(
                    "Inkwell closed unexpectedly last time. Your work was restored and page {} reopened.",
                    page + 1
                ));
            } else {
                notes.push("Inkwell closed unexpectedly last time. Nothing was lost.".into());
            }
        }
        if resumed > 0 {
            notes.push(format!(
                "Finishing {resumed} import(s) that were interrupted."
            ));
            app.open_after_import = app.open.is_none();
        }
        if !notes.is_empty() {
            let text = notes.join(" ");
            if app.open.is_some() {
                app.toast(text.clone(), false);
            }
            app.banner = Some(text);
        }
        if !files.is_empty() {
            app.take_paths(files);
        }
        if app.screenshot.is_none() {
            if let Ok(Some(_)) = crate::launcher::install(false) {
                app.toast("Inkwell is now in your app launcher: search for “Inkwell” to start it.", false);
            }
        }
        app
    }

    fn toast(&mut self, text: impl Into<String>, error: bool) {
        self.toasts.push((text.into(), error, Instant::now()));
    }

    fn save_session(&mut self) {
        if self.screenshot.is_some() {
            return;
        }
        self.session.last_project = self.open.as_ref().map(|o| o.dir.clone());
        self.session.page = self.open.as_ref().map(|o| o.data.current).unwrap_or(0);
        self.session.save();
    }

    fn open_project(&mut self, dir: PathBuf, page: Option<usize>) {
        if let Some(o) = &mut self.open {
            o.save();
        }
        match project::load(&dir) {
            Ok((mut data, note)) => {
                if let Some(n) = note {
                    self.toast(n, false);
                }
                if let Some(p) = page {
                    data.current = p;
                }
                data.current = data.current.min(data.pages.len().saturating_sub(1));
                self.gen += 1;
                self.pool.clear();
                self.open = Some(Open::new(dir, data, self.gen));
                self.filter.clear();
                self.save_session();
            }
            Err(e) => self.toast(format!("Can't open that comic: {e}"), true),
        }
    }

    fn close_project(&mut self) {
        if let Some(r) = &self.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
        self.running = None;
        if let Some(mut o) = self.open.take() {
            o.save();
        }
        self.pool.clear();
        self.library = project::list_library(&self.settings.library);
        self.save_session();
    }

    fn go_to(&mut self, index: usize) {
        let Some(o) = &mut self.open else { return };
        if index >= o.data.pages.len() || index == o.data.current {
            return;
        }
        o.data.current = index;
        o.selected = None;
        o.fit = true;
        o.block_tex.clear();
        o.coalesce = None;
        o.scroll_list = true;
        o.dirty = true;
        o.save();
        self.save_session();
    }

    /// Files dropped on the window or picked: comics become new folders, pictures join
    /// the open comic.
    fn take_paths(&mut self, paths: Vec<PathBuf>) {
        let mut pictures = Vec::new();
        let mut skipped = Vec::new();
        for p in paths {
            if p.is_dir() || import::is_archive(&p) {
                let id = self.importer.send(import::Request::New {
                    source: p.clone(),
                    resume: None,
                });
                self.imports.push(ImportRow {
                    id,
                    name: p
                        .file_stem()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                    done: 0,
                    total: 0,
                    state: ImportState::Running,
                });
                // Every dropped comic becomes a new project; the first one opens.
                self.open_after_import = true;
            } else if import::is_picture_name(&p.to_string_lossy()) {
                pictures.push(p);
            } else {
                skipped.push(
                    p.file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default(),
                );
            }
        }
        if !pictures.is_empty() {
            match &self.open {
                Some(o) => {
                    let n = pictures.len();
                    self.importer.send(import::Request::Append {
                        files: pictures,
                        project: o.dir.clone(),
                        first_number: o.data.pages.len() + 1,
                    });
                    self.toast(format!("Adding {n} picture(s) as new pages…"), false);
                }
                None => self.toast(
                    "Open a comic first to add loose pictures, or drop a whole folder of pictures.",
                    true,
                ),
            }
        }
        if !skipped.is_empty() {
            self.toast(
                format!(
                    "Skipped {} (only CBZ, ZIP, folders and PNG/JPEG/WebP pictures are supported).",
                    skipped.join(", ")
                ),
                true,
            );
        }
    }

    fn poll(&mut self, ctx: &egui::Context) {
        // Pictures from the workers.
        while let Ok(done) = self.pool.rx.try_recv() {
            let Some(o) = &mut self.open else { continue };
            match done {
                Done::Page {
                    gen,
                    index,
                    size,
                    rgba,
                } if gen == o.gen => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                    let tex = ctx.load_texture(format!("page-{gen}-{index}"), img, PAGE_TEX);
                    o.pages.insert(index, tex);
                    o.page_pending.remove(&index);
                    o.lru.retain(|&i| i != index);
                    o.lru.push_back(index);
                    let cur = o.data.current;
                    let mut guard = 0;
                    while o.pages.len() > self.settings.page_cache.max(3) && guard < 32 {
                        guard += 1;
                        let Some(old) = o.lru.pop_front() else { break };
                        if old.abs_diff(cur) <= 1 {
                            o.lru.push_back(old);
                            continue;
                        }
                        o.pages.remove(&old);
                    }
                }
                Done::Thumb {
                    gen,
                    index,
                    size,
                    rgba,
                } if gen == o.gen => {
                    let img = egui::ColorImage::from_rgba_unmultiplied(size, &rgba);
                    let tex = ctx.load_texture(
                        format!("thumb-{gen}-{index}"),
                        img,
                        egui::TextureOptions::LINEAR,
                    );
                    o.thumbs.insert(index, tex);
                }
                Done::Failed { gen, index, error } if gen == o.gen => {
                    o.page_pending.remove(&index);
                    o.failed.insert(index, error);
                }
                _ => {}
            }
        }

        // Imports.
        while let Ok(msg) = self.importer.rx.try_recv() {
            match msg {
                import::Msg::Started { id, name } => {
                    if let Some(r) = self.imports.iter_mut().find(|r| r.id == id) {
                        r.name = name;
                    } else {
                        self.imports.push(ImportRow {
                            id,
                            name,
                            done: 0,
                            total: 0,
                            state: ImportState::Running,
                        });
                    }
                }
                import::Msg::Progress { id, done, total } => {
                    if let Some(r) = self.imports.iter_mut().find(|r| r.id == id) {
                        r.done = done;
                        r.total = total;
                    }
                }
                import::Msg::Finished { id, dir } => {
                    let name = dir
                        .file_name()
                        .map(|n| n.to_string_lossy().into_owned())
                        .unwrap_or_default();
                    if let Some(r) = self.imports.iter_mut().find(|r| r.id == id) {
                        r.state = ImportState::Done;
                    }
                    self.library_stale = true;
                    self.toast(format!("New project “{name}” created from the comic."), false);
                    if self.open_after_import {
                        self.open_after_import = false;
                        self.banner = None;
                        self.open_project(dir, None);
                    }
                }
                import::Msg::Appended { project, pages } => {
                    if let Some(o) = &mut self.open {
                        if o.dir == project {
                            let n = pages.len();
                            o.data.pages.extend(pages);
                            o.changed();
                            o.save();
                            self.toast(format!("Added {n} page(s)."), false);
                        }
                    }
                }
                import::Msg::Failed { id, error } => {
                    if let Some(r) = self.imports.iter_mut().find(|r| r.id == id) {
                        r.state = ImportState::Failed(error.clone());
                    }
                    self.library_stale = true;
                    self.toast(error, true);
                }
            }
        }
        if self.library_stale {
            self.library_stale = false;
            self.library = project::list_library(&self.settings.library);
        }

        // Processing results.
        let mut finished = None;
        let mut errors = Vec::new();
        if let (Some(run), Some(o)) = (&mut self.running, &mut self.open) {
            while let Ok(msg) = run.rx.try_recv() {
                match msg {
                    process::Msg::Progress { done, total, label } => {
                        run.done = done;
                        run.total = total;
                        run.label = label;
                    }
                    process::Msg::Found { index, blocks } => {
                        if self.touched.insert(index) {
                            o.snapshot_page(index, None);
                        }
                        if let Some(p) = o.data.pages.get_mut(index) {
                            p.blocks.extend(blocks);
                            o.changed();
                        }
                    }
                    process::Msg::Update {
                        index,
                        id,
                        source,
                        translation,
                        cover,
                    } => {
                        if self.touched.insert(index) {
                            o.snapshot_page(index, None);
                        }
                        if let Some(b) = o
                            .data
                            .pages
                            .get_mut(index)
                            .and_then(|p| p.blocks.iter_mut().find(|b| b.id == id))
                        {
                            if let Some(s) = source {
                                b.source = s;
                            }
                            if let Some(t) = translation {
                                b.translation = t;
                            }
                            if let Some(c) = cover {
                                b.cover = true;
                                b.cover_color = c;
                            }
                            o.changed();
                        }
                    }
                    process::Msg::Error(e) => errors.push(e),
                    process::Msg::Finished { cancelled, summary } => {
                        finished = Some((cancelled, summary))
                    }
                }
            }
        }
        if !errors.is_empty() {
            self.run_problem = Some(errors.join("\n\n"));
        }
        if let Some((cancelled, summary)) = finished {
            self.running = None;
            self.touched.clear();
            if let Some(o) = &mut self.open {
                o.save();
            }
            let text = if cancelled { format!("Stopped. {summary}") } else { summary };
            self.toast(text, false);
        }

        // Export.
        let mut export_done = None;
        if let Some(ex) = &mut self.exporting {
            while let Ok(m) = ex.rx.try_recv() {
                match m {
                    export::Msg::Progress(d, t) => {
                        ex.done = d;
                        ex.total = t;
                    }
                    export::Msg::Done(p) => export_done = Some(Ok(p)),
                    export::Msg::Failed(e) => export_done = Some(Err(e)),
                }
            }
        }
        match export_done {
            Some(Ok(p)) => {
                self.exporting = None;
                self.toast(format!("Exported to {}", p.display()), false);
                self.last_export = Some(p);
            }
            Some(Err(e)) => {
                self.exporting = None;
                self.toast(e, true);
            }
            None => {}
        }

        // File dialogs (run on their own threads so the window keeps drawing).
        if let Some(rx) = &self.picker {
            if let Ok(paths) = rx.try_recv() {
                self.picker = None;
                self.take_paths(paths);
            }
        }
        if let Some(rx) = &self.folder_picker {
            if let Ok(dir) = rx.try_recv() {
                self.folder_picker = None;
                self.settings.library = dir;
                self.apply_library(ctx);
            }
        }
    }

    fn apply_library(&mut self, ctx: &egui::Context) {
        let _ = std::fs::create_dir_all(&self.settings.library);
        self.settings.save();
        if self.importer.library() != self.settings.library {
            self.importer = Importer::new(ctx.clone(), self.settings.library.clone());
            self.importer.resume_interrupted();
        }
        self.library = project::list_library(&self.settings.library);
    }

    fn pick_comics(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            let files = rfd::FileDialog::new()
                .add_filter(
                    "Comics and pictures",
                    &["cbz", "zip", "png", "jpg", "jpeg", "webp"],
                )
                .pick_files()
                .unwrap_or_default();
            let _ = tx.send(files);
        });
        self.picker = Some(rx);
    }

    fn pick_library(&mut self) {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            if let Some(d) = rfd::FileDialog::new().pick_folder() {
                let _ = tx.send(d);
            }
        });
        self.folder_picker = Some(rx);
    }

    fn start_run(
        &mut self,
        ctx: &egui::Context,
        steps: Steps,
        only_block: Option<u64>,
        force_read: bool,
    ) {
        if self.running.is_some() {
            return;
        }
        let Some(o) = &mut self.open else { return };
        let indices: Vec<usize> = if self.all_pages && only_block.is_none() {
            (0..o.data.pages.len()).collect()
        } else {
            vec![o.data.current]
        };
        let jobs: Vec<PageJob> = indices
            .into_iter()
            .map(|i| {
                let p = &o.data.pages[i];
                PageJob {
                    index: i,
                    path: o.dir.join(&p.file),
                    blocks: p
                        .blocks
                        .iter()
                        .filter(|b| only_block.is_none_or(|id| b.id == id))
                        .cloned()
                        .collect(),
                }
            })
            .collect();
        let next_id = o.data.next_id;
        o.data.next_id += 1_000_000;
        o.changed();
        let langs = self.langs.clone().unwrap_or_default();
        let steps = Steps {
            find: steps.find && only_block.is_none(),
            ..steps
        };
        self.touched.clear();
        self.running = Some(process::start(
            ctx.clone(),
            self.settings.clone(),
            langs,
            steps,
            jobs,
            next_id,
            force_read,
        ));
    }

    fn start_export(&mut self, ctx: &egui::Context, format: export::Format) {
        if self.exporting.is_some() {
            return;
        }
        let Some(o) = &mut self.open else { return };
        o.save();
        self.exporting = Some(export::start(
            ctx.clone(),
            o.dir.clone(),
            o.data.clone(),
            self.fonts.clone(),
            format,
            self.settings.workers,
        ));
    }

    fn shortcuts(&mut self, ctx: &egui::Context) {
        if ctx.wants_keyboard_input() || self.open.is_none() {
            return;
        }
        let (redo, undo, save, fit, next, prev, del, v, t, h) = ctx.input_mut(|i| {
            (
                i.consume_key(Modifiers::COMMAND | Modifiers::SHIFT, Key::Z)
                    || i.consume_key(Modifiers::COMMAND, Key::Y),
                i.consume_key(Modifiers::COMMAND, Key::Z),
                i.consume_key(Modifiers::COMMAND, Key::S),
                i.consume_key(Modifiers::COMMAND, Key::Num0),
                i.consume_key(Modifiers::NONE, Key::ArrowRight)
                    || i.consume_key(Modifiers::NONE, Key::PageDown)
                    || i.consume_key(Modifiers::NONE, Key::ArrowDown),
                i.consume_key(Modifiers::NONE, Key::ArrowLeft)
                    || i.consume_key(Modifiers::NONE, Key::PageUp)
                    || i.consume_key(Modifiers::NONE, Key::ArrowUp),
                i.consume_key(Modifiers::NONE, Key::Delete)
                    || i.consume_key(Modifiers::NONE, Key::Backspace),
                i.consume_key(Modifiers::NONE, Key::V),
                i.consume_key(Modifiers::NONE, Key::T),
                i.consume_key(Modifiers::NONE, Key::H),
            )
        });
        if v {
            self.tool = Tool::Select;
        }
        if t {
            self.tool = Tool::Text;
        }
        if h {
            self.tool = Tool::Pan;
        }
        let cur = self.open.as_ref().map(|o| o.data.current).unwrap_or(0);
        if next {
            self.go_to(cur + 1);
        }
        if prev && cur > 0 {
            self.go_to(cur - 1);
        }
        let Some(o) = &mut self.open else { return };
        if redo {
            o.step(false);
        } else if undo {
            o.step(true);
        }
        if save {
            o.dirty = true;
            o.save();
        }
        if fit {
            o.fit = true;
        }
        if del {
            if let Some(id) = o.selected.take() {
                o.snapshot(None);
                let cur = o.data.current;
                o.data.pages[cur].blocks.retain(|b| b.id != id);
                o.changed();
            }
        }
    }

    // ---------- panels ----------

    fn menu_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("menu")
            .frame(
                egui::Frame::new()
                    .fill(menubar())
                    .inner_margin(egui::Margin::symmetric(10, 6)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    ui.label(RichText::new("✒").color(rose()).size(18.0));
                    egui::menu::bar(ui, |ui| {
                        ui.menu_button("File", |ui| {
                            if ui.button("Import comics…").clicked() {
                                self.pick_comics();
                                ui.close_menu();
                            }
                            if ui.button("Open library folder").clicked() {
                                open_in_files(&self.settings.library);
                                ui.close_menu();
                            }
                            ui.separator();
                            let has = self.open.is_some();
                            if ui
                                .add_enabled(has, egui::Button::new("Export as CBZ"))
                                .clicked()
                            {
                                self.start_export(ctx, export::Format::Cbz);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(has, egui::Button::new("Export as PNG files"))
                                .clicked()
                            {
                                self.start_export(ctx, export::Format::Png);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(has, egui::Button::new("Show comic folder"))
                                .clicked()
                            {
                                if let Some(o) = &self.open {
                                    open_in_files(&o.dir);
                                }
                                ui.close_menu();
                            }
                            ui.separator();
                            if ui
                                .add_enabled(has, egui::Button::new("Close comic"))
                                .clicked()
                            {
                                self.close_project();
                                ui.close_menu();
                            }
                            if ui.button("Settings…").clicked() {
                                self.settings_open = true;
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("Edit", |ui| {
                            let Some(o) = &mut self.open else {
                                ui.label("Open a comic first.");
                                return;
                            };
                            if ui
                                .add_enabled(!o.undo.is_empty(), egui::Button::new("Undo   Ctrl+Z"))
                                .clicked()
                            {
                                o.step(true);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(
                                    !o.redo.is_empty(),
                                    egui::Button::new("Redo   Ctrl+Shift+Z"),
                                )
                                .clicked()
                            {
                                o.step(false);
                                ui.close_menu();
                            }
                            if ui
                                .add_enabled(
                                    o.selected.is_some(),
                                    egui::Button::new("Delete text box   Del"),
                                )
                                .clicked()
                            {
                                if let Some(id) = o.selected.take() {
                                    o.snapshot(None);
                                    let cur = o.data.current;
                                    o.data.pages[cur].blocks.retain(|b| b.id != id);
                                    o.changed();
                                }
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("Process", |ui| {
                            let has = self.open.is_some() && self.running.is_none();
                            for (label, steps) in [
                                (
                                    "Find text",
                                    Steps {
                                        find: true,
                                        read: false,
                                        translate: false,
                                        clean: false,
                                    },
                                ),
                                (
                                    "Read text in boxes",
                                    Steps {
                                        find: false,
                                        read: true,
                                        translate: false,
                                        clean: false,
                                    },
                                ),
                                (
                                    "Translate",
                                    Steps {
                                        find: false,
                                        read: false,
                                        translate: true,
                                        clean: false,
                                    },
                                ),
                                (
                                    "Clean up (cover original text)",
                                    Steps {
                                        find: false,
                                        read: false,
                                        translate: false,
                                        clean: true,
                                    },
                                ),
                            ] {
                                if ui.add_enabled(has, egui::Button::new(label)).clicked() {
                                    self.start_run(ctx, steps, None, false);
                                    ui.close_menu();
                                }
                            }
                            ui.separator();
                            ui.checkbox(&mut self.all_pages, "Apply to all pages");
                        });
                        ui.menu_button("View", |ui| {
                            if let Some(o) = &mut self.open {
                                if ui.button("Fit page   Ctrl+0").clicked() {
                                    o.fit = true;
                                    ui.close_menu();
                                }
                                if ui.button("Actual size (100%)").clicked() {
                                    o.zoom = 1.0;
                                    o.pan = Vec2::ZERO;
                                    ui.close_menu();
                                }
                            } else {
                                ui.label("Open a comic first.");
                            }
                        });
                        ui.menu_button("Theme", |ui| {
                            let before = self.settings.theme.clone();
                            ui.radio_value(&mut self.settings.theme, "light".to_owned(), "☀ Light");
                            ui.radio_value(&mut self.settings.theme, "dark".to_owned(), "🌙 Dark");
                            ui.radio_value(&mut self.settings.theme, "system".to_owned(), "💻 Same as system");
                            if self.settings.theme != before {
                                self.settings.save();
                                ui.close_menu();
                            }
                        });
                        ui.menu_button("Help", |ui| {
                            if ui.button("Add Inkwell to the app menu").clicked() {
                                match crate::launcher::install(true) {
                                    Ok(_) => self.toast("Inkwell is in your app menu: search for “Inkwell”.", false),
                                    Err(e) => self.toast(format!("Couldn't add it to the app menu: {e}"), true),
                                }
                                ui.close_menu();
                            }
                            if ui.button("About and shortcuts").clicked() {
                                self.about_open = true;
                                ui.close_menu();
                            }
                        });
                    });
                    let title = match &self.open {
                        Some(o) => {
                            let page = o
                                .data
                                .pages
                                .get(o.data.current)
                                .map(|p| p.name.clone())
                                .unwrap_or_default();
                            format!("{}   /   {}", o.data.name, page)
                        }
                        None => "Library".to_owned(),
                    };
                    let rect = ui.max_rect();
                    ui.painter().text(
                        rect.center(),
                        Align2::CENTER_CENTER,
                        title,
                        FontId::proportional(15.0),
                        text_c(),
                    );
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        let (icon, tip, next) = if dark() {
                            ("☀", "Switch to light mode", "light")
                        } else {
                            ("🌙", "Switch to dark mode", "dark")
                        };
                        if ui.button(icon).on_hover_text(tip).clicked() {
                            self.settings.theme = next.to_owned();
                            self.settings.save();
                        }
                        if self.open.is_some() && ui.button("◀ Library").clicked() {
                            self.close_project();
                        }
                    });
                });
            });
    }

    fn pages_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::left("pages")
            .exact_width(230.0).show_separator_line(false)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(sidebar())
                    .inner_margin(egui::Margin::symmetric(10, 10))
                    .stroke(Stroke::new(1.0_f32, border())),
            )
            .show(ctx, |ui| {
                let Some(o) = &mut self.open else { return };
                ui.horizontal(|ui| {
                    ui.label(RichText::new("Pages").size(17.0).strong());
                    egui::Frame::new()
                        .fill(rose_soft())
                        .corner_radius(CornerRadius::same(8))
                        .inner_margin(egui::Margin::symmetric(8, 2))
                        .show(ui, |ui| {
                            ui.label(RichText::new(o.data.pages.len().to_string()).color(rose()));
                        });
                });
                ui.add_space(4.0);
                ui.add(
                    egui::TextEdit::singleline(&mut self.filter)
                        .hint_text("🔍  Filter pages")
                        .desired_width(f32::INFINITY),
                );
                ui.add_space(6.0);
                let needle = self.filter.to_lowercase();
                let rows: Vec<usize> = (0..o.data.pages.len())
                    .filter(|&i| {
                        needle.is_empty()
                            || o.data.pages[i].name.to_lowercase().contains(&needle)
                            || (i + 1).to_string() == needle
                    })
                    .collect();
                let mut clicked = None;
                let mut area = egui::ScrollArea::vertical()
                    .id_salt("pages")
                    .auto_shrink([false, false]);
                if o.scroll_list {
                    o.scroll_list = false;
                    if let Some(k) = rows.iter().position(|&i| i == o.data.current) {
                        let spacing = ui.spacing().item_spacing.y;
                        let h = ui.available_height();
                        area = area.vertical_scroll_offset(
                            ((ROW_H + spacing) * k as f32 - h / 2.0 + ROW_H).max(0.0),
                        );
                    }
                }
                area.show_rows(ui, ROW_H, rows.len(), |ui, range| {
                    for k in range {
                        let i = rows[k];
                        let page = &o.data.pages[i];
                        if !o.thumbs.contains_key(&i) && o.thumb_pending.insert(i) {
                            let stem = Path::new(&page.file)
                                .file_stem()
                                .map(|s| s.to_string_lossy().into_owned())
                                .unwrap_or_else(|| i.to_string());
                            self.pool.push(Job::Thumb {
                                gen: o.gen,
                                index: i,
                                path: o.dir.join(&page.file),
                                cache: o.dir.join("thumbs").join(format!("{stem}.png")),
                            });
                        }
                        let (rect, resp) = ui
                            .allocate_exact_size(vec2(ui.available_width(), ROW_H), Sense::click());
                        let p = ui.painter();
                        let current = i == o.data.current;
                        if current {
                            p.rect_filled(rect, CornerRadius::same(12), rose_soft());
                        } else if resp.hovered() {
                            p.rect_filled(
                                rect,
                                CornerRadius::same(12),
                                hover_c(),
                            );
                        }
                        let thumb = Rect::from_min_size(
                            rect.min + vec2(8.0, 6.0),
                            vec2(52.0, ROW_H - 12.0),
                        );
                        match o.thumbs.get(&i) {
                            Some(t) => {
                                let s = t.size_vec2();
                                let scale = (thumb.width() / s.x).min(thumb.height() / s.y);
                                let r = Rect::from_center_size(thumb.center(), s * scale);
                                p.image(
                                    t.id(),
                                    r,
                                    Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                    Color32::WHITE,
                                );
                                p.rect_stroke(
                                    r,
                                    2.0,
                                    Stroke::new(1.0_f32, border()),
                                    StrokeKind::Outside,
                                );
                            }
                            None => {
                                p.rect_filled(thumb, 4.0, placeholder());
                            }
                        }
                        let x = thumb.right() + 12.0;
                        let mut name = format!("{}  {}", i + 1, page.name);
                        if name.chars().count() > 22 {
                            name = name.chars().take(21).collect::<String>() + "…";
                        }
                        p.text(
                            pos2(x, rect.top() + 10.0),
                            Align2::LEFT_TOP,
                            name,
                            FontId::proportional(14.0),
                            text_c(),
                        );
                        let n = page.blocks.len();
                        p.text(
                            pos2(x, rect.top() + 34.0),
                            Align2::LEFT_TOP,
                            format!("{} text box{}", n, if n == 1 { "" } else { "es" }),
                            FontId::proportional(12.5),
                            muted(),
                        );
                        if page.width > 0 {
                            p.text(
                                pos2(x, rect.top() + 54.0),
                                Align2::LEFT_TOP,
                                format!("{} × {}", page.width, page.height),
                                FontId::proportional(12.5),
                                muted(),
                            );
                        }
                        if resp.clicked() {
                            clicked = Some(i);
                        }
                    }
                });
                if let Some(i) = clicked {
                    self.go_to(i);
                    if let Some(o) = &mut self.open {
                        o.scroll_list = false;
                    }
                }
            });
    }

    fn right_panel(&mut self, ctx: &egui::Context) {
        egui::SidePanel::right("props")
            .exact_width(290.0).show_separator_line(false)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(surface())
                    .inner_margin(egui::Margin::symmetric(12, 10))
                    .stroke(Stroke::new(1.0_f32, border())),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    let w = (ui.available_width() - 8.0) / 2.0;
                    for (tab, label) in [
                        (Tab::Properties, "☰  Properties"),
                        (Tab::Translate, "🌐  Translate"),
                    ] {
                        let on = self.tab == tab;
                        let b = egui::Button::new(RichText::new(label).size(14.5))
                            .fill(if on {
                                hover_c()
                            } else {
                                surface()
                            })
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(10))
                            .min_size(vec2(w, 34.0));
                        if ui.add(b).clicked() {
                            self.tab = tab;
                        }
                    }
                });
                ui.separator();
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| match self.tab {
                        Tab::Properties => self.properties(ui, ctx),
                        Tab::Translate => self.translate_tab(ui),
                    });
            });
    }

    fn properties(&mut self, ui: &mut egui::Ui, ctx: &egui::Context) {
        let fonts = self.fonts.clone();
        let mut run: Option<(Steps, u64, bool)> = None;
        {
            let Some(o) = &mut self.open else { return };
            let selected = o.selected.and_then(|id| o.block(id).cloned());
            section(ui, "T", "Type", None);
            match selected {
                None => {
                    ui.label(RichText::new("Select a text box on the page, or draw one with the Text tool (T). Run ▶ can find the text for you.").color(muted()));
                }
                Some(mut b) => {
                    let before = b.clone();
                    let id = b.id;
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            caps(ui, "FONT");
                            egui::ComboBox::from_id_salt("font")
                                .width(196.0)
                                .selected_text(fonts.name_of(&b.font))
                                .show_ui(ui, |ui| {
                                    ui.add(
                                        egui::TextEdit::singleline(&mut self.font_filter)
                                            .hint_text("Search fonts"),
                                    );
                                    let f = self.font_filter.to_lowercase();
                                    egui::ScrollArea::vertical()
                                        .max_height(260.0)
                                        .show(ui, |ui| {
                                            ui.selectable_value(
                                                &mut b.font,
                                                String::new(),
                                                format!("Default ({})", fonts.name_of("")),
                                            );
                                            for (name, path) in fonts
                                                .list
                                                .iter()
                                                .filter(|(n, _)| {
                                                    f.is_empty() || n.to_lowercase().contains(&f)
                                                })
                                                .take(300)
                                            {
                                                ui.selectable_value(
                                                    &mut b.font,
                                                    path.clone(),
                                                    name,
                                                );
                                            }
                                        });
                                });
                        });
                        ui.vertical(|ui| {
                            caps(ui, "COLOR");
                            ui.color_edit_button_srgba_unmultiplied(&mut b.color);
                        });
                    });
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            caps(ui, "SIZE");
                            ui.horizontal(|ui| {
                                let mut auto = b.size.is_none();
                                if ui.checkbox(&mut auto, "Auto").changed() {
                                    b.size = if auto { None } else { Some(24.0) };
                                }
                                if let Some(s) = &mut b.size {
                                    ui.add(
                                        egui::DragValue::new(s)
                                            .range(4.0..=300.0)
                                            .speed(0.5)
                                            .suffix(" px"),
                                    );
                                }
                            });
                        });
                        ui.vertical(|ui| {
                            caps(ui, "STYLE");
                            ui.checkbox(&mut b.uppercase, "CAPS");
                        });
                    });
                    caps(ui, "ALIGNMENT");
                    ui.horizontal(|ui| {
                        for (a, label) in [
                            (Align::Left, "⬅ Left"),
                            (Align::Center, "↔ Center"),
                            (Align::Right, "Right ➡"),
                        ] {
                            let on = b.align == a;
                            let btn = egui::Button::new(label).fill(if on {
                                rose_mid()
                            } else {
                                chip()
                            });
                            if ui.add(btn).clicked() {
                                b.align = a;
                            }
                        }
                    });
                    ui.horizontal(|ui| {
                        ui.vertical(|ui| {
                            caps(ui, "border()");
                            ui.color_edit_button_srgba_unmultiplied(&mut b.stroke);
                        });
                        ui.vertical(|ui| {
                            caps(ui, "WIDTH");
                            ui.add(
                                egui::DragValue::new(&mut b.stroke_width)
                                    .range(0.0..=12.0)
                                    .speed(0.1),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    caps(ui, "CLEAN UP");
                    ui.horizontal(|ui| {
                        ui.checkbox(&mut b.cover, "Cover original text");
                        ui.color_edit_button_srgba_unmultiplied(&mut b.cover_color);
                    });
                    ui.horizontal(|ui| {
                        ui.radio_value(&mut b.cover_shape, CoverShape::Oval, "Oval");
                        ui.radio_value(&mut b.cover_shape, CoverShape::Box, "Box");
                        if ui.small_button("Pick colour from page").clicked() {
                            run = Some((
                                Steps {
                                    find: false,
                                    read: false,
                                    translate: false,
                                    clean: true,
                                },
                                id,
                                false,
                            ));
                        }
                    });
                    ui.add_space(4.0);
                    caps(ui, "ORIGINAL TEXT");
                    let src = ui.add(
                        egui::TextEdit::multiline(&mut b.source)
                            .desired_rows(2)
                            .desired_width(f32::INFINITY),
                    );
                    caps(ui, "TRANSLATION");
                    let tr = ui.add(
                        egui::TextEdit::multiline(&mut b.translation)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY)
                            .hint_text("Type the translation…"),
                    );
                    if o.focus_translation {
                        o.focus_translation = false;
                        tr.request_focus();
                    }
                    ui.horizontal_wrapped(|ui| {
                        if ui
                            .button("👁 Read text")
                            .on_hover_text("Read the original text in this box")
                            .clicked()
                        {
                            run = Some((
                                Steps {
                                    find: false,
                                    read: true,
                                    translate: false,
                                    clean: false,
                                },
                                id,
                                true,
                            ));
                        }
                        if ui.button("🌐 Translate box").clicked() {
                            run = Some((
                                Steps {
                                    find: false,
                                    read: false,
                                    translate: true,
                                    clean: false,
                                },
                                id,
                                false,
                            ));
                        }
                        if ui.button("🗑 Delete").clicked() {
                            o.snapshot(None);
                            let cur = o.data.current;
                            o.data.pages[cur].blocks.retain(|x| x.id != id);
                            o.selected = None;
                            o.changed();
                        }
                    });
                    if b != before && o.block(id).is_some() {
                        let field: u8 = if src.changed() {
                            1
                        } else if tr.changed() {
                            2
                        } else {
                            3
                        };
                        let key = if field == 3 { None } else { Some((id, field)) };
                        o.snapshot(key);
                        if let Some(slot) = o.block_mut(id) {
                            *slot = b;
                        }
                        o.changed();
                    }
                }
            }

            ui.add_space(8.0);
            ui.separator();
            let cur = o.data.current;
            let count = o.data.pages.get(cur).map(|p| p.blocks.len()).unwrap_or(0);
            section(ui, "≡", "Layers", Some(count + 1));
            let mut select = None;
            let mut toggle = None;
            if let Some(p) = o.data.pages.get(cur) {
                for b in p.blocks.iter().rev() {
                    let on = o.selected == Some(b.id);
                    let (rect, resp) =
                        ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::click());
                    let painter = ui.painter();
                    if on {
                        painter.rect_filled(rect, 8.0, rose_soft());
                    } else if resp.hovered() {
                        painter.rect_filled(rect, 8.0, chip());
                    }
                    painter.text(
                        pos2(rect.left() + 12.0, rect.center().y),
                        Align2::LEFT_CENTER,
                        "T",
                        FontId::proportional(18.0),
                        muted(),
                    );
                    let mut label = b.label();
                    if label.chars().count() > 26 {
                        label = label.chars().take(25).collect::<String>() + "…";
                    }
                    painter.text(
                        pos2(rect.left() + 36.0, rect.top() + 6.0),
                        Align2::LEFT_TOP,
                        label,
                        FontId::proportional(14.0),
                        text_c(),
                    );
                    let kind = if b.translation.trim().is_empty() {
                        "Not translated"
                    } else {
                        "Dialogue"
                    };
                    painter.text(
                        pos2(rect.left() + 36.0, rect.top() + 26.0),
                        Align2::LEFT_TOP,
                        kind,
                        FontId::proportional(11.5),
                        muted(),
                    );
                    let eye = Rect::from_center_size(
                        pos2(rect.right() - 16.0, rect.center().y),
                        vec2(24.0, 24.0),
                    );
                    painter.text(
                        eye.center(),
                        Align2::CENTER_CENTER,
                        if b.visible { "👁" } else { "—" },
                        FontId::proportional(15.0),
                        if b.visible { text_c() } else { muted() },
                    );
                    if resp.clicked() {
                        let hit_eye = resp
                            .interact_pointer_pos()
                            .is_some_and(|p| eye.expand(4.0).contains(p));
                        if hit_eye {
                            toggle = Some(b.id);
                        } else {
                            select = Some(b.id);
                        }
                    }
                }
            }
            let (rect, _) =
                ui.allocate_exact_size(vec2(ui.available_width(), 46.0), Sense::hover());
            let painter = ui.painter();
            painter.text(
                pos2(rect.left() + 12.0, rect.center().y),
                Align2::LEFT_CENTER,
                "🖼",
                FontId::proportional(16.0),
                muted(),
            );
            painter.text(
                pos2(rect.left() + 36.0, rect.top() + 6.0),
                Align2::LEFT_TOP,
                "Original artwork",
                FontId::proportional(14.0),
                text_c(),
            );
            painter.text(
                pos2(rect.left() + 36.0, rect.top() + 26.0),
                Align2::LEFT_TOP,
                "Image",
                FontId::proportional(11.5),
                muted(),
            );
            painter.text(
                pos2(rect.right() - 16.0, rect.center().y),
                Align2::CENTER_CENTER,
                "🔒",
                FontId::proportional(14.0),
                muted(),
            );
            if let Some(id) = select {
                o.selected = Some(id);
            }
            if let Some(id) = toggle {
                o.snapshot(None);
                if let Some(b) = o.block_mut(id) {
                    b.visible = !b.visible;
                }
                o.changed();
            }
        }
        if let Some((steps, id, force)) = run {
            self.start_run(ctx, steps, Some(id), force);
        }
    }

    fn translate_tab(&mut self, ui: &mut egui::Ui) {
        let before = self.settings.clone();
        section(ui, "🌐", "Translation server", None);
        ui.label(RichText::new("Any OpenAI-compatible chat server: Ollama on this laptop, LM Studio, or an online API.").color(muted()).size(12.5));
        caps(ui, "SERVER ADDRESS");
        ui.add(
            egui::TextEdit::singleline(&mut self.settings.endpoint).desired_width(f32::INFINITY),
        );
        caps(ui, "MODEL");
        ui.add(egui::TextEdit::singleline(&mut self.settings.model).desired_width(f32::INFINITY));
        caps(ui, "API KEY (ONLINE SERVICES ONLY)");
        ui.add(
            egui::TextEdit::singleline(&mut self.settings.api_key)
                .password(true)
                .desired_width(f32::INFINITY),
        );
        caps(ui, "TRANSLATE INTO");
        egui::ComboBox::from_id_salt("target")
            .width(ui.available_width())
            .selected_text(self.settings.target_lang.clone())
            .show_ui(ui, |ui| {
                for l in [
                    "English",
                    "Spanish",
                    "Portuguese",
                    "French",
                    "German",
                    "Italian",
                    "Russian",
                    "Japanese",
                    "Korean",
                    "Chinese (Simplified)",
                    "Chinese (Traditional)",
                    "Vietnamese",
                    "Indonesian",
                    "Thai",
                    "Arabic",
                    "Polish",
                    "Turkish",
                ] {
                    ui.selectable_value(&mut self.settings.target_lang, l.to_owned(), l);
                }
            });
        caps(ui, "EXTRA INSTRUCTIONS");
        ui.add(
            egui::TextEdit::multiline(&mut self.settings.instructions)
                .desired_rows(3)
                .desired_width(f32::INFINITY)
                .hint_text("e.g. Keep honorifics like -san."),
        );

        ui.add_space(8.0);
        section(ui, "👁", "Reading text (Tesseract)", None);
        match &self.langs {
            None => {
                ui.label(RichText::new("Tesseract isn't installed. Install the 'tesseract' package to find and read text.").color(rose()));
            }
            Some(langs) => {
                caps(ui, "TEXT LANGUAGE ON THE PAGES");
                egui::ComboBox::from_id_salt("ocr")
                    .width(ui.available_width())
                    .selected_text(self.settings.ocr_lang.clone())
                    .show_ui(ui, |ui| {
                        for l in ["jpn", "chi_sim", "chi_tra", "kor", "eng"] {
                            let have = langs.iter().any(|x| x == l);
                            let label = if have {
                                l.to_owned()
                            } else {
                                format!("{l} (not installed)")
                            };
                            ui.selectable_value(&mut self.settings.ocr_lang, l.to_owned(), label);
                        }
                        for l in langs.iter().filter(|l| {
                            !["jpn", "chi_sim", "chi_tra", "kor", "eng", "osd"]
                                .contains(&l.as_str())
                        }) {
                            ui.selectable_value(&mut self.settings.ocr_lang, l.clone(), l);
                        }
                    });
                if !langs.iter().any(|l| l == &self.settings.ocr_lang) {
                    ui.label(
                        RichText::new(format!(
                            "The '{}' language pack isn't installed. On Arch: sudo pacman -S tesseract-data-{}",
                            self.settings.ocr_lang, self.settings.ocr_lang
                        ))
                        .color(rose())
                        .size(12.5),
                    );
                }
            }
        }
        ui.add_space(8.0);
        section(ui, "▶", "Run steps", None);
        ui.checkbox(&mut self.steps.find, "Find text on pages without boxes");
        ui.checkbox(&mut self.steps.read, "Read the original text");
        ui.checkbox(
            &mut self.steps.clean,
            "Clean up (cover the original lettering)",
        );
        ui.checkbox(&mut self.steps.translate, "Translate");
        ui.checkbox(&mut self.all_pages, "All pages (otherwise this page)");
        if self.settings != before {
            self.settings.save();
        }
    }

    fn run_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::top("runbar")
            .frame(
                egui::Frame::new()
                    .fill(surface())
                    .inner_margin(egui::Margin::symmetric(12, 8)),
            )
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    if let Some(r) = &self.running {
                        let frac = if r.total == 0 {
                            0.0
                        } else {
                            r.done as f32 / r.total as f32
                        };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(220.0)
                                .text(r.label.clone())
                                .fill(rose()),
                        );
                    } else if let Some(e) = &self.exporting {
                        let frac = if e.total == 0 {
                            0.0
                        } else {
                            e.done as f32 / e.total as f32
                        };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(220.0)
                                .text(format!("Exporting {}/{}", e.done, e.total))
                                .fill(rose()),
                        );
                    }
                    if let Some(r) = self.imports.iter().find(|r| matches!(r.state, ImportState::Running)) {
                        let frac = if r.total == 0 { 0.0 } else { r.done as f32 / r.total as f32 };
                        ui.add(
                            egui::ProgressBar::new(frac)
                                .desired_width(200.0)
                                .text(format!("New project “{}” {}/{}", r.name, r.done, r.total))
                                .fill(rose()),
                        );
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(r) = &self.running {
                            if ui
                                .add(rose_button("■ Stop").min_size(vec2(90.0, 34.0)))
                                .clicked()
                            {
                                r.cancel.store(true, Ordering::Relaxed);
                            }
                        } else if let Some(e) = &self.exporting {
                            if ui
                                .add(rose_button("■ Stop export").min_size(vec2(110.0, 34.0)))
                                .clicked()
                            {
                                e.cancel.store(true, Ordering::Relaxed);
                            }
                        } else if ui
                            .add(rose_button("▶ Run").min_size(vec2(90.0, 34.0)))
                            .on_hover_text("Run the steps chosen in the Translate tab")
                            .clicked()
                        {
                            let steps = self.steps;
                            self.start_run(ctx, steps, None, false);
                        }
                        let model = format!("🌐  {}  ⏷", self.settings.model);
                        let chip = egui::Button::new(RichText::new(model).size(14.5))
                            .fill(chip())
                            .stroke(Stroke::NONE)
                            .corner_radius(CornerRadius::same(10))
                            .min_size(vec2(0.0, 34.0));
                        if ui.add(chip).on_hover_text("Translation settings").clicked() {
                            self.tab = Tab::Translate;
                        }
                        let scope = if self.all_pages {
                            "All pages"
                        } else {
                            "This page"
                        };
                        egui::ComboBox::from_id_salt("scope")
                            .selected_text(scope)
                            .width(110.0)
                            .show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.all_pages, false, "This page");
                                ui.selectable_value(&mut self.all_pages, true, "All pages");
                            });
                    });
                });
            });
    }

    fn status_bar(&mut self, ctx: &egui::Context) {
        egui::TopBottomPanel::bottom("zoombar")
            .frame(
                egui::Frame::new()
                    .fill(surface())
                    .inner_margin(egui::Margin::symmetric(12, 6))
                    .stroke(Stroke::new(1.0_f32, border())),
            )
            .show(ctx, |ui| {
                let Some(o) = &mut self.open else { return };
                ui.horizontal(|ui| {
                    ui.spacing_mut().slider_width = 100.0;
                    ui.add(
                        egui::Slider::new(&mut o.zoom, 0.05..=8.0)
                            .logarithmic(true)
                            .show_value(false),
                    );
                    ui.label(format!("{:.0}%", o.zoom * 100.0));
                    if ui.small_button("Fit").clicked() {
                        o.fit = true;
                    }
                    ui.separator();
                    let status = if let Some(e) = &o.save_error {
                        RichText::new(format!("⚠ Not saved: {e}")).color(rose())
                    } else if o.dirty {
                        RichText::new("● Saving…").color(muted())
                    } else {
                        RichText::new("✔ Saved").color(muted())
                    };
                    ui.label(status.size(12.5));
                    if let Some(p) = self.last_export.clone() {
                        ui.separator();
                        if ui.small_button("📂 Show export").clicked() {
                            open_in_files(p.parent().unwrap_or(&p));
                        }
                    }
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if let Some(p) = o.data.pages.get(o.data.current) {
                            let full = format!(
                                "{} / {}  ·  {} × {} px",
                                o.data.current + 1,
                                o.data.pages.len(),
                                p.width,
                                p.height
                            );
                            let short = format!("{} / {}", o.data.current + 1, o.data.pages.len());
                            let room = ui.available_width();
                            let text = if room > 190.0 { full } else if room > 60.0 { short } else { String::new() };
                            ui.label(RichText::new(text).color(muted()).size(12.5));
                        }
                    });
                });
            });
    }

    fn canvas(&mut self, ctx: &egui::Context) {
        let fonts = self.fonts.clone();
        let tool = self.tool;
        let cap = self.settings.texture_cap;
        let default_font = self.settings.default_font.clone();
        let mut new_tool = None;
        let mut to_props = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(canvas_c()))
            .show(ctx, |ui| {
                let Some(o) = &mut self.open else { return };
                if o.data.pages.is_empty() {
                    ui.centered_and_justified(|ui| ui.label("This comic has no pages."));
                    return;
                }
                let cur = o.data.current;
                // Load this page and its neighbours.
                for i in [cur, cur + 1, cur.wrapping_sub(1)] {
                    if i < o.data.pages.len()
                        && !o.pages.contains_key(&i)
                        && !o.failed.contains_key(&i)
                        && o.page_pending.insert(i)
                    {
                        self.pool.push(Job::Page {
                            gen: o.gen,
                            index: i,
                            path: o.dir.join(&o.data.pages[i].file),
                            cap,
                        });
                    }
                }
                let (resp, painter) =
                    ui.allocate_painter(ui.available_size(), Sense::click_and_drag());
                let area = resp.rect;
                let page = &o.data.pages[cur];
                let tex = o.pages.get(&cur).cloned();
                let size = if page.width > 0 {
                    vec2(page.width as f32, page.height as f32)
                } else if let Some(t) = &tex {
                    t.size_vec2()
                } else {
                    vec2(1000.0, 1400.0)
                };
                if o.fit {
                    o.zoom = ((area.width() - 120.0) / size.x)
                        .min((area.height() - 48.0) / size.y)
                        .clamp(0.05, 8.0);
                    o.pan = Vec2::ZERO;
                    o.fit = false;
                }
                // Zoom around the pointer (pinch or Ctrl+scroll), scroll to pan.
                if resp.hovered() {
                    let (scroll, zd, hover) = ui
                        .input(|i| (i.smooth_scroll_delta, i.zoom_delta(), i.pointer.hover_pos()));
                    if zd != 1.0 {
                        let anchor = hover.unwrap_or(area.center());
                        let origin = area.center() + o.pan - size * o.zoom / 2.0;
                        let new_zoom = (o.zoom * zd).clamp(0.05, 8.0);
                        let f = new_zoom / o.zoom;
                        let new_origin = anchor - (anchor - origin) * f;
                        o.pan += new_origin - (area.center() + o.pan - size * new_zoom / 2.0);
                        o.zoom = new_zoom;
                    } else if scroll != Vec2::ZERO {
                        o.pan += scroll;
                    }
                }
                let zoom = o.zoom;
                let origin = area.center() + o.pan - size * zoom / 2.0;
                let to_screen = |p: Pos2| origin + p.to_vec2() * zoom;
                let to_page = |s: Pos2| ((s - origin) / zoom).to_pos2();
                let page_rect = Rect::from_min_size(origin, size * zoom);

                painter.rect_filled(
                    page_rect.translate(vec2(0.0, 3.0)).expand(2.0),
                    4.0,
                    Color32::from_black_alpha(18),
                );
                match &tex {
                    Some(t) => {
                        painter.image(
                            t.id(),
                            page_rect,
                            Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                            Color32::WHITE,
                        );
                    }
                    None => {
                        painter.rect_filled(page_rect, 0.0, Color32::WHITE);
                        let msg = o
                            .failed
                            .get(&cur)
                            .map(|e| format!("Can't show this page: {e}"))
                            .unwrap_or_else(|| "Loading page…".into());
                        painter.text(
                            page_rect.center(),
                            Align2::CENTER_CENTER,
                            msg,
                            FontId::proportional(15.0),
                            muted(),
                        );
                    }
                }

                // Text boxes: the same pixels export will paste.
                let blocks: Vec<Block> = o.data.pages[cur].blocks.clone();
                for b in &blocks {
                    let r = Rect::from_min_max(
                        to_screen(pos2(b.x, b.y)),
                        to_screen(pos2(b.x + b.w, b.y + b.h)),
                    );
                    if b.visible {
                        let key = b.render_key();
                        if o.block_tex.get(&b.id).map(|(k, _)| *k) != Some(key) {
                            let img = textfit::render_block(b, &fonts);
                            let ci = egui::ColorImage::from_rgba_unmultiplied(
                                [img.width() as usize, img.height() as usize],
                                img.as_raw(),
                            );
                            let t = ctx.load_texture(
                                format!("block-{}", b.id),
                                ci,
                                egui::TextureOptions::LINEAR,
                            );
                            o.block_tex.insert(b.id, (key, t));
                        }
                        if let Some((_, t)) = o.block_tex.get(&b.id) {
                            painter.image(
                                t.id(),
                                r,
                                Rect::from_min_max(pos2(0.0, 0.0), pos2(1.0, 1.0)),
                                Color32::WHITE,
                            );
                        }
                    }
                    if o.selected == Some(b.id) {
                        painter.rect_stroke(
                            r,
                            2.0,
                            Stroke::new(2.0_f32, rose()),
                            StrokeKind::Outside,
                        );
                        let h = Rect::from_center_size(r.max, vec2(10.0, 10.0));
                        painter.rect_filled(h, 2.0, Color32::WHITE);
                        painter.rect_stroke(h, 2.0, Stroke::new(1.5_f32, rose()), StrokeKind::Inside);
                    } else {
                        let col = if b.translation.trim().is_empty() {
                            rose()
                        } else {
                            rose_a(90)
                        };
                        dashed_rect(&painter, r, Stroke::new(1.2_f32, col), 5.0, 4.0);
                    }
                }
                o.block_tex
                    .retain(|id, _| blocks.iter().any(|b| b.id == *id));

                // Pointer.
                let hit = |p: Pos2| {
                    blocks
                        .iter()
                        .rev()
                        .find(|b| p.x >= b.x && p.x <= b.x + b.w && p.y >= b.y && p.y <= b.y + b.h)
                        .map(|b| b.id)
                };
                let selected = o.selected;
                let on_handle = |s: Pos2| {
                    selected
                        .and_then(|id| blocks.iter().find(|b| b.id == id))
                        .is_some_and(|b| {
                            Rect::from_center_size(
                                to_screen(pos2(b.x + b.w, b.y + b.h)),
                                vec2(16.0, 16.0),
                            )
                            .contains(s)
                        })
                };
                let pointer = resp.interact_pointer_pos().or(resp.hover_pos());
                if let Some(s) = resp.hover_pos() {
                    let icon = match (tool, &o.drag) {
                        (_, Some(Drag::Pan)) => egui::CursorIcon::Grabbing,
                        (Tool::Pan, _) => egui::CursorIcon::Grab,
                        (Tool::Text, _) => egui::CursorIcon::Crosshair,
                        _ if on_handle(s) => egui::CursorIcon::ResizeNwSe,
                        _ if hit(to_page(s)).is_some() => egui::CursorIcon::Move,
                        _ => egui::CursorIcon::Default,
                    };
                    ctx.set_cursor_icon(icon);
                }
                if resp.drag_started() {
                    if let Some(s) = pointer {
                        let p = to_page(s);
                        let other =
                            ui.input(|i| i.pointer.middle_down() || i.pointer.secondary_down());
                        let drag = if other || tool == Tool::Pan {
                            Drag::Pan
                        } else if tool == Tool::Text {
                            Drag::Create { start: p }
                        } else if on_handle(s) {
                            o.snapshot(None);
                            Drag::Resize {
                                id: selected.unwrap_or_default(),
                            }
                        } else if let Some(id) = hit(p) {
                            o.selected = Some(id);
                            o.snapshot(None);
                            let b = blocks.iter().find(|b| b.id == id).expect("hit box");
                            Drag::Move {
                                id,
                                grab: p - pos2(b.x, b.y),
                            }
                        } else {
                            Drag::Create { start: p }
                        };
                        o.drag = Some(drag);
                    }
                }
                if resp.dragged() {
                    if let Some(s) = pointer {
                        let p = to_page(s);
                        match &o.drag {
                            Some(Drag::Pan) => o.pan += resp.drag_delta(),
                            Some(Drag::Move { id, grab }) => {
                                let (id, grab) = (*id, *grab);
                                if let Some(b) = o.block_mut(id) {
                                    b.x = (p.x - grab.x).round();
                                    b.y = (p.y - grab.y).round();
                                }
                                o.changed();
                            }
                            Some(Drag::Resize { id }) => {
                                let id = *id;
                                if let Some(b) = o.block_mut(id) {
                                    b.w = (p.x - b.x).max(12.0).round();
                                    b.h = (p.y - b.y).max(12.0).round();
                                }
                                o.changed();
                            }
                            Some(Drag::Create { start }) => {
                                let r = Rect::from_two_pos(to_screen(*start), s);
                                painter.rect_filled(
                                    r,
                                    2.0,
                                    rose_a(30),
                                );
                                painter.rect_stroke(
                                    r,
                                    2.0,
                                    Stroke::new(1.5_f32, rose()),
                                    StrokeKind::Inside,
                                );
                            }
                            None => {}
                        }
                    }
                }
                if resp.drag_stopped() {
                    if let (Some(Drag::Create { start }), Some(s)) = (&o.drag, pointer) {
                        let r = Rect::from_two_pos(*start, to_page(s));
                        if r.width() >= 8.0 && r.height() >= 8.0 {
                            o.snapshot(None);
                            let id = o.data.next_id;
                            o.data.next_id += 1;
                            let b = Block {
                                id,
                                x: r.min.x.round(),
                                y: r.min.y.round(),
                                w: r.width().round(),
                                h: r.height().round(),
                                font: default_font.clone(),
                                cover: false,
                                ..Block::default()
                            };
                            o.data.pages[cur].blocks.push(b);
                            o.selected = Some(id);
                            o.focus_translation = true;
                            o.changed();
                            new_tool = Some(Tool::Select);
                            to_props = true;
                        }
                    }
                    o.drag = None;
                    o.coalesce = None;
                }
                if resp.clicked() {
                    if let Some(s) = pointer {
                        o.selected = hit(to_page(s));
                        o.coalesce = None;
                    }
                }
                if resp.double_clicked() && o.selected.is_some() {
                    o.focus_translation = true;
                    to_props = true;
                }

                // Tool rail.
                egui::Area::new(egui::Id::new("tools"))
                    .fixed_pos(area.left_top() + vec2(16.0, 16.0))
                    .order(egui::Order::Foreground)
                    .show(ctx, |ui| {
                        egui::Frame::new()
                            .fill(surface())
                            .corner_radius(CornerRadius::same(24))
                            .stroke(Stroke::new(1.0_f32, border()))
                            .shadow(egui::epaint::Shadow {
                                offset: [0, 2],
                                blur: 10,
                                spread: 0,
                                color: Color32::from_black_alpha(20),
                            })
                            .inner_margin(egui::Margin::same(6))
                            .show(ui, |ui| {
                                ui.vertical(|ui| {
                                    for (t, icon, tip) in [
                                        (Tool::Select, "⬉", "Select and move boxes (V)"),
                                        (Tool::Text, "T", "Draw a text box (T)"),
                                        (Tool::Pan, "✋", "Move the page (H)"),
                                    ] {
                                        let on = tool == t;
                                        let b = egui::Button::new(
                                            RichText::new(icon).size(20.0).color(if on {
                                                rose()
                                            } else {
                                                text_c()
                                            }),
                                        )
                                        .fill(if on { rose_soft() } else { surface() })
                                        .stroke(Stroke::NONE)
                                        .corner_radius(CornerRadius::same(18))
                                        .min_size(vec2(40.0, 40.0));
                                        if ui.add(b).on_hover_text(tip).clicked() {
                                            new_tool = Some(t);
                                        }
                                    }
                                });
                            });
                    });
            });
        if let Some(t) = new_tool {
            self.tool = t;
        }
        if to_props {
            self.tab = Tab::Properties;
        }
    }

    fn start_view(&mut self, ctx: &egui::Context) {
        let mut open = None;
        let mut pick = false;
        let mut pick_lib = false;
        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(sidebar()).inner_margin(egui::Margin::same(24)))
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().auto_shrink([false, false]).show(ui, |ui| {
                    ui.vertical_centered(|ui| {
                        ui.set_max_width(820.0);
                        ui.add_space(8.0);
                        ui.label(RichText::new("✒ Inkwell").size(30.0).strong().color(rose()));
                        ui.label(RichText::new("Translate manga pages: drop comics, find and read the text, translate, clean up, export.").color(muted()));
                        ui.add_space(14.0);
                        if let Some(b) = &self.banner {
                            egui::Frame::new()
                                .fill(banner_bg())
                                .stroke(Stroke::new(1.0_f32, banner_line()))
                                .corner_radius(CornerRadius::same(10))
                                .inner_margin(egui::Margin::same(12))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.label(RichText::new(format!("🛟  {b}")).color(banner_text()));
                                });
                            ui.add_space(10.0);
                        }
                        // Drop zone.
                        let (rect, resp) = ui.allocate_exact_size(vec2(ui.available_width(), 190.0), Sense::click());
                        let p = ui.painter();
                        p.rect_filled(rect, 18.0, if resp.hovered() { rose_soft() } else { surface() });
                        dashed_rect(p, rect.shrink(1.0), Stroke::new(2.0_f32, rose_mid()), 10.0, 7.0);
                        p.text(rect.center() - vec2(0.0, 44.0), Align2::CENTER_CENTER, "📥", FontId::proportional(40.0), rose());
                        p.text(rect.center() + vec2(0.0, 6.0), Align2::CENTER_CENTER, "Drop CBZ or ZIP comics here", FontId::proportional(20.0), text_c());
                        p.text(
                            rect.center() + vec2(0.0, 34.0),
                            Align2::CENTER_CENTER,
                            "Each comic gets its own folder. Several at once is fine.",
                            FontId::proportional(13.5),
                            muted(),
                        );
                        p.text(rect.center() + vec2(0.0, 60.0), Align2::CENTER_CENTER, "or click to choose files", FontId::proportional(13.0), rose());
                        if resp.clicked() {
                            pick = true;
                        }
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new(format!("Library: {}", self.settings.library.display())).color(muted()).size(12.5));
                            if ui.small_button("📂 Open").clicked() {
                                open_in_files(&self.settings.library);
                            }
                            if ui.small_button("Change…").clicked() {
                                pick_lib = true;
                            }
                        });

                        if !self.imports.is_empty() {
                            ui.add_space(10.0);
                            ui.horizontal(|ui| {
                                ui.label(RichText::new("Imports").size(16.0).strong());
                                if self.imports.iter().all(|r| !matches!(r.state, ImportState::Running)) && ui.small_button("Clear").clicked() {
                                    self.imports.clear();
                                }
                            });
                            for r in &self.imports {
                                ui.horizontal(|ui| {
                                    ui.label(RichText::new(&r.name).strong());
                                    match &r.state {
                                        ImportState::Running => {
                                            let frac = if r.total == 0 { 0.0 } else { r.done as f32 / r.total as f32 };
                                            ui.add(egui::ProgressBar::new(frac).fill(rose()).text(format!("{}/{} pages", r.done, r.total)));
                                        }
                                        ImportState::Done => {
                                            ui.label(RichText::new(format!("✔ {} pages", r.total)).color(ok_text()));
                                        }
                                        ImportState::Failed(e) => {
                                            ui.label(RichText::new(format!("✖ {e}")).color(rose()));
                                        }
                                    }
                                });
                            }
                        }

                        ui.add_space(14.0);
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Your comics").size(18.0).strong());
                            ui.label(RichText::new(self.library.len().to_string()).color(muted()));
                        });
                        ui.add_space(4.0);
                        if self.library.is_empty() {
                            ui.label(RichText::new("Nothing here yet. Drop a comic above to start.").color(muted()));
                        }
                        for e in &self.library {
                            egui::Frame::new()
                                .fill(surface())
                                .stroke(Stroke::new(1.0_f32, border()))
                                .corner_radius(CornerRadius::same(12))
                                .inner_margin(egui::Margin::symmetric(14, 10))
                                .show(ui, |ui| {
                                    ui.set_width(ui.available_width());
                                    ui.horizontal(|ui| {
                                        ui.label(RichText::new("📖").size(22.0));
                                        ui.vertical(|ui| {
                                            ui.label(RichText::new(&e.name).size(15.0).strong());
                                            let sub = if e.interrupted {
                                                "Import was interrupted — it resumes automatically".to_owned()
                                            } else {
                                                format!("{} pages · changed {}", e.pages, ago(e.modified))
                                            };
                                            ui.label(RichText::new(sub).color(muted()).size(12.5));
                                        });
                                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                                            if ui.small_button("🗑").on_hover_text("Delete this comic's folder").clicked() {
                                                self.confirm_delete = Some(e.dir.clone());
                                            }
                                            if ui.small_button("📂").on_hover_text("Show folder").clicked() {
                                                open_in_files(&e.dir);
                                            }
                                            if !e.interrupted && ui.add(rose_button("Open")).clicked() {
                                                open = Some(e.dir.clone());
                                            }
                                        });
                                    });
                                });
                            ui.add_space(4.0);
                        }
                    });
                });
            });
        if pick {
            self.pick_comics();
        }
        if pick_lib {
            self.pick_library();
        }
        if let Some(d) = open {
            self.banner = None;
            self.open_project(d, None);
        }
    }

    fn windows(&mut self, ctx: &egui::Context) {
        if let Some(problem) = self.run_problem.clone() {
            let mut keep = true;
            egui::Window::new("Run needs something")
                .collapsible(false)
                .resizable(false)
                .default_width(520.0)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.add(egui::Label::new(RichText::new(&problem).size(14.0)).selectable(true));
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        if ui.add(rose_button("OK")).clicked() {
                            keep = false;
                        }
                        if ui.button("Copy text").clicked() {
                            ui.ctx().copy_text(problem.clone());
                        }
                        if ui.button("Open the Translate tab").clicked() {
                            self.tab = Tab::Translate;
                            keep = false;
                        }
                    });
                });
            if !keep {
                self.run_problem = None;
            }
        }
        if let Some(dir) = self.confirm_delete.clone() {
            let name = dir
                .file_name()
                .map(|n| n.to_string_lossy().into_owned())
                .unwrap_or_default();
            let mut keep = true;
            egui::Window::new("Delete comic?")
                .collapsible(false)
                .resizable(false)
                .anchor(Align2::CENTER_CENTER, vec2(0.0, 0.0))
                .show(ctx, |ui| {
                    ui.label(format!("“{name}” and its folder (pages, text boxes, exports) will be removed. This can't be undone."));
                    ui.horizontal(|ui| {
                        if ui.add(rose_button("Delete")).clicked() {
                            let _ = std::fs::remove_dir_all(&dir);
                            keep = false;
                            self.library_stale = true;
                        }
                        if ui.button("Cancel").clicked() {
                            keep = false;
                        }
                    });
                });
            if !keep {
                self.confirm_delete = None;
            }
        }
        if self.settings_open {
            let mut openw = true;
            let before = self.settings.clone();
            let mut change_lib = false;
            let fonts = self.fonts.clone();
            egui::Window::new("Settings")
                .open(&mut openw)
                .default_width(460.0)
                .show(ctx, |ui| {
                    section(ui, "📚", "Library", None);
                    ui.label(
                        RichText::new(self.settings.library.display().to_string()).color(muted()),
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Change folder…").clicked() {
                            change_lib = true;
                        }
                        if ui.button("📂 Open").clicked() {
                            open_in_files(&self.settings.library);
                        }
                    });
                    section(ui, "🌙", "Appearance", None);
                ui.horizontal(|ui| {
                    ui.radio_value(&mut self.settings.theme, "light".to_owned(), "Light");
                    ui.radio_value(&mut self.settings.theme, "dark".to_owned(), "Dark");
                    ui.radio_value(&mut self.settings.theme, "system".to_owned(), "Same as system");
                });
                section(ui, "💻", "This laptop", None);
                    ui.label(format!(
                        "{} cores · {:.0} GB memory",
                        self.machine.cores, self.machine.memory_gb
                    ));
                    egui::Grid::new("laptop")
                        .num_columns(2)
                        .spacing([12.0, 6.0])
                        .show(ui, |ui| {
                            ui.label("Picture workers");
                            ui.add(
                                egui::DragValue::new(&mut self.settings.workers)
                                    .range(1..=self.machine.cores.max(1)),
                            );
                            ui.end_row();
                            ui.label("Largest page shown (px)");
                            egui::ComboBox::from_id_salt("cap")
                                .selected_text(self.settings.texture_cap.to_string())
                                .show_ui(ui, |ui| {
                                    for c in [2048u32, 3072, 4096, 8192] {
                                        ui.selectable_value(
                                            &mut self.settings.texture_cap,
                                            c,
                                            c.to_string(),
                                        );
                                    }
                                });
                            ui.end_row();
                            ui.label("Pages kept in memory");
                            ui.add(
                                egui::DragValue::new(&mut self.settings.page_cache).range(3..=20),
                            );
                            ui.end_row();
                            ui.label("Autosave after (ms)");
                            ui.add(
                                egui::DragValue::new(&mut self.settings.autosave_ms)
                                    .range(100..=10_000)
                                    .speed(50),
                            );
                            ui.end_row();
                        });
                    ui.label(
                        RichText::new("Worker changes apply after a restart.")
                            .color(muted())
                            .size(12.0),
                    );
                    section(ui, "T", "Default font for new boxes", None);
                    egui::ComboBox::from_id_salt("deffont")
                        .width(300.0)
                        .selected_text(fonts.name_of(&self.settings.default_font))
                        .show_ui(ui, |ui| {
                            egui::ScrollArea::vertical()
                                .max_height(260.0)
                                .show(ui, |ui| {
                                    ui.selectable_value(
                                        &mut self.settings.default_font,
                                        String::new(),
                                        "Default",
                                    );
                                    for (n, p) in fonts.list.iter().take(400) {
                                        ui.selectable_value(
                                            &mut self.settings.default_font,
                                            p.clone(),
                                            n,
                                        );
                                    }
                                });
                        });
                });
            if change_lib {
                self.pick_library();
            }
            if self.settings != before {
                self.settings.save();
            }
            self.settings_open = openw;
        }
        if self.about_open {
            let mut openw = true;
            egui::Window::new("About Inkwell").open(&mut openw).show(ctx, |ui| {
                ui.label("A light manga translation workbench, tuned for laptops.");
                ui.label("Your work saves itself a moment after every change; after a crash it reopens where you were.");
                ui.add_space(6.0);
                egui::Grid::new("keys").num_columns(2).show(ui, |ui| {
                    for (k, d) in [
                        ("V / T / H", "Select · Text box · Move page"),
                        ("← → / PgUp PgDn", "Previous / next page"),
                        ("Ctrl+Z / Ctrl+Shift+Z", "Undo / redo"),
                        ("Del", "Delete the selected box"),
                        ("Ctrl+scroll, pinch", "Zoom"),
                        ("Ctrl+0", "Fit the page"),
                        ("Double-click a box", "Edit its translation"),
                    ] {
                        ui.label(RichText::new(k).strong());
                        ui.label(d);
                        ui.end_row();
                    }
                });
            });
            self.about_open = openw;
        }
    }

    fn overlays(&mut self, ctx: &egui::Context) {
        // Drag-and-drop hint.
        let hovering = ctx.input(|i| i.raw.hovered_files.len());
        if hovering > 0 {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Tooltip,
                egui::Id::new("drop"),
            ));
            painter.rect_filled(
                screen,
                0.0,
                rose_a(40),
            );
            let card = Rect::from_center_size(screen.center(), vec2(460.0, 130.0));
            painter.rect_filled(card, 18.0, surface());
            painter.rect_stroke(card, 18.0, Stroke::new(2.0_f32, rose()), StrokeKind::Inside);
            painter.text(
                card.center() - vec2(0.0, 18.0),
                Align2::CENTER_CENTER,
                format!("Drop {hovering} file(s) to import"),
                FontId::proportional(20.0),
                text_c(),
            );
            painter.text(
                card.center() + vec2(0.0, 16.0),
                Align2::CENTER_CENTER,
                "Comics get their own folders · pictures join the open comic",
                FontId::proportional(13.0),
                muted(),
            );
        }
        // Toasts.
        self.toasts
            .retain(|(_, err, t)| t.elapsed() < Duration::from_secs(if *err { 10 } else { 5 }));
        if !self.toasts.is_empty() {
            egui::Area::new(egui::Id::new("toasts"))
                .anchor(Align2::RIGHT_BOTTOM, vec2(-16.0, -48.0))
                .order(egui::Order::Foreground)
                .show(ctx, |ui| {
                    for (text, err, _) in &self.toasts {
                        egui::Frame::new()
                            .fill(if *err {
                                err_bg()
                            } else {
                                surface()
                            })
                            .stroke(Stroke::new(1.0_f32, if *err { rose() } else { border() }))
                            .corner_radius(CornerRadius::same(10))
                            .inner_margin(egui::Margin::symmetric(12, 8))
                            .show(ui, |ui| {
                                ui.set_max_width(380.0);
                                ui.label(RichText::new(text).color(if *err {
                                    err_text()
                                } else {
                                    text_c()
                                }));
                            });
                    }
                });
            ctx.request_repaint_after(Duration::from_millis(500));
        }
    }

    fn screenshot_step(&mut self, ctx: &egui::Context) {
        let Some(path) = self.screenshot.clone() else {
            return;
        };
        self.frames += 1;
        ctx.request_repaint();
        if self.frames >= 120 && !self.shot_sent {
            self.shot_sent = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::Screenshot(egui::UserData::default()));
        }
        let shot = ctx.input(|i| {
            i.raw.events.iter().find_map(|e| match e {
                egui::Event::Screenshot { image, .. } => Some(image.clone()),
                _ => None,
            })
        });
        if let Some(img) = shot {
            let [w, h] = img.size;
            let bytes: Vec<u8> = img.pixels.iter().flat_map(|c| c.to_array()).collect();
            if let Some(buf) = image::RgbaImage::from_raw(w as u32, h as u32, bytes) {
                if let Some(dir) = path.parent() {
                    let _ = std::fs::create_dir_all(dir);
                }
                if let Err(e) = buf.save(&path) {
                    eprintln!("can't save the screenshot: {e}");
                }
            }
            ctx.send_viewport_cmd(egui::ViewportCommand::Close);
        }
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        let want_dark = match self.settings.theme.as_str() {
            "dark" => true,
            "light" => false,
            _ => ctx.system_theme() == Some(egui::Theme::Dark),
        };
        if self.applied_dark != Some(want_dark) {
            self.applied_dark = Some(want_dark);
            apply_theme(ctx, want_dark);
            if let Some(o) = &mut self.open {
                o.block_tex.clear();
            }
        }
        self.screenshot_step(ctx);
        self.poll(ctx);
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.take_paths(dropped);
        }
        self.shortcuts(ctx);

        self.menu_bar(ctx);
        if self.open.is_some() {
            self.pages_panel(ctx);
            self.right_panel(ctx);
            self.run_bar(ctx);
            self.status_bar(ctx);
            self.canvas(ctx);
        } else {
            self.start_view(ctx);
        }
        self.windows(ctx);
        self.overlays(ctx);

        // Autosave a moment after the last edit.
        if let Some(o) = &mut self.open {
            if o.dirty {
                let wait = Duration::from_millis(self.settings.autosave_ms);
                let since = o.last_change.elapsed();
                if since >= wait && o.drag.is_none() {
                    o.save();
                } else {
                    ctx.request_repaint_after(
                        wait.saturating_sub(since).max(Duration::from_millis(50)),
                    );
                }
            }
        }
        if self.running.is_some()
            || self.exporting.is_some()
            || self
                .imports
                .iter()
                .any(|r| matches!(r.state, ImportState::Running))
        {
            ctx.request_repaint_after(Duration::from_millis(250));
        }
    }

    fn clear_color(&self, _visuals: &egui::Visuals) -> [f32; 4] {
        // Gaps between panels read as light dividers.
        egui::Rgba::from(border()).to_array()
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(r) = &self.running {
            r.cancel.store(true, Ordering::Relaxed);
        }
        if let Some(o) = &mut self.open {
            o.save();
        }
        if self.screenshot.is_none() {
            self.save_session();
            self.session.clean_exit = true;
            self.session.save();
        }
    }
}
