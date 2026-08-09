# Task definitions shared by developers and CI.
#
# CI runs these recipes rather than keeping its own copy of the commands.
# Before this file existed the two had already drifted: the documented
# `cargo clippy --all-targets` did not fail on a warning while CI's did, so a
# change could pass locally and break on push. One definition, one behaviour.
#
# Needs `just` itself, then `just setup` for everything the recipes call.

# `cmd.exe` cannot run these recipes as written; PowerShell can.
set windows-shell := ["powershell.exe", "-NoLogo", "-Command"]

# `cargo install` puts binaries in the cargo bin directory. Prepending it here
# means the recipes work whether or not the surrounding shell happens to have
# it, rather than failing with a bare "command not found" that says nothing
# about why. `home_directory()` and `/` are platform-aware; the separator
# between PATH entries is not, so it is chosen explicitly.
cargo-bin := home_directory() / ".cargo" / "bin"
export PATH := if os_family() == "windows" {
  cargo-bin + ";" + env_var("PATH")
} else {
  cargo-bin + ":" + env_var("PATH")
}

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
    cargo binstall --no-confirm cargo-shear cargo-deny git-cliff typos-cli zizmor
    cargo binstall --no-confirm cargo-release
    # Pinned: `dist generate --check` compares against what one specific version
    # emits, so a newer one here reports the committed workflow as stale. Keep
    # this equal to `cargo-dist-version` in dist-workspace.toml.
    cargo binstall --no-confirm cargo-dist@0.32.0
    @echo "actionlint is not a crate. See https://github.com/rhysd/actionlint/releases"

# Run every check CI runs, across all of its workflows.
#
# `audit` belongs here despite being the slow one. It was left out at first, on
# the reasoning that CI runs it weekly anyway — and a licence allowance went
# stale for several commits without anything local saying so, because dropping a
# dependency removed the only crate that used it. A list of checks that is almost
# everything CI runs is worth less than it looks.
check: fmt lint test unused spell workflows release-check audit

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

# Preview the entry the next release will write, without writing it.
#
# Deliberately read-only: `cargo release` generates the real entry through the
# pre-release hook in `release.toml`, and a copy prepended by hand beforehand
# would leave the file with two sections for the same version.
changelog:
    git cliff --unreleased

# Regenerate the release workflow after changing dist-workspace.toml, and fail
# if it was left stale. `dist` writes the workflow; it is committed like any
# other file, so the pipeline outlives the tool.
release-check:
    dist generate --check
    dist plan

# Build as released, for all platforms CI covers.
build:
    cargo build --release --locked
