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
    cargo clippy --all-targets --locked -- -D warnings

# Run the test suite
test:
    cargo test --locked

# Run property tests with many more cases
test-props:
    PROPTEST_CASES=10000 cargo test --locked --test properties

# Run the ignored large-input streaming test in release mode
test-large:
    cargo test --locked --release -- --ignored large_input

# Compare the engine with a reference model on random batches (needs python3)
differential *ARGS:
    python3 -I scripts/differential_test.py {{ARGS}}

# Run the engine on a CSV file
run FILE:
    cargo run -- {{FILE}}

# Check the declared minimum supported Rust version (needs `rustup toolchain install 1.85`)
msrv:
    cargo +1.85 test --locked

# Build and open the documentation
doc:
    cargo doc --no-deps --open

# Build the documentation, treating warnings as errors
doc-check:
    RUSTDOCFLAGS="-D warnings" cargo doc --no-deps

# Everything CI runs
ci: fmt-check lint test doc-check
