# tgfwd

Many-to-many Telegram forwarding, built for messages that get deleted seconds
after they are posted.

Point it at any number of source chats and any number of targets. When something
is posted, it is captured locally *before* anything else happens, then delivered
to every target in parallel. If the publisher deletes the post one second later,
the delivery still completes.

```
📢 Breaking News  ─┐          ┌─→ 👥 Team channel
📢 Market Alerts  ─┼─ tgfwd ──┼─→ 📌 Saved Messages
👤 A contact      ─┘          └─→ 👥 Archive group
```

## Why it survives a deletion

A native Telegram forward needs the source message to still exist. If it is gone,
the forward fails and the message is lost. `tgfwd` walks down a ladder instead:

| | What it does | Cost | Survives |
|---|---|---|---|
| **Forward** | native forward, keeps "Forwarded from" | 1 request | — |
| **Copy** | re-sends as your own message, reusing the original media | 1 request | the source being deleted, and channels that forbid forwarding |
| **Rehost** | re-sends using bytes captured locally | uploads | the file reference expiring |

The important detail: **Copy does not re-upload anything.** It reuses the file
reference Telegram already gave us, so falling back costs the same as a forward.
That is why `auto` — try to keep attribution, fall back instantly if you cannot —
is the default rather than a slow safety net.

Deliveries that only succeeded because of the fallback are counted separately as
**rescued**, so you can see how often it actually mattered.

## Install

Requires [rustup](https://rustup.rs). The toolchain is pinned, so a fresh clone
builds with no further setup.

```sh
git clone <this repo> && cd telegram-forwarder
cargo build --release
./target/release/tgfwd --help
```

There is no database and no native dependency: the session is one JSON file and
the build produces a single binary.

## Getting started

```sh
tgfwd login          # walks you through getting an API key, then signs in
tgfwd route add      # pick sources and targets from a searchable list
tgfwd start          # go
```

You never type a chat ID. `route add` lists the chats your account is actually
in, searchable by name, `@username` or ID, and marks the channels you do not have
posting rights in before you pick them.

### Watching it run

```sh
tgfwd start          # colourful log lines
tgfwd start --tui    # full-screen dashboard
```

The dashboard shows per-route throughput, how many messages were rescued, what is
in flight, what is currently sitting out a rate limit, and a live event feed.

## Commands

| Command | Purpose |
|---|---|
| `tgfwd login` / `logout` | manage the session |
| `tgfwd route add` | create a route interactively |
| `tgfwd route list` | show configured routes |
| `tgfwd route edit` | change a route's sources, targets, mode or filter |
| `tgfwd route remove` | delete a route |
| `tgfwd route enable` / `disable` | toggle a route without deleting it |
| `tgfwd route sync` | refresh the chat names stored in the config |
| `tgfwd start [--tui] [--catch-up]` | run the forwarder |
| `tgfwd config edit` | open the config file in `$EDITOR`, then check it |
| `tgfwd config path` | print the config file's path |
| `tgfwd status` | show config, account and routes without connecting |
| `tgfwd doctor` | check for problems, including chats that are no longer reachable |
| `tgfwd completions <shell>` | shell completion script |

**You never have to type anything you cannot see.** Run any of the route
commands with nothing after it. Each one opens a picker showing what your routes
actually move — which matters because chat names routinely contain emoji and
symbols nobody can type from memory:

```
$ tgfwd route edit

? Which route?
❯ Breaking News → Team channel +1 more
  Market Alerts → Saved Messages       [disabled]

? Editing breaking-news — what would you like to change?
❯ Sources        (Breaking News)
  Targets        (Team channel +1 more)
  Delivery mode  (auto)
  Filter         (2 required, 1 blocked)
```

<details>
<summary>Scripting these commands</summary>

Each route gets a short name, generated from its source chat — you are never
asked to invent one. It exists so logs, the dashboard and shell scripts have
something stable to refer to, and it lets the same commands run unattended:

```sh
tgfwd route enable breaking-news    # no prompt, works in cron
```

`tgfwd route list` shows the names.

</details>

## Configuration

`tgfwd route add` writes the config for you, but it is plain TOML and meant to be
readable and hand-editable.

It lives wherever your platform says application data belongs — XDG directories
on Linux, `Application Support` on macOS, `%APPDATA%` on Windows. Rather than
memorise that, let the tool find it:

```sh
tgfwd config edit               # opens $EDITOR, then re-checks what you saved
tgfwd config path               # just the path, for scripts
$EDITOR "$(tgfwd config path)"  # quotes matter: the macOS path has a space
```

`config edit` creates a commented starting file if none exists, and refuses to
stay quiet if what you saved no longer parses or no longer makes sense.

```toml
[telegram]
api_id = 1234567
api_hash = "…"

[defaults]
mode = "auto"                     # auto | copy | forward

[defaults.snapshot]
enabled = true                    # download media as deletion insurance
max_bytes = 52428800              # skip files larger than this
ttl = "1h"                        # how long snapshots stay on disk

[defaults.dispatch]
per_target_interval = "300ms"     # minimum gap per destination chat
max_attempts = 5
max_flood_wait = "5m"             # refuse waits longer than this
max_in_flight = 64

[[route]]
id = "news-mirror"
enabled = true
sources = [{ id = -1001234567890, title = "Breaking News" }]
targets = [
  { id = -1009876543210, title = "Team channel" },
  { id = -1005555555555, title = "Archive group" },
]

[route.filter]
include = ["urgent", "快訊"]      # keep messages containing any of these
exclude = ["sponsored"]           # drop messages containing any of these
require_media = false
skip_forwarded = false
```

Chat IDs use the `-100…` form you see in Telegram Desktop. Titles are labels
only — `tgfwd route sync` refreshes them.

Pacing is enforced **per target chat**, so fanning out to ten channels happens at
full speed while a single busy channel is throttled on its own.

### Multiple profiles

Every path is relative to `TGFWD_HOME`, so separate accounts stay separate:

```sh
TGFWD_HOME=~/.tgfwd-work tgfwd start
```

## Loop protection

Forwarding chains can multiply messages without end, so two things prevent it:

- Configuration is rejected if the routes form a cycle (`A → B` and `B → A`).
- At runtime, messages this tool produced are remembered and never treated as new
  source material — so `A → B` plus `B → C` does not re-forward your own delivery.

## A note on accounts

This signs in as **you**, not as a bot, because only a user account can list the
chats you are in and read channels you have merely joined. Two consequences:

- The session file is a live credential. It is stored `chmod 600`, and anyone who
  copies it is logged into your account. `tgfwd logout` revokes it.
- Automating a user account is your responsibility under Telegram's terms.
  Sensible pacing defaults are set, but forwarding aggressively to many chats can
  still get an account limited.

## Development

```sh
cargo test                    # 91 tests, all offline
cargo clippy --all-targets    # clean, with pedantic lints on
cargo fmt
```

See [AGENTS.md](AGENTS.md) for the architecture and the reasoning behind it.

## Status

Working and tested, but young. Not yet done: CI, published binaries, and
long-outage reconnection testing.

## License

MIT
