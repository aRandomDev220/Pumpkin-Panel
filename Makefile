.PHONY: test

test:
	cargo fmt --check
	cargo clippy --workspace --all-targets --offline -- -D warnings
	# Auth integration tests share an in-process limiter; serialize the suite
	# so this required offline command is deterministic on every machine.
	cargo test --workspace --offline -- --test-threads=1
	npm --prefix apps/web run build
