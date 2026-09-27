# Homebrew's rustup is keg-only, so cargo may not be on PATH. Appending rather
# than prepending leaves an existing toolchain in charge.
export PATH := $(PATH):/opt/homebrew/opt/rustup/bin

# discourse and mastodon are gitless source drops and only partially bundled;
# script/bench.py stages them and reports the conditions (DEC-001).
CORPORA ?= ~/code/lib/ruby/rails ~/code/lib/ruby/discourse ~/code/lib/ruby/mastodon ~/code/lib/ruby/ruby ~/code/lib/ruby/graph_weaver

.PHONY: check build release bench dogfood vscode-test clicks gold-gem

## the commit gate: fmt, clippy, tests, the VS Code extension's unit tests
check:
	@script/check.sh

## the VS Code extension's unit tests, then its e2e suite in a real VS Code
## against the debug build. `script/check.sh --e2e` runs both after the gate.
vscode-test: build
	@test -d editors/vscode/node_modules || npm --prefix editors/vscode ci --no-audit --no-fund
	@npm --prefix editors/vscode test
	@npm --prefix editors/vscode run test:e2e

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

## replay editor clicks — definition and hover on every name in a sample of
## each repo's files — and bucket what came back empty or unsure. Point it at
## copies: REPOS="/path/copy-a /path/copy-b" (script/clicks.py).
clicks: release
	@script/clicks.py $(REPOS)

## a gold set from a gem's own spec suite, traced and scored in a copy of it:
## make gold-gem GEM=~/code/lib/ruby/graph_weaver (script/gold_gem.sh).
gold-gem: release
	@script/gold_gem.sh $(GEM)

