# Homebrew's rustup is keg-only, so cargo may not be on PATH. Appending rather
# than prepending leaves an existing toolchain in charge.
export PATH := $(PATH):/opt/homebrew/opt/rustup/bin

# discourse and mastodon are gitless source drops and only partially bundled;
# script/bench.py stages them and reports the conditions (DEC-001).
CORPORA ?= ~/code/lib/ruby/rails ~/code/lib/ruby/discourse ~/code/lib/ruby/mastodon ~/code/lib/ruby/ruby ~/code/lib/ruby/graph_weaver

.PHONY: check build release bench dogfood

## the commit gate: fmt, clippy, tests
check:
	@script/check.sh

build:
	@cargo build

release:
	@cargo build --release

## reproduce the numbers in docs/ARCHITECTURE.md
bench: release
	@script/bench.py $(CORPORA)

## feel the tool on real code — the practice that has already found two defects
## a unit test could not. REPO= picks the target, Q= the name to look up.
REPO ?= ~/code/lib/ruby/rails
dogfood: release
	@TREKR_DB=/tmp/trekr-dogfood.db ./target/release/trekr --index $(REPO)
ifdef Q
	@cd $(REPO) && TREKR_DB=/tmp/trekr-dogfood.db $(CURDIR)/target/release/trekr --refs $(Q)
endif
ifdef F
	@cd $(REPO) && TREKR_DB=/tmp/trekr-dogfood.db $(CURDIR)/target/release/trekr --symbols $(F)
endif
