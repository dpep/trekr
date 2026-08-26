#!/usr/bin/env bash
# The commit gate: format, lint, test. CI runs the same three.
set -euo pipefail

cd "$(dirname "$0")/.."

# Homebrew's rustup is keg-only, so cargo may not be on PATH. Only go looking
# if it isn't already — otherwise this would shadow a perfectly good toolchain.
if ! command -v cargo >/dev/null 2>&1; then
  export PATH="/opt/homebrew/opt/rustup/bin:$PATH"
fi
command -v cargo >/dev/null 2>&1 || {
  echo "check: cargo not found (tried /opt/homebrew/opt/rustup/bin)" >&2
  exit 1
}

cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
