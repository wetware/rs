.PHONY: build clean

# Build the WASM file
# Note: wasm32-wasip2 requires unstable features available in nightly
# The #![feature(wasip2)] in lib.rs enables the feature for this crate and dependencies
build:
	@echo "Building default-kernel WASM..."
	@PATH="$$(rustup which cargo | xargs dirname):$$PATH" RUSTFLAGS="--cfg tokio_unstable" cargo build --target wasm32-wasip2 --release
	@WASM_FILE=$$(find target/wasm32-wasip2/release -name "*.wasm" -type f | head -1); \
	if [ -n "$$WASM_FILE" ]; then \
		cp "$$WASM_FILE" target/wasm32-wasip2/release/main.wasm; \
		echo "Built: target/wasm32-wasip2/release/main.wasm"; \
	else \
		echo "Warning: No WASM file found. Ensure the build produces a .wasm file."; \
		exit 1; \
	fi

clean:
	cargo clean

