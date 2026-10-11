# List available recipes
default:
    @just --list

# Format all code
fmt:
    cargo fmt

# Check formatting without changing files
fmt-check:
    cargo fmt --check

# Run clippy on all targets, treating warnings as errors
lint:
    cargo clippy --all-targets -- -D warnings

# Run the test suite
test:
    cargo test

# Run property tests with many more cases
test-props:
    PROPTEST_CASES=10000 cargo test --test properties

# Run the ignored large-input streaming test in release mode
test-large:
    cargo test --release -- --ignored large_input

# Run the engine on a CSV file
run FILE:
    cargo run -- {{FILE}}

# Check the declared minimum supported Rust version (needs `rustup toolchain install 1.85`)
msrv:
    cargo +1.85 test

# Build and open the documentation
doc:
    cargo doc --no-deps --open

# Everything CI runs
ci: fmt-check lint test
