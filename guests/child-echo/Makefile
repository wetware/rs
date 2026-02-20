.PHONY: build clean

build:
	@echo "Building child-echo WASM..."
	@PATH="$$(rustup which cargo | xargs dirname):$$PATH" cargo build --target wasm32-wasip2 --release
	@if [ -f target/wasm32-wasip2/release/child_echo.wasm ]; then \
		cp target/wasm32-wasip2/release/child_echo.wasm target/wasm32-wasip2/release/main.wasm; \
		echo "Built: target/wasm32-wasip2/release/main.wasm"; \
	else \
		echo "Error: child_echo.wasm not found. Build may have failed."; \
		exit 1; \
	fi

clean:
	@echo "Cleaning child-echo artifacts..."
	@cargo clean
	@rm -rf target
