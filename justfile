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

# Run the binary on one fixture exactly as the brief invokes it, then compare the output
smoke FIXTURE="chargeback_negative_total":
    mkdir -p target/smoke
    cp tests/fixtures/{{FIXTURE}}/input.csv target/smoke/transactions.csv
    cd target/smoke && cargo run -- transactions.csv > accounts.csv
    diff target/smoke/accounts.csv tests/fixtures/{{FIXTURE}}/expected.csv
    @echo "smoke test passed: {{FIXTURE}}"

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

# Everything the main CI job runs (MSRV is a separate job: just msrv)
ci: fmt-check lint test doc-check
