#!/usr/bin/env bash
# The commit gate: format, lint, test. CI runs the same three.
set -euo pipefail

# cargo is keg-only on this machine.
export PATH="/opt/homebrew/opt/rustup/bin:$PATH"

cd "$(dirname "$0")/.."

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
