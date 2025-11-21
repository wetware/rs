.PHONY: clean examples podman-build podman-run podman-clean default-config
WASM_EXAMPLES := examples/wasm
DEFAULT_KERNEL := examples/default-kernel
DEFAULT_KERNEL_WASM := $(DEFAULT_KERNEL)/target/wasm32-wasip2/release/main.wasm
DEFAULT_CONFIG_CID := target/default-config.cid

all: examples default-config build

build:
	@# Ensure target directory and default-config.cid exists (create empty if missing)
	@mkdir -p target
	@if [ ! -f $(DEFAULT_CONFIG_CID) ]; then \
		echo "Info: Creating empty $(DEFAULT_CONFIG_CID) (run 'make default-config' to populate it)"; \
		touch $(DEFAULT_CONFIG_CID); \
	fi
	cargo build --release

clean:
	cargo clean
	rm -rf target
	rm -rf tmp
	@$(MAKE) -C $(DEFAULT_KERNEL) clean
	@if [ -d "$(WASM_EXAMPLES)/echo" ]; then \
		$(MAKE) -C $(WASM_EXAMPLES)/echo clean; \
	fi

examples: example-default-kernel
	@if [ -d "$(WASM_EXAMPLES)/echo" ]; then \
		$(MAKE) -C $(WASM_EXAMPLES)/echo; \
	fi

# Build default-kernel WASM example
# Can also be run directly: make -C examples/default-kernel build
example-default-kernel:
	@$(MAKE) -C $(DEFAULT_KERNEL) build

# Rebuild the default config
# This builds the default-kernel WASM, adds it to IPFS as a UnixFS directory,
# and writes the root CID to target/default-config.cid.
# Side effect: pushes the default config directory to IPFS.
# Runs automatically as part of 'make all', but only warns if IPFS is unavailable.
default-config:
	@echo "Rebuilding default config..."
	@$(MAKE) -C $(DEFAULT_KERNEL) build
	@if ! command -v ipfs >/dev/null 2>&1; then \
		echo "Warning: ipfs CLI not found. Skipping CID generation."; \
		echo "  Install Kubo (IPFS) and run 'make default-config' to generate the CID."; \
		exit 0; \
	fi
	@if ! ipfs swarm peers >/dev/null 2>&1; then \
		echo "Warning: IPFS daemon may not be running. Attempting export anyway..."; \
	fi
	@mkdir -p target
	@cd $(DEFAULT_KERNEL)/target/wasm32-wasip2/release && \
		IPFS_HASH=$$(ipfs add -r -Q . 2>/dev/null) && \
		if [ -n "$$IPFS_HASH" ]; then \
			echo "$$IPFS_HASH" > ../../../../../$(DEFAULT_CONFIG_CID) && \
			echo "" && \
			echo "✓ Successfully updated default config" && \
			echo "  CID: /ipfs/$$IPFS_HASH" && \
			echo "  File: $(DEFAULT_CONFIG_CID)"; \
		else \
			echo "Warning: Failed to add to IPFS. CID not updated."; \
			echo "  Ensure IPFS daemon is running: ipfs daemon"; \
		fi

# Podman targets
podman-build:
	podman build -t wetware:latest .

podman-run:
	podman run --rm -it wetware:latest

podman-clean:
	podman rmi wetware:latest || true
	podman system prune -f

# Development with Podman
podman-dev: podman-build
	podman run --rm -it -v $(PWD):/app wetware:latest
