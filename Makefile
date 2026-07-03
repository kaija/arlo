.PHONY: build run test check fmt lint clean release

# Default target
all: check build test

# Build in debug mode
build:
	cargo build

# Build in release mode
release:
	cargo build --release

# Run the CLI
run:
	cargo run --bin agent-cli

# Run all tests
test:
	cargo test --workspace

# Run clippy lints
lint:
	cargo clippy --workspace --all-targets -- -D warnings

# Check compilation without building
check:
	cargo check --workspace

# Format code
fmt:
	cargo fmt --all

# Check formatting
fmt-check:
	cargo fmt --all -- --check

# Clean build artifacts
clean:
	cargo clean

# Run tests with output
test-verbose:
	cargo test --workspace -- --nocapture

# Build docs
doc:
	cargo doc --workspace --no-deps --open
