#!/usr/bin/env bash
# A gold set from a gem's own spec suite, scored — the widget_shop harness
# pointed at real code (script/trace_gold.rb, script/gold.py).
#
#   script/gold_gem.sh ~/code/lib/ruby/graph_weaver
#   make gold-gem GEM=~/code/lib/ruby/graph_weaver
#
# The repo is copied first and everything happens in the copy: its suite runs
# there, trekr indexes it into a store of its own, and the original is only
# read. The suite has to pass offline with the gems already installed; a red
# suite still yields a gold set, and says so.
#
# Environment: APP_SAMPLE (default 600) and SAMPLE (gem floor, default 300)
# bound the scoring, one process per site; TREKR_BIN picks the build; WORK
# keeps the copy, gold set and verdicts somewhere named instead of a temp dir.
set -euo pipefail

here="$(cd "$(dirname "$0")" && pwd)"
src="$(cd "${1:?usage: script/gold_gem.sh REPO}" && pwd)"
name="$(basename "$src")"
work="${WORK:-$(mktemp -d "${TMPDIR:-/tmp}/trekr-gold-$name.XXXXXX")}"
mkdir -p "$work"
work="$(cd "$work" && pwd -P)"
copy="$work/$name"
bin="${TREKR_BIN:-$here/../target/release/trekr}"
[ -x "$bin" ] || { echo "gold_gem: build first (make release)" >&2; exit 1; }

rsync -a --delete --exclude tmp/ --exclude log/ --exclude coverage/ --exclude node_modules/ \
  "$src/" "$copy/"

export TREKR_DB="${TREKR_DB:-$work/trekr.db}" TREKR_USAGE=off
gold="$work/gold.ndjson"
echo "gold_gem: tracing $name's suite in $copy" >&2
(
  cd "$copy"
  BUNDLE_GEMFILE="$copy/Gemfile" TREKR_GOLD="$gold" TREKR_MAX="${TREKR_MAX:-200000}" \
    TREKR_EXERCISE="$here/exercise_rspec.rb" bundle exec ruby "$here/trace_gold.rb" >/dev/null
)
"$bin" --index "$copy" >/dev/null
APP_SAMPLE="${APP_SAMPLE:-600}" SAMPLE="${SAMPLE:-300}" TREKR_BIN="$bin" GOLD_ROOT="$copy" \
  VERDICTS="$work/verdicts.ndjson" "$here/gold.py" "$gold"
echo "gold_gem: gold set, verdicts and copy in $work" >&2
