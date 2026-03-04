test: build lint
	cargo test

build:
	cargo build

lint:
	cargo clippy -- -D warnings
