SHELL := /bin/sh

CARGO ?= cargo
BIN := ghost-complete
PACKAGE := ghost-complete
CRATE_PATH := crates/ghost-complete

.PHONY: help build release install install-shell doctor status validate-specs check test clippy fmt clean

help:
	@printf '%s\n' \
		'Targets:' \
		'  build          Build the workspace in debug mode' \
		'  release        Build the ghost-complete binary in release mode' \
		'  install        Install the local ghost-complete binary to ~/.cargo/bin' \
		'  install-shell  Install shell integration with the installed binary' \
		'  doctor         Run ghost-complete doctor from PATH' \
		'  status         Show ghost-complete status from PATH' \
		'  validate-specs Validate completion specs' \
		'  check          Run cargo check --all-targets' \
		'  test           Run cargo test' \
		'  clippy         Run clippy with warnings denied' \
		'  fmt            Check rustfmt formatting' \
		'  clean          Remove Cargo build artifacts'

build:
	$(CARGO) build

release:
	$(CARGO) build --release -p $(PACKAGE)

install:
	$(CARGO) install --path $(CRATE_PATH) --locked --force

install-shell:
	$(BIN) install

doctor:
	$(BIN) doctor

status:
	$(BIN) status

validate-specs:
	$(CARGO) run -- validate-specs

check:
	$(CARGO) check --all-targets

test:
	$(CARGO) test

clippy:
	$(CARGO) clippy --all-targets -- -D warnings

fmt:
	$(CARGO) fmt --check

clean:
	$(CARGO) clean
