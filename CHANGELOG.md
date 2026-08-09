# Changelog

All notable changes to this project are documented here.

## [0.1.0] - 2026-08-09

### Added

- Add many-to-many telegram forwarder
- Add config edit and path commands
- Make the album grouping window configurable
- Write the configuration owner-only, like the session
- Say where the media cache lives

### Fixed

- Reject settings that would stop delivery working
- Stop losing and duplicating parts of a post
- Keep the dashboard readable and always shut down
- Edit a filter instead of asking for it again
- Never leave the authorization key briefly readable
- Announce every delivery, not only the rescued ones
- Make a silently dropped album diagnosable
- Say why an update was dropped
- Deliver a group's parts when the group itself is refused
- Only split a refused group when smaller could succeed
- Name a route by what it moves at every decision point
- Always offer the picker, even for a single route
- Refuse --tui when its output is redirected
- Match the session file this tool actually writes
- Check for routes before asking for API credentials
- Keep one panicking task from taking every delivery with it
- Stop the dry run writing the changelog
- Stop passing --all-targets twice to rust-analyzer
- Space out every delivery to one chat, not just the second
- Pick the route that was highlighted, not the first that reads alike
- Keep cached media as private as the session beside it
- Tolerate two snapshots racing for one cache path
- Match Cargo's semver, not Renovate's

### Changed

- Drop the dashboard

### Documentation

- Record the traps found while auditing the delivery path
- Add the MIT licence text
- Rewrite the README around the questions it did not answer
- Explain what the filter settings actually match
- Say what the pacing interval trades, and admit the default is a guess
- Describe what actually happens when the network drops
- Record that taplo is maintenance-only, and why it stays
- Say what actually shipped, and add the files a public repo needs
- Lead with what it routes, not what it survives
- Describe the release process that now exists
- Describe shell completions, and what the media cache actually holds
- Write every example in English

### Build

- Run the same checks locally and in CI through a justfile
- Enforce the unwritten rules with clippy.toml
- Add a setup recipe for the tools the others call
- Ship prebuilt binaries and installer scripts with dist
- Generate the changelog from the commit history
- Stop the recipes depending on the caller's PATH
- Link the C runtime statically on Windows
- Fail on licence allowances nothing uses
- Declare the toolchain that is actually used as the minimum
- Generate it at release time, and skip nothing
- Prepare releases with cargo-release
- Group reverts where this file decides, not where the tool guesses

### CI

- Check formatting, lints, tests, spelling and unused dependencies
- Lint the workflow files themselves
- Follow the workflow conventions used across these repositories
- Check dependency licences and security advisories
- Run both workflow linters from one workflow
- Run the tests on every platform, not only Linux
- Run the checks that only ever ran locally

### Miscellaneous

- Normalise line endings through .gitattributes
- Point the repository metadata at the real remote
- Drop four dependencies nothing uses
- Settle which formatter runs on save
- Format TOML on save, with the rules committed
- Fold the TOML rules into the editor settings
- Recommend the TOML extension it already depends on
- Align the crate description with the README
- Name the author
- Add a pull request template
- Let Renovate propose dependency updates
- Update rust crate toml to v1
Entries are generated from the commit history by
[git-cliff](https://git-cliff.org) at release time — see `cliff.toml` and
`release.toml`. Nothing appears here until `cargo release` puts it here, so a
version and a date in this file always describe a release that happened.
