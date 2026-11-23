#!/usr/bin/env bash
# Regenerate and verify the generated reference docs; build the book when
# mdbook is installed. Run this after changing flags, tools, providers,
# slash commands, or any documented behaviour.
set -euo pipefail
cd "$(dirname "$0")/.."

UPDATE_DOCS=1 cargo test --quiet doc_sync
cargo test --quiet doc_sync

if command -v mdbook >/dev/null 2>&1; then
  mdbook build docs
else
  echo "mdbook not found; skipped the book build (cargo install mdbook)"
fi
echo "docs in sync"
