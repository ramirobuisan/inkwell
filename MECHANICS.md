# Mechanics sheet

The target (`koharu-0.85.2.zip`) is a **source archive**, not a compiled program, so every
value below was read from the original's own source files rather than from decompiled code.
"Source" names the file inside the archive. Values marked *estimated* were looked for and
not found (or belong to parts the original does with GPU models this rebuild replaces).

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

- An edit is one undo step. Rebuild keeps **100** steps (*estimated*).

## Translation (crates/koharu-pipeline/src/stages/translate/mod.rs)

| Rule | Value | Source |
|---|---|---|
| Prompt | translate each page's texts into the target; texts given as a JSON object numbered in reading order; translate only values, keep keys; output only the JSON | `system()` |
| User instructions | appended after the template on a blank line | `system()` |
| Page image long side sent to vision models | 1280 px | `SIGHT` (rebuild sends text only) |
| Earlier pages as context | conversation with summaries | conversation.rs; rebuild keeps the last **2** pages (*estimated*) |
| Temperature | 0.2 (*estimated*; not set in the original's translate stage) | — |

## Export (crates/koharu-desktop/src/commands/export.rs)

| Rule | Value | Source |
|---|---|---|
| Formats | PNG files in a folder, or a CBZ of PNGs (original also layered PSD) | `ExportFormat` |
| Page file names | page number zero-padded to max(3, digits of page count), e.g. `001.png` | `write()` |
| CBZ compression | stored (no deflate: PNGs are already compressed) | `write()` |

## Text fitting (crates/koharu-renderer/src/text/fit.rs)

- Auto size grows/shrinks text to the largest size whose whole words fit the box; a word is
  only split when it cannot fit at the smallest size. Rebuild: binary search from 6 px up to
  min(box height, 160 px) (*estimated bounds*; original ceiling 256× scale `CEILING`).
- Line height 1.15 × font height (*estimated*).

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
