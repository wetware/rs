.PHONY: build clean

# Build the WASM file
# Note: wasm32-wasip2 requires unstable features available in nightly
# The #![feature(wasip2)] in lib.rs enables the feature for this crate and dependencies
build:
	@echo "Building default-kernel WASM..."
	@PATH="$$(rustup which cargo | xargs dirname):$$PATH" cargo build --target wasm32-wasip2 --release
	@if [ -f target/wasm32-wasip2/release/default_kernel.wasm ]; then \
		cp target/wasm32-wasip2/release/default_kernel.wasm target/wasm32-wasip2/release/main.wasm; \
		echo "Built: target/wasm32-wasip2/release/main.wasm"; \
	else \
		echo "Error: default_kernel.wasm not found. Build may have failed."; \
		exit 1; \
	fi

clean:
	@echo "Cleaning default-kernel artifacts..."
	@cargo clean
	@rm -f *.cid
	@rm -rf target

