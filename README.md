# telegram-forwarder

Mirror Telegram messages from any number of chats into any number of other chats
— built for publishers who post and then delete seconds later.

The command is called **`tgfwd`**. That is the binary this repository builds;
`telegram-forwarder` is the project and crate name.

```
📢 Breaking News  ─┐          ┌─→ 👥 Team channel
📢 Market Alerts  ─┼─ tgfwd ──┼─→ 📌 Saved Messages
👤 A contact      ─┘          └─→ 👥 Archive group
```

## What problem this solves

A native Telegram forward needs the source message to still exist. Point a
scheduler at a channel that deletes its posts a second after publishing them and
you get nothing: by the time the forward is issued, there is nothing to forward.

`tgfwd` copies the message into memory the instant the update arrives — before
filtering, before routing, before any network call — and only then decides what
to do with it. Deleting the source afterwards cannot take it back.

If the forward fails, delivery walks down a ladder instead of giving up:

| | What it does | Cost | Survives |
|---|---|---|---|
| **Forward** | native forward, keeps "Forwarded from" | 1 request | — |
| **Copy** | re-sends as your own message, reusing the original media | 1 request | the source being deleted, and channels that forbid forwarding |
| **Rehost** | re-sends using bytes captured locally | uploads | the file reference expiring |

The detail that makes this practical: **Copy does not re-upload anything.** It
reuses the file reference Telegram already handed over, so falling back costs the
same as a forward. That is why `auto` — keep attribution when you can, fall back
instantly when you cannot — is the default rather than a slow safety net.

Messages that only arrived because of a fallback are counted separately as
**rescued**, so you can see how often it actually mattered.

## Requirements

