.PHONY: test

test:
	cargo fmt --check
	cargo clippy --workspace --all-targets --offline -- -D warnings
	cargo test --workspace --offline
	npm --prefix apps/web run build
