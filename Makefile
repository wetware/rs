# Wetware build system
#
# Builds std/ components and places artifacts at <component>/boot/main.wasm.
# Publish to IPFS with: ww push std/

WASM_TARGET := wasm32-wasip2

.PHONY: all host std kernel shell examples chess clean run-kernel
.PHONY: podman-build podman-run podman-clean podman-dev

all: std examples host

# --- Host --------------------------------------------------------------------

host:
	cargo build --release

# --- Std components ----------------------------------------------------------

std: kernel shell

kernel:
	cargo build -p kernel --target $(WASM_TARGET) --release
	@mkdir -p crates/kernel/bin
	cp target/$(WASM_TARGET)/release/kernel.wasm crates/kernel/bin/main.wasm

shell:
	cargo build -p shell --target $(WASM_TARGET) --release
	@mkdir -p std/shell/boot
	cp target/$(WASM_TARGET)/release/shell.wasm std/shell/boot/main.wasm

# --- Examples ----------------------------------------------------------------

examples: chess

chess:
	cargo build -p chess --target $(WASM_TARGET) --release
	@mkdir -p examples/chess/bin
	cp target/$(WASM_TARGET)/release/chess.wasm examples/chess/bin/main.wasm

# --- Run ---------------------------------------------------------------------

run-kernel: kernel
	cargo run -- run crates/kernel

# --- Clean -------------------------------------------------------------------

clean:
	cargo clean
	rm -f crates/kernel/bin/main.wasm
	rm -f std/shell/boot/main.wasm
	rm -f examples/chess/bin/main.wasm

# --- Podman ------------------------------------------------------------------

podman-build:
	podman build -t wetware:latest .

podman-run:
	podman run --rm -it wetware:latest

podman-clean:
	podman rmi wetware:latest || true
	podman system prune -f

podman-dev: podman-build
	podman run --rm -it -v $(PWD):/app wetware:latest
