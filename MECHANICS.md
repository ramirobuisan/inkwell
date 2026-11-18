# Mechanics sheet

The target (`koharu-0.85.2.zip`) is the **source archive** of Koharu 0.85.2; the native
tools refuse it (target `t_b7404cce40fa`: "A ZIP archive is not native machine code").
The **compiled** Koharu 0.85.2 is installed on this laptop as `/usr/bin/koharu` (package
`koharu-bin 0.85.2-1`, ELF arm64, not stripped, sha256 `e196b586…7261e`, target
`t_e196b586c4a7`). It was opened with `core_target_open` and its functions decompiled with
the built-in decompiler (Ghidra isn't installed, so pseudo-C is register-level). Each value
below cites the compiled function address where one was found, and the source file:line
it matches. Values marked *estimated* were looked for and not found, or belong to parts
the original does with GPU models that this rebuild replaces.

## Decompiled from /usr/bin/koharu

| Rule | Value in the compiled program | Address | Rebuild |
|---|---|---|---|
| Archive members skipped | glob set `**/__MACOSX/**`, `**/._*`, `**/.DS_Store`, `**/Thumbs.db`, `**/desktop.ini` (string data used by `koharu_desktop::commands::import::METADATA`) | data 0x4a7d71f, builder 0x9302e8 | `import::is_metadata` (same five patterns) |
| Page order | `in_name_order`: empty list → error; otherwise stable sort of (name, bytes) pairs, 48-byte elements, insertion sort below 21 items | 0x91e568 | `natural_cmp` sort |
| Import errors | "`<path>` holds no images" (0x3a58ad2); "`<path>` isn't an image, an archive, or a PDF" (0x3a58768) | rodata | same wording ("…an image or an archive", since PDF is left out) |
| Size search | `koharu_renderer::text::fit::halve`: bisection loop of **8** steps, midpoint `(lo + hi) × 0.5` (`fmov s11, #0.5`), keeps the already placed text when the scale is `1.0` (`fmov s12, #1.0`) | 0x16fc5ec (also 0x16fc894, 0xcfbf04) | `textfit::HALVINGS = 8` |
| PNG export | `koharu_desktop::commands::export::png`: rasterize, then `write_to(…, ImageFormat::Png)` with no options, i.e. the PNG encoder's **default** compression | 0x91b2ec | `CompressionType::Default` (was Fast) |
| Undo depth | inlined into the scene session; no separate function found in the first 50 000 functions | — | 20, from source `koharu-scene/src/session.rs:18` |
| Line height 1.2, shrink floor 0.5, balloon inset 1 em | constants inlined into the renderer | `fit::halve` shows the 0.5/1.0 pair | from source (see "Text fitting") |

## Import (crates/koharu-desktop/src/commands/import.rs)

| Rule | Value | Source |
|---|---|---|
| Page files accepted | png, jpg, jpeg, webp, and archives zip, cbz (original also rar, cbr, pdf) | `EXTENSIONS` |
| Archive members that are skipped | `__MACOSX/`, `._*`, `.DS_Store`, `Thumbs.db`, `desktop.ini` | `METADATA` |
| A member is a page when | its extension is PNG, JPEG or WebP | `raster()` |
| Page order | name order, numbers compared by value (alphanumeric sort) | `in_name_order()` |
| An archive with no images | refused: "holds no images" | `in_name_order()` |
| Imports in parallel | each file read on its own thread, pages appended after the last page | `import()` (rayon) |

**Rebuild change asked by the person:** every dropped CBZ/ZIP becomes its *own project folder*
in the library (`<library>/<comic name>/pages/0001.jpg…`), instead of the original's single
`.khr` SQLite file per project.

## Projects (crates/koharu-desktop/src/library.rs, README §3)

| Rule | Value | Source |
|---|---|---|
| Projects list order | last changed first; ties in name order, numbers by value | `Library::list` |
| Empty project name | refused | `Library::path` |
| A name that already exists | refused for create; rebuild adds " (2)", " (3)"… for dropped comics | `Library::create` |

## Crash safety (crates/koharu-scene/src/store.rs, tests/edits.rs)

The original commits every edit to SQLite in WAL mode immediately, so a crash leaves
the main file plus its `-wal` journal. The rebuild matches "every edit is kept" with:

- project state written to `project.json` by write-to-temp → fsync → rename (atomic), the
  previous version kept as `project.json.bak`; loading falls back to `.tmp` then `.bak`;
- save after **500 ms** of quiet after an edit (*estimated*; original: immediate per edit);
- a panic hook that writes the last unsaved state before the program dies;
- `session.json` with a `clean_exit` flag: after an unclean exit the last project and page
  are reopened and a banner says the work was restored;
- an `import.json` marker in a comic's folder while it is being unpacked; an interrupted
  import resumes on next start, skipping pages already written.

## Undo (crates/koharu-desktop/README.md §2)

- An edit is one undo step; history keeps **20** steps, dropping the oldest (koharu-scene `session.rs:18`, `STEPS = 20`).

## Translation (crates/koharu-pipeline/src/stages/translate/mod.rs)

| Rule | Value | Source |
|---|---|---|
| Prompt | translate each page's texts into the target; texts given as a JSON object numbered in reading order; translate only values, keep keys; output only the JSON | `system()` |
| User instructions | appended after the template on a blank line | `system()` |
| Page image long side sent to vision models | 1280 px | `SIGHT` (rebuild sends text only) |
| Earlier pages as context | conversation with summaries | conversation.rs; rebuild keeps the last **2** pages (*estimated*) |
| Temperature | **not sent**: the translate stage sets none, so the model's default applies (llama.cpp path: 1.0) | `stages/translate/mod.rs` (no sampling set), koharu-llm `local/engine.rs:786` (`unwrap_or(1.0)`) |

## Export (crates/koharu-desktop/src/commands/export.rs)

| Rule | Value | Source |
|---|---|---|
| Formats | PNG files in a folder, or a CBZ of PNGs (original also layered PSD) | `ExportFormat` |
| Page file names | page number zero-padded to max(3, digits of page count), e.g. `001.png` | `write()` |
| CBZ compression | stored (no deflate: PNGs are already compressed) | `write()` |

## Text fitting (crates/koharu-renderer/src/text/fit.rs)

| Rule | Value | Source |
|---|---|---|
| Line height | **1.2** × font height | koharu-renderer `text/style.rs:73` (`line_height.unwrap_or(1.2)`) |
| A set size that overflows | shrinks, whole words only, to the largest scale that fits, **no smaller than 0.5** of the set size | `text/fit.rs:64-75` (`floor = 0.5`, `largest_fit`) |
| Still overflowing at that floor | a balloon's margin narrows to **half**, then words break | `text/fit.rs:79-90` (`inset / 2.0`) |
| Balloon margin | **1 em** (one letter width) of air from the outline | koharu-scene `values.rs:296` (`Frame::DEFAULT_INSET = 1.0`) |
| Auto (best-fit) size | grows to the largest size that fits; ceiling 256× scale | `text/fit.rs:23` (`CEILING`), `grow()` |

Rebuild: the same order of rules (`app/src/textfit.rs` `fit()`); balloon room is the
rectangle inscribed in the oval after the 1 em inset; auto size is searched from 6 px to
min(box height, 160 px) (*estimated bounds*: 256× of a page font is far outside any box).

## Look (packages/ui/src/styles/*.css, packages/docs/screenshot.png)

| Token | Value | Source |
|---|---|---|
| Primary | rose-500 `oklch(0.645 0.246 16.439)` ≈ #f43f5e | `--primary` |
| Accent | rose-100 `oklch(0.941 0.03 12.58)` ≈ #ffe4e6 | `--accent` |
| Background | white; sidebar ≈ #fafafa; border ≈ #e5e5e5 | `--background`, `--sidebar`, `--border` |
| Layout | Pages list left (thumbnail, name, layers, size), canvas centre with tool rail, Properties + Layers right, zoom bar bottom, model picker + Run top | screenshot.png |

## Laptop tuning (this machine: Apple M2 MacBook Air, 8 cores (4 performance), 16 GB, Linux)

All *estimated* choices for this machine, read at start from the system:

- background image workers = half the cores, at least 2 (4 here: the performance cores);
- page pictures shown at most **4096 px** on the long side with ≥ 12 GB memory, else 2048;
- at most **6** full pages kept in graphics memory (else 4), next/previous page preloaded;
- thumbnails 160 px, cached on disk in the comic's `thumbs/` folder;
- the window only redraws when something changes (saves battery);
- export encodes several pages at once (one per worker) with fast PNG compression.

## AI parts replaced

The original runs detection, OCR, inpainting and LLM models on the GPU. Here:

- **Find text / Read text**: the system's Tesseract (`tesseract … tsv` for boxes, plain
  output for one box). Japanese needs the `jpn` / `jpn_vert` language packs installed.
