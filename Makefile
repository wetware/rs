# Wetware build system
#
# Builds std/ components and places artifacts at <component>/boot/main.wasm.
# Publish to IPFS with: ww push std/

WASM_TARGET := wasm32-wasip2

.PHONY: all host std kernel shell clean run-kernel
.PHONY: podman-build podman-run podman-clean podman-dev

all: std host

# --- Host --------------------------------------------------------------------

host:
	cargo build --release

# --- Std components ----------------------------------------------------------

std: kernel shell

kernel:
	cargo build -p kernel --target $(WASM_TARGET) --release
	@mkdir -p std/kernel/boot
	cp target/$(WASM_TARGET)/release/kernel.wasm std/kernel/boot/main.wasm

shell:
	cargo build -p shell --target $(WASM_TARGET) --release
	@mkdir -p std/shell/boot
	cp target/$(WASM_TARGET)/release/shell.wasm std/shell/boot/main.wasm

# --- Run ---------------------------------------------------------------------

run-kernel: kernel
	cargo run -- run std/kernel

# --- Clean -------------------------------------------------------------------

clean:
	cargo clean
	rm -f std/kernel/boot/main.wasm
	rm -f std/shell/boot/main.wasm

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
