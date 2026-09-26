# Justfile - https://github.com/casey/just
# Note: `check` and `test` must stay side-effect free so they are safe in CI.

default:
    @just --list

# Build the release binary
build:
    cargo build --release

# Build the debug binary
build-debug:
    cargo build

# Format the source tree
fmt:
    cargo fmt

# Verify formatting without modifying files
fmt-check:
    cargo fmt -- --check

# Lint with clippy, treating warnings as errors
lint:
    cargo clippy --all-targets --all-features -- -D warnings

# Compile every target without running anything
check:
    cargo fmt -- --check
    cargo clippy --all-targets --all-features -- -D warnings
    cargo check --locked --all-targets

# Run the test suite (unit + integration)
test:
    cargo test --locked

# Run only the fast unit tests
test-unit:
    cargo test --locked --bin Nucleus

# Run the integration tests with the privileges they need
test-integration:
    sudo -E cargo test --locked --test integration_tests -- --include-ignored --nocapture

# Everything CI runs
ci: check test

# Install the git pre-commit hook
install-hook:
    #!/usr/bin/env bash
    cat > .git/hooks/pre-commit << 'EOF'
    #!/bin/sh
    set -e
    echo "Running pre-commit quality checks..."
    just ci
    EOF
    chmod +x .git/hooks/pre-commit
    echo "Pre-commit hook installation confirmed."

# Remove the git pre-commit hook
remove-hook:
    rm -f .git/hooks/pre-commit
    echo "Pre-commit hook removal confirmed."

# Create and push a release tag
release version notes='':
    @echo "Tagging release v{{version}}..."
    git tag -a v{{version}} -m "Release v{{version}}: {{notes}}"
    git push origin v{{version}}
    echo "Release tag pushed."
