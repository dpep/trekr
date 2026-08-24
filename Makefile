# cargo is keg-only on this machine (see CLAUDE.md).
export PATH := /opt/homebrew/opt/rustup/bin:$(PATH)

# discourse has no .git locally; script/bench.py stages it (DEC-001).
CORPORA ?= ~/code/lib/ruby/rails ~/code/lib/ruby/discourse ~/code/lib/ruby/ruby

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
