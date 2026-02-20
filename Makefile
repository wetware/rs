# Wetware build system
#
# Guest builds are staged: child-echo must be built before the kernel
# (kernel embeds child-echo via include_bytes!).
#
# `make images` assembles FHS-style image directories under examples/images/.
# Each image has bin/main.wasm as its entrypoint, consumable by `ww exec`.

WASM_TARGET := wasm32-wasip2
RELEASE_DIR  = target/$(WASM_TARGET)/release

IMAGES_DIR := examples/images

.PHONY: all host guests images clean
.PHONY: guest-child-echo guest-shell guest-kernel
.PHONY: image-child-echo image-shell image-kernel
.PHONY: podman-build podman-run podman-clean podman-dev

all: guests images host

# --- Host -------------------------------------------------------------------

host:
	cargo build --release

# --- Guests ------------------------------------------------------------------
# Each guest is built with --target-dir target so that artifacts land in
# the per-crate target/ dir instead of the workspace root.  This matters
# for kernel, which uses include_bytes! pointing at guests/child-echo/target/.

guests: guest-child-echo guest-shell guest-kernel

guest-child-echo:
	cd guests/child-echo && cargo build --target $(WASM_TARGET) --release --target-dir target

guest-shell:
	cd guests/shell && cargo build --target $(WASM_TARGET) --release --target-dir target

# kernel depends on child-echo (include_bytes! references its wasm)
guest-kernel: guest-child-echo
	cd std/kernel && cargo build --target $(WASM_TARGET) --release --target-dir target

# --- Images ------------------------------------------------------------------
# Assemble FHS-style image directories:
#   <image>/bin/main.wasm   — guest entrypoint

images: image-child-echo image-shell image-kernel

image-child-echo: guest-child-echo
	@mkdir -p $(IMAGES_DIR)/child-echo/bin
	cp guests/child-echo/$(RELEASE_DIR)/child_echo.wasm $(IMAGES_DIR)/child-echo/bin/main.wasm

image-shell: guest-shell
	@mkdir -p $(IMAGES_DIR)/shell/bin
	cp guests/shell/$(RELEASE_DIR)/shell.wasm $(IMAGES_DIR)/shell/bin/main.wasm

image-kernel: guest-kernel
	@mkdir -p $(IMAGES_DIR)/kernel/bin
	cp std/kernel/$(RELEASE_DIR)/pid0.wasm $(IMAGES_DIR)/kernel/bin/main.wasm

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