- **Translate**: any OpenAI-compatible chat endpoint (Ollama default
  `http://localhost:11434/v1`), model and key set in Settings.
- **Clean up** (inpainting): fills the box with the bubble's own colour, sampled as the
  median colour along the box edge (*estimated replacement*).

## Features added after comparing with the original

| Feature | Original | Rebuild |
|---|---|---|
| Story | `StoryPanel.tsx`: every page's texts in page order, each page's in reading order, numbered on the canvas while it shows; editing a field edits the box; focusing it shows its page and selects the box | Story tab, same behaviour; "Renumber right-to-left" sorts one page in manga order (*estimated* rule: rows of 0.6 × the tallest box, right to left) |
| History | `HistoryWindow.tsx`, `commands/scene.rs` `go_to(step)`: a Photoshop-style list, 0 = as opened | History window (View → History…); steps labelled from what changed; a click goes to that step |
| Brush / eraser / picker | `set_brush`, `ColorSampling.tsx`, the "Cleanup" layer | A cleanup layer per page (`paint/NNNN.png`), round brush with a half-pixel soft edge, eraser, picker sampling the cleanup then the artwork; each stroke is one undo step |
| RAR / CBR / PDF import | `import.rs`: `rars`, `hayro`, PDF at 300 DPI (`PDF_SCALE`) | the same libraries (rars 0.12, hayro 0.8), PDF at 300/72 scale |
| Direction | `values.rs:199` `Direction { Horizontal, Vertical }`, Auto in the type bar | Auto / Across / Down; Auto = down when most letters are CJK and the box is 1.2× taller than wide (*estimated*); upright punctuation forms; closing marks never start a column |
| Right-to-left | `text/arrange.rs`: runs ordered by Unicode bidi levels | `unicode-bidi` reordering per line; Arabic letters joined with `ar-reshaper` (the original uses a full text shaper) |
| PSD | `koharu-psd`: layered PSD, at most 30,000 px a side, editable type layers | layered PSD (artwork, Cleanup, each box's cover and text as named layers, hidden boxes hidden), same size limit; texts are pixel layers, not editable type |
| Assistant | `koharu-agent`: get_project, get_page, get_layer, edit (one undo step), run, wait, stop, view_page; turns capped | Ask tab: get_project, get_page, show_page, edit (one undo step, applied whole or not at all); 8 model calls per message (*estimated* cap); no run/wait/stop/view_page |

## Koharu's models (vendored, run as-is)

| Step | Original | Rebuild |
|---|---|---|
| Find text | `koharu-ml` `KoharuLayoutHighres::inference` + `Detected::layout`: lettering in reading order, glyph masks, rooms in bubbles; thresholds onomatopoeia 0.4, bubble 0.4 (`ONOMATOPOEIA_THRESHOLD`, `BUBBLE_THRESHOLD`) | the same calls; a box is the lettering's bbox grown to its room in the bubble |
| Read text | `MangaOcrAr::inference` on crops | the same, one crop per box |
| Clean up | `Lama::inference` (IOPaint defaults: whole page up to 800 px, else crops with a 128 px margin, `WHOLE`) under the glyph mask | the same, mask = glyphs inside the boxes grown by 7 px (*estimated* margin), result kept on the cleanup layer as one undo step |
| Translate | local GGUF through llama.cpp (Index-Translate) or hosted providers | Ollama / OpenAI-compatible servers (unchanged) |
