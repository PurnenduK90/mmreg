
CROSS_TARGETS := $(shell cat targets.txt | grep -v '^#' | grep -v '^$$')


.PHONY: all $(CROSS_TARGETS) clean


all: $(CROSS_TARGETS)


$(CROSS_TARGETS):
	cross build --release --target $@


clean:
	cargo clean
