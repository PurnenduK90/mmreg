
CROSS_TARGETS := $(shell cat targets.txt | grep -v '^#' | grep -v '^$$')


.PHONY: all $(CROSS_TARGETS) clean check lint doc


all: $(CROSS_TARGETS)


$(CROSS_TARGETS):
	cross build --release --target $@


check:
	cargo check --all-features
	# cargo test --all-features  # TODO: Enable when tests are properly set up


lint:
	cargo fmt --all -- --check
	cargo clippy --all-features -- -D warnings


doc:
	cargo doc --no-deps --open


clean:
	cargo clean
