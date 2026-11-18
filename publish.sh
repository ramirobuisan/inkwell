#!/usr/bin/env bash
# Uploads Inkwell to a public GitHub repository named "inkwell" on the GitHub account
# the `gh` command is signed in to. Run it from a terminal: ./publish.sh
set -euo pipefail
cd "$(dirname "$0")"
if git remote get-url origin >/dev/null 2>&1; then
    git push -u origin main
else
    gh repo create inkwell --public --source . --remote origin --push \
        --description "Manga translation workbench in Rust/egui: drag-and-drop comics into projects, crash-safe autosave, Koharu's own detection/OCR/LaMa models, translation via Ollama, CBZ/PNG/PSD export"
fi
echo "Published: $(gh repo view --json url -q .url)"
