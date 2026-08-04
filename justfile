# Task definitions shared by developers and CI.
#
# CI runs these recipes rather than keeping its own copy of the commands.
# Before this file existed the two had already drifted: the documented
# `cargo clippy --all-targets` did not fail on a warning while CI's did, so a
# change could pass locally and break on push. One definition, one behaviour.
#
# Needs `just` itself, then `just setup` for everything the recipes call.

# Show the available recipes.
default:
    @just --list

# `binstall` fetches prebuilt binaries; building these from source takes longer
# than every check they then perform. Installs land in ~/.cargo/bin, which has
# to be on PATH. `actionlint` is written in Go and is not on crates.io, so it
# comes from elsewhere.
#
# Install everything the other recipes call.
setup:
    cargo install cargo-binstall
    cargo binstall --no-confirm cargo-shear cargo-deny typos-cli zizmor
    @echo "actionlint is not a crate — install it with: brew install actionlint"

# Run every check CI runs, across all of its workflows.
check: fmt lint test unused spell workflows release-check

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

# Check dependency licences, security advisories and sources.
audit:
    cargo deny check

# Check the workflow files for syntax errors and security problems.
workflows:
    actionlint
    zizmor --no-progress .github/workflows/

# Regenerate the release workflow after changing dist-workspace.toml, and fail
# if it was left stale. `dist` writes the workflow; it is committed like any
# other file, so the pipeline outlives the tool.
release-check:
    dist generate --check
    dist plan

# Build as released, for all platforms CI covers.
build:
    cargo build --release --locked
