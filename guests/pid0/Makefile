.PHONY: build clean

build:
	@echo "Building pid0 WASM..."
	@$(MAKE) -C ../child-echo build
	@PATH="$$(rustup which cargo | xargs dirname):$$PATH" cargo build --target wasm32-wasip2 --release
	@if [ -f target/wasm32-wasip2/release/pid0.wasm ]; then \
		cp target/wasm32-wasip2/release/pid0.wasm target/wasm32-wasip2/release/main.wasm; \
		echo "Built: target/wasm32-wasip2/release/main.wasm"; \
	else \
		echo "Error: pid0.wasm not found. Build may have failed."; \
		exit 1; \
	fi

clean:
	@echo "Cleaning pid0 artifacts..."
	@cargo clean
	@rm -rf target
