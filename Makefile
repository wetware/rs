.PHONY: clean examples
WASM_EXAMPLES := examples/wasm

all: clean build examples

build:
	cargo build --release

clean:
	rm -rf target

examples:
	@cd $(WASM_EXAMPLES)/echo; make
