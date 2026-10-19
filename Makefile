.PHONY: test release

VERSION ?= 0.1.0

test:
	cargo fmt --check
	cargo clippy --workspace --all-targets --offline -- -D warnings
	# Auth integration tests share an in-process limiter; serialize the suite
	# so this required offline command is deterministic on every machine.
	cargo test --workspace --offline -- --test-threads=1
	npm --prefix apps/web run build


release:
	mkdir -p release
	git archive --format=tar.gz --prefix=pumpkin-manager-$(VERSION)/ \
		-o release/pumpkin-manager-$(VERSION)-source.tar.gz HEAD
	sha256sum release/pumpkin-manager-$(VERSION)-source.tar.gz \
		> release/pumpkin-manager-$(VERSION)-source.tar.gz.sha256
	@echo "Wrote release/pumpkin-manager-$(VERSION)-source.tar.gz"
