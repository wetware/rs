# Wetware build system
#
# `make images` assembles FHS-style image directories under images/.
# Each image has bin/main.wasm as its entrypoint, consumable by `ww run`.

WASM_TARGET := wasm32-wasip2
RELEASE_DIR  = target/$(WASM_TARGET)/release

IMAGES_DIR := images

.PHONY: all host guests images clean run-kernel
.PHONY: guest-kernel image-kernel
.PHONY: podman-build podman-run podman-clean podman-dev

all: guests images host

# --- Host -------------------------------------------------------------------

host:
	cargo build --release

# --- Guests ------------------------------------------------------------------

guests: guest-kernel

guest-kernel:
	cd std/kernel && cargo build --target $(WASM_TARGET) --release --target-dir target

# --- Images ------------------------------------------------------------------
# Assemble FHS-style image directories:
#   <image>/bin/main.wasm   — guest entrypoint

images: image-kernel

image-kernel: guest-kernel
	@mkdir -p $(IMAGES_DIR)/kernel/bin
	cp std/kernel/$(RELEASE_DIR)/kernel.wasm $(IMAGES_DIR)/kernel/bin/main.wasm

# Build the kernel image and launch it interactively.
run-kernel: image-kernel
	cargo run -- run $(IMAGES_DIR)/kernel

# --- Clean -------------------------------------------------------------------

clean:
	cargo clean
	rm -rf $(IMAGES_DIR)/*/bin/*.wasm
	rm -rf tmp

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
