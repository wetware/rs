# Wetware build system
#
# `make images` assembles FHS-style image directories under images/.
# Each image has bin/main.wasm as its entrypoint, consumable by `ww run`.

WASM_TARGET := wasm32-wasip2
RELEASE_DIR  = target/$(WASM_TARGET)/release

IMAGES_DIR := images

.PHONY: all host guests images clean run-shell
.PHONY: guest-shell image-shell
.PHONY: podman-build podman-run podman-clean podman-dev

all: guests images host

# --- Host -------------------------------------------------------------------

host:
	cargo build --release

# --- Guests ------------------------------------------------------------------

guests: guest-shell

guest-shell:
	cd std/shell && cargo build --target $(WASM_TARGET) --release --target-dir target

# --- Images ------------------------------------------------------------------
# Assemble FHS-style image directories:
#   <image>/bin/main.wasm   — guest entrypoint

images: image-shell

image-shell: guest-shell
	@mkdir -p $(IMAGES_DIR)/shell/bin
	cp std/shell/$(RELEASE_DIR)/shell.wasm $(IMAGES_DIR)/shell/bin/main.wasm

# Build the shell image and launch it interactively.
run-shell: image-shell
	cargo run -- run $(IMAGES_DIR)/shell

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
