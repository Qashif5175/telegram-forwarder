# Task definitions shared by developers and CI.
#
# CI runs these recipes rather than keeping its own copy of the commands.
# Before this file existed the two had already drifted: the documented
# `cargo clippy --all-targets` did not fail on a warning while CI's did, so a
# change could pass locally and break on push. One definition, one behaviour.
#
# Needs `just` plus the tools each recipe names:
#
#   brew install just typos-cli actionlint zizmor
#   cargo install cargo-shear
#
# `cargo install` puts binaries in ~/.cargo/bin, which has to be on PATH.

# Show the available recipes.
default:
    @just --list

# Run every check CI runs, across all of its workflows.
check: fmt lint test unused spell workflows

# Verify formatting without rewriting anything.
fmt:
    cargo fmt --all --check

# Rewrite formatting in place, for use while working.
fix:
    cargo fmt --all

# Lint, with pedantic on and warnings treated as failures.
lint:
    cargo clippy --all-targets --locked -- -D warnings

# Run the test suite. `--locked` also fails on an out-of-date Cargo.lock.
test:
    cargo test --all-targets --locked

# Find dependencies nothing references.
unused:
    cargo shear

# Spell check code and prose alike.
spell:
    typos

# Check the workflow files for syntax errors and security problems.
workflows:
    actionlint
    zizmor --no-progress .github/workflows/

# Build as released, for all platforms CI covers.
build:
    cargo build --release --locked
