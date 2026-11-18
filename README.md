# Inkwell

A light manga translation workbench for Linux laptops, written in Rust with egui.

- **Drag and drop comics**: drop `.cbz` / `.zip` files (or a folder of pictures) on the window. Each one becomes a new project in its own folder in your library (`~/Documents/Inkwell Library` by default) and opens.
- **Crash-safe**: every change is saved half a second later with atomic writes and a backup; after a crash the last comic and page reopen. Imports interrupted by a crash resume on the next start.
- **Text boxes**: draw boxes on a page, type translations, pick font, size (or auto-fit), colour, outline and alignment, and cover the original lettering.
- **Helpers**: find and read text with [Tesseract](https://github.com/tesseract-ocr/tesseract) (install `tesseract-data-jpn` for Japanese); translate through any OpenAI-compatible chat server such as Ollama.
- **Export** a translated CBZ or PNG files.

## Build

```sh
cargo build --release
./target/release/inkwell
```

The first normal start adds **Inkwell** to your desktop's app menu (or run `inkwell --install`,
or Help → "Add Inkwell to the app menu"), so afterwards you can start it like any other app.

Light and dark mode: the sun/moon button in the top-right corner, or the Theme menu
(Light, Dark, Same as system).

`inkwell --screenshot out.png` draws ~120 frames, saves a picture of the window and exits.
`inkwell --export-cbz <project folder>` exports without opening the window.

See `MECHANICS.md` for the rules this app follows.