- [rustup](https://rustup.rs). The toolchain is pinned in `rust-toolchain.toml`,
  so a fresh clone builds with no further setup.
- A **Telegram account** (not a bot — see [Why a user account](#why-a-user-account)).
- Your own Telegram **API key**, which is free and takes about a minute to get.
  The next section is entirely about that, because it is the one part nobody can
  do for you.

No database, no C toolchain, no native dependencies. The build produces a single
binary and the session is one JSON file.

## Install

**macOS and Linux**

```sh
curl -LsSf https://github.com/awdr74100/telegram-forwarder/releases/latest/download/telegram-forwarder-installer.sh | sh
```

**Windows**

```powershell
powershell -ExecutionPolicy Bypass -c "irm https://github.com/awdr74100/telegram-forwarder/releases/latest/download/telegram-forwarder-installer.ps1 | iex"
```

Both fetch a prebuilt binary for your platform, check it against the published
checksum, and put `tgfwd` on your `PATH`. **Updating is the same command** —
there is no separate updater to keep track of.

Prefer to do it yourself? Every release carries an archive per platform, each
with its own `.sha256`, on the
[releases page](https://github.com/awdr74100/telegram-forwarder/releases).

<details>
<summary>Building from source</summary>

Not required to use this — only to work on it.

```sh
git clone https://github.com/awdr74100/telegram-forwarder
cd telegram-forwarder
cargo build --release
./target/release/tgfwd --help
```

This crate is deliberately not published to crates.io: `cargo install` would ask
for a Rust toolchain and a few minutes of compiling to deliver a binary the
installer above hands over in seconds.

</details>

## Where the login details come from

Three separate things are involved, and it is worth knowing which is which:

### 1. An API key identifies the *application*

Telegram requires every third-party client to register. This key cannot be
shipped inside a public binary, so you create your own:

1. Open <https://my.telegram.org/auth> and sign in with your phone number.
2. Choose **API development tools**.
3. Create an application. Any name and description will do — nobody reviews it.
4. Copy the **`api_id`** (a number) and **`api_hash`** (32 hex characters).

`tgfwd login` prints these steps and then prompts for both values, so you do not
have to do this in advance. They are written to the config file, not compiled in.

These identify the app, not you. They are not a password, but they are yours and
are not meant to be committed anywhere.

### 2. Your phone number and login code identify *you*

`tgfwd login` then asks for:

- your phone number in international format, e.g. `+886912345678`;
- the **login code** Telegram sends to your other signed-in devices (not SMS, if
  you have another device active);
- your **two-factor password**, if you have one set. Your own hint is shown.

A mistyped code or password is re-asked up to three times against the same
request, because asking Telegram for a fresh code is rate-limited far more
aggressively than retrying one.

### 3. The session file is what stays behind

On success, the resulting authorization key is stored so you never repeat the
above. **That file is a live credential**: whoever copies it is signed into your
account. It is written `chmod 600`, created with those permissions rather than
tightened afterwards, and `tgfwd logout` revokes it server-side and deletes it.

## Where your data lives

Nothing is stored in the project directory. Paths follow each platform's
convention, which means they are not guessable — so ask the tool instead of
memorising them:

```sh
tgfwd status                    # everything, in human form
tgfwd config path               # just the config path, for scripts
tgfwd config edit               # open it in $EDITOR, then re-check what you saved
$EDITOR "$(tgfwd config path)"  # quotes matter: the macOS path contains a space
```

| | What it is | macOS | Linux |
|---|---|---|---|
| `config.toml` | your API key and routes | `~/Library/Application Support/tgfwd/` | `~/.config/tgfwd/` |
| `session.json` | the authorization key — a live credential | `~/Library/Application Support/tgfwd/` | `~/.local/share/tgfwd/` |
| media cache | snapshotted bytes, safe to delete | `~/Library/Caches/tgfwd/` | `~/.cache/tgfwd/` |

On Windows these resolve under `%APPDATA%` and `%LOCALAPPDATA%`. Rather than
trust this table, run `tgfwd status` — it prints the real answer for your machine.

### Multiple accounts

`TGFWD_HOME` overrides all of it and lays one profile out under a single root,
which is also the safe way to experiment without touching your real setup:

```sh
TGFWD_HOME=~/.tgfwd-work tgfwd login
TGFWD_HOME=~/.tgfwd-work tgfwd start
```

## Quick start

```sh
tgfwd login          # API key walkthrough, then phone + code
tgfwd route add      # pick sources and targets from a searchable list
tgfwd start          # go
```

**You never type a chat ID.** `route add` lists the chats your account is
actually in, searchable by name, `@username` or ID, and flags the channels you do
not have posting rights in *before* you pick them:

```
? Which chats should be watched?
❯ ◻ 📢 台灣科技新聞      @twtech      -1001234567890
  ◻ 📢 Breaking News     @news        -1009876543210
  ◻ 👥 Team channel      —            -1005555555555  (no post rights)
```

This is deliberate: chat titles are full of emoji and symbols nobody can retype
from memory, and a mistyped ID fails silently at delivery time — the worst
possible moment to find out.

## What a route is

A route is one rule: **these source chats → these target chats**, plus how to
deliver and what to filter. Sources and targets are both lists, so one route can
fan several channels into several destinations.

Each route gets a short name generated from its first source chat. You are never
asked to invent one; it exists only so logs, the dashboard and shell scripts have
something stable to refer to.

```sh
tgfwd route list      # see them
tgfwd route edit      # change sources, targets, delivery mode or filter
tgfwd route disable   # switch one off without deleting it
tgfwd route sync      # refresh the chat names stored in the config
```

Run any of these with no arguments and you get a picker showing what each route
*moves*, not its name:

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

Editing a filter starts from the filter you already have — existing keywords come
back in the input buffer rather than needing to be retyped.

<details>
<summary>Naming a route explicitly, for scripts and cron</summary>

```sh
tgfwd route enable breaking-news    # no prompt, works unattended
tgfwd route list                    # shows the names
```

</details>

## Filtering

Optional, per route. A message has to satisfy every condition to be forwarded.

| Setting | Effect |
|---|---|
| `include` | drop the message unless it contains at least one of these |
| `exclude` | drop the message if it contains any of these |
| `kinds` | drop the message unless it is one of these kinds |
| `require_media` | drop messages carrying no media |
| `skip_forwarded` | drop messages that are themselves forwards |

### What `include` and `exclude` match against

The **message text** — and for a photo, video or file, that means its **caption**,
since Telegram stores them in the same place. Nothing else is searched: not the
sender, not the file name, not the contents of the file.

Matching is **case-insensitive substring** matching. There is no regex dialect
and no word-boundary rule:

- `urgent` matches `URGENT`, `Urgently`, and also `insurgent`
- `快訊` matches `【快訊】今日重點`
- `exclude` is checked first, so a message matching both is dropped

Two consequences worth knowing before you rely on them:

> **An `include` filter drops every post that has no caption.** A photo posted
> with no text has nothing to match, so `include = ["urgent"]` on a photo channel
> keeps only the captioned photos. If you want media regardless of wording, use
> `kinds` or `require_media` instead.

> **For an album, all the captions are searched together.** Telegram puts the
> caption on one member of the group, not necessarily the first, so the whole
> post is judged as a whole — a keyword on the third photo still counts, and a
> blocked word anywhere drops the entire group.

### What `skip_forwarded` is for

It drops messages that already carry Telegram's "Forwarded from" header — that
is, posts the source chat did not write itself but relayed from somewhere else.

Two situations it exists for:

- **Aggregator channels.** Many channels repost other channels. If you only want
  what this one actually publishes, this removes the relayed noise.
- **Avoiding duplicates.** If you already mirror the original channel, its posts
  would otherwise reach your target twice — once from the original, once via
  whoever reposted it.

It says nothing about *this* tool's own deliveries. Those are recognised
separately and are never treated as new source material, whether or not you set
this.

## Running it

```sh
tgfwd start              # colourful log lines
tgfwd start --tui        # full-screen dashboard
tgfwd start --catch-up   # also process messages that arrived while it was stopped
```

The dashboard shows per-route throughput, how many messages were rescued, what is
in flight, what is sitting out a rate limit, and a live event feed. Log output is
held back while it is open and replayed when you quit, so nothing is lost and
nothing paints over the display.

**Stopping.** `Ctrl+C` finishes in-flight deliveries first, which can take a
moment if one is waiting out a server-issued rate limit. Press it a second time
to leave immediately.

**Exit codes** — useful under `systemd`, `launchd` or a supervisor:

| Code | Meaning |
|---|---|
| `0` | stopped cleanly |
| `1` | configuration or connection problem, explained on stderr |
| `130` | interrupted, or a prompt was cancelled |

Losing the connection to Telegram is an error, not a clean stop, so a supervisor
will restart rather than assume all is well.

## Configuration

`tgfwd route add` writes this for you, but it is plain TOML and meant to be read
and edited by hand. `tgfwd config edit` opens it and **re-validates on save**, so
a mistake is caught there rather than at the next start.

```toml
[telegram]
api_id = 1234567
api_hash = "…"

[defaults]
mode = "auto"                     # auto | copy | forward

[defaults.snapshot]
enabled = true                    # download media as deletion insurance
max_bytes = 52428800              # skip files larger than this; 0 disables the limit
ttl = "1h"                        # how long snapshots stay on disk

[defaults.dispatch]
album_window = "400ms"            # how long to wait for an album's other parts
per_target_interval = "300ms"     # minimum gap per destination chat
max_attempts = 5                  # per delivery strategy
max_flood_wait = "5m"             # refuse server-requested waits longer than this
max_in_flight = 64                # concurrent deliveries across all routes

[[route]]
id = "news-mirror"
enabled = true
sources = [{ id = -1001234567890, title = "Breaking News" }]
targets = [
  { id = -1009876543210, title = "Team channel" },
  { id = -1005555555555, title = "Archive group" },
]

[route.filter]
include = ["urgent", "快訊"]
exclude = ["sponsored"]
require_media = false
skip_forwarded = false
```

Anything under `[defaults]` can be overridden per route. Chat IDs use the `-100…`
form Telegram Desktop shows; titles are labels only, refreshed by
`tgfwd route sync`.

Pacing is enforced **per target chat**, so fanning out to ten channels runs at
full speed while one busy channel is throttled on its own. An album counts once,
not once per photo.

### Tuning `album_window`

Telegram sends the members of an album as separate updates and never signals
that the last one has gone, so the only way to know a group is complete is that
it stopped growing. This is how long to give it.

Waiting costs latency and never content — every part is captured before the timer
starts, so a source deleted during the window is still delivered. Set it longer
if you ever see one post arrive as two (a straggler that missed the window forms
a group of its own); there is no reason to set it shorter.

`0` turns grouping off, forwarding each member as its own message. It is the one
value here that changes *what* the target receives rather than *when*.

### Tuning `per_target_interval`

It only bites during a burst into a single chat — a channel posting a few times a
minute never touches it. The trade it makes is deliberately lopsided: a few
hundred milliseconds spent here costs exactly that, while earning a `FLOOD_WAIT`
instead costs whatever Telegram decides, holds a delivery slot while it waits,
and fails outright once the wait exceeds `max_flood_wait`.

The default is a conservative guess, not a derived figure — Telegram does not
publish the limits that apply to user accounts, and the numbers usually quoted
are the Bot API's, which do not. So tune it by observation rather than by
arithmetic: **the `waiting` counter on the dashboard is the signal.** If it sits
above zero, the interval is too short for your traffic. Setting it to `0`
disables pacing altogether.

## Loop protection

Forwarding chains can multiply messages without end, so two things prevent it:

- Configuration is rejected if the routes form a cycle (`A → B` plus `B → A`).
- At runtime, messages this tool produced are remembered and never treated as new
  source material, so `A → B` plus `B → C` does not re-forward your own delivery.

## Why a user account

Only a user account can list the chats you are in and read channels you have
merely joined; a bot cannot enumerate its own dialogs, which would force you back
to pasting chat IDs. Two consequences worth stating plainly:

- The session file is a live credential (see [above](#3-the-session-file-is-what-stays-behind)).
- Automating a user account is your responsibility under Telegram's terms. The
  defaults pace conservatively, but forwarding aggressively into many chats can
  still get an account limited.

## Troubleshooting

Start here:

```sh
tgfwd doctor
```

It checks that the config parses, that the routes are consistent, that
credentials are present, that a session exists, and that **every configured chat
is still reachable by this account** — which is the failure people hit most, and
the one that is otherwise invisible until a delivery fails.

| Symptom | Likely cause |
|---|---|
| `… is not reachable by this account` | the account left the chat, or the cache is stale — run `tgfwd login` to refresh it |
| Nothing is forwarded | check the route is enabled (`tgfwd route list`) and the filter is not rejecting everything |
| Only some targets receive | usually a permissions problem in that one chat; `tgfwd doctor` names it |
| Constantly rate-limited | raise `defaults.dispatch.per_target_interval` |

`-v` adds this tool's debug output; `-vv` adds the underlying Telegram stack.
`RUST_LOG` overrides both if you want finer control.

## Development

```sh
just            # list the recipes
just check      # everything CI checks: format, lint, test, spelling, unused deps, workflows
just fix        # rewrite formatting in place
```

CI runs the same recipes, so a clean `just check` is a clean pipeline.

```sh
brew install just actionlint   # actionlint is Go, not a crate
just setup                     # the rest, as prebuilt binaries
```

`just setup` installs into `~/.cargo/bin`; the recipes put that on `PATH`
themselves, so nothing needs adding to your shell profile.

CI also builds for release on Linux, macOS and Windows: the session file's
permissions and the config directory layout both differ on Windows, and building
on Linux alone would exercise neither.

The toolchain is pinned in `rust-toolchain.toml`, so `rustup` installs the right
version automatically on first build — there is no setup step. Clippy runs with
`pedantic` enabled; the handful of exceptions live in `Cargo.toml` under
`[lints.clippy]`, each with a written reason. `unsafe` is forbidden crate-wide,
including in tests.

Tests live beside the code in `#[cfg(test)] mod tests` and are named as
sentences, e.g. `a_deleted_source_degrades_to_the_snapshot`.

To try changes against a real account without touching your own setup, point
`TGFWD_HOME` somewhere disposable:

```sh
export TGFWD_HOME=/tmp/tgfwd-test
cargo run -- login
cargo run -- route add
cargo run -- start
rm -rf /tmp/tgfwd-test        # start over
```

[AGENTS.md](AGENTS.md) documents the architecture, the design rules that must not
be broken, and the Telegram API traps that cost the most to discover.

## Status

Working, but young. Not yet done: CI, published binaries, and testing against a
long network outage.

## License

[MIT](LICENSE) © 2026-present Roya
