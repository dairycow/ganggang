#!/usr/bin/env bash
# Commit gate: format check, clippy, tests. Runs from the repository root.
set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

echo "pre-commit: fmt"
cargo fmt --all -- --check || { echo "pre-commit: FAILED fmt"; exit 1; }

echo "pre-commit: clippy"
cargo clippy --all-targets -- -D warnings || { echo "pre-commit: FAILED clippy"; exit 1; }

echo "pre-commit: test"
cargo test --quiet || { echo "pre-commit: FAILED test"; exit 1; }
