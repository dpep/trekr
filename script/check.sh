#!/usr/bin/env bash
# The commit gate: format, lint, test — then the VS Code extension's unit
# tests. CI runs the first three.
#
#   script/check.sh          # the commit gate
#   script/check.sh --e2e    # plus the extension's e2e suite in a real VS Code
#                            # (also TREKR_CHECK_E2E=1). Run it before a release.
set -euo pipefail

cd "$(dirname "$0")/.."

e2e="${TREKR_CHECK_E2E:-}"
for arg in "$@"; do
  case "$arg" in
    --e2e) e2e=1 ;;
    *) echo "check: unknown argument $arg (only --e2e)" >&2; exit 64 ;;
  esac
done

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

# The extension's tests went red unnoticed once, because nothing ran them.
# No npm is the one reason to skip, and it is said out loud.
if ! command -v npm >/dev/null 2>&1; then
  echo "check: npm not found — SKIPPED the VS Code extension's tests" >&2
  exit 0
fi
if [ ! -d editors/vscode/node_modules ]; then
  npm --prefix editors/vscode ci --no-audit --no-fund
fi
npm --prefix editors/vscode test
if [ -n "$e2e" ]; then
  # Against target/debug/trekr, which `cargo test` just built.
  npm --prefix editors/vscode run test:e2e
fi
