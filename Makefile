.DEFAULT_GOAL := build

.PHONY: build test test-unit test-integration test-stress check clippy install update uninstall uninstall-purge

build:
	cargo build --release

test:
	cargo test

test-unit:
	cargo test --test unit_suite

test-integration:
	cargo test --test integration_suite -- --ignored

test-stress:
	cargo test --test stress_suite -- --ignored

check:
	cargo fmt --check
	cargo check

clippy:
	cargo clippy --all-targets -- -D warnings

install:
	./scripts/install.sh

update:
	./scripts/update.sh

uninstall:
	./scripts/uninstall.sh

uninstall-purge:
	./scripts/uninstall.sh --purge-data
