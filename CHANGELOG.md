# Changelog

All notable changes to this project are documented here.

Entries from 0.2.0 onwards are generated from the commit history by
[git-cliff](https://git-cliff.org) — see `cliff.toml` and `just changelog`.

## [0.1.0] - 2026-08-05

First release.

Mirrors Telegram messages from any number of chats into any number of others,
built around surviving a publisher who posts and then deletes seconds later. A
message is captured the instant its update arrives — before filtering, before
routing, before any network call — so deleting the source afterwards cannot take
it back.

### Added

- Many-to-many routing: any number of source chats into any number of targets,
  each route with its own delivery mode and content filter.
- A three-rung delivery ladder. A native forward keeps attribution; a copy
  survives the source being deleted or a channel that forbids forwarding; a
  rehost survives the file reference expiring. Anything that arrives below the
  top rung is counted separately as *rescued*.
- Albums are held briefly, ordered by message id and delivered as one post. If
  the group is refused as a group, its parts are delivered individually rather
  than lost.
- Every delivery is announced as it happens, saying which rung of the ladder
  carried it and how long it took, with per-route totals reported on exit.
- Interactive route management, with every chat chosen from the account's own
  dialog list. No chat ID is ever typed.
- Per-target pacing, a configurable album window, and content filters on
  keywords, media kinds, captions and forwarded posts.
- `tgfwd doctor` for checking a configuration, including whether every
  configured chat is still reachable.
- Both files this tool writes are created readable by their owner alone, with
  the permissions applied at creation rather than tightened afterwards. The
  session file is a live login to the account; the configuration holds the API
  credentials.
- Prebuilt binaries and installer scripts for macOS, Linux and Windows.

[0.1.0]: https://github.com/awdr74100/telegram-forwarder/releases/tag/v0.1.0
