.PHONY: check fmt clippy test build

check: fmt clippy test

fmt:
	cargo fmt --check

clippy:
	cargo clippy --all-targets -- -D warnings

test:
	cargo test

build:
	cargo build --release
