# AGENTS.md

Guidance for anyone — human or agent — working on this repository.

## What this is

`tgfwd` mirrors Telegram messages from many source chats into many target chats.
It is built around one hostile case: **a publisher posts, then deletes one second
later**. Everything below follows from that.

## Non-negotiable design rules

1. **Capture before you decide.** `Snapshotter::capture` is synchronous and
   cannot fail. It runs before filtering, before routing, before any network
   call. Once a message is captured, deletion cannot take it away. Never move
   work ahead of capture, and never make capture `async`.

2. **Targets are independent.** Each target gets its own task and its own place
   in the pacer. One rate-limited chat must never delay the other nine. Do not
   introduce a shared serial queue.

3. **Failures degrade, they do not just retry.** See `engine/failure.rs`. A
   Telegram error means one of four things — wait, degrade, back off, or give up
   — and conflating them is the main way this class of tool loses messages.

4. **Nobody types a chat ID.** Everything selectable comes from the account's
   own dialog list (`telegram/dialogs.rs`). A mistyped ID fails silently at
   delivery time, which is the worst possible moment.

5. **Identifiers are for machines, not people.** A route's `id` exists so logs,
   the dashboard and shell scripts can name one. It is derived from the source
   chat by `auto_id`; the user is never asked to invent or recall it. Anywhere a
   route is offered for selection, show `describe_route` — what it moves — not
   the id. Do not add a prompt that asks the user to name something, and never
   ask anyone to type a value that already exists in the config or on Telegram:
   chat titles are full of emoji and symbols that cannot be typed from memory.

## Architecture

```
main.rs → cli.rs → commands/ → engine/
                             → telegram/  (all Telegram I/O)
                             → config/    (schema, validation)
                             → session.rs (single-file Session impl)
                             → ui/        (theme, prompts, logger, TUI)
```

`telegram/` never depends on `cli/`, `config/` or `ui/`. The login flow asks
questions through the `LoginPrompt` trait so it carries no terminal dependency.

### The delivery ladder (`engine/delivery.rs`)

| Rung | Cost | Survives |
|---|---|---|
| `Forward` | 1 RPC, no upload, keeps attribution | nothing extra |
| `Copy` | 1 RPC, no upload, reuses the file reference | source deletion, forward restrictions |
| `Rehost` | re-uploads snapshotted bytes | a dead file reference |

`DeliveryMode` picks which rungs exist. `Auto` uses all three, which is why it is
the default. Anything delivered below the top rung is counted as *rescued*.

**The payload shrinks as it descends.** Telegram answers a multi-message send
with one slot per message and puts `None` in the ones it refused, so a rung can
place part of an album and refuse the rest. Only the refused parts continue
downwards: re-sending the whole group would duplicate what already arrived, and
those duplicates would not be recognised by the `EchoGuard`. Whatever a rung does
place is remembered by the guard immediately, success or not.

### Session storage (`session.rs`)

`grammers-session` ships only a memory store and a SQLite one. This project
implements the `Session` trait over a single JSON file instead, so
`grammers-session` is built with `default-features = false` and `libsql` never
enters the dependency graph. **Do not re-enable `sqlite-storage`.**

Authorization keys are written through immediately; everything else is batched
behind a dirty flag and flushed by the engine's housekeeping tick.

**`cache_peer` must not block or await anything real.** `Engine::run` polls
`UpdateStream::next` inside a `tokio::select!`, so that future is dropped
whenever the housekeeping tick wins the race. It survives that today only
because the one await inside `process_socket_updates` — `build_peer_map`, which
calls `cache_peer` — completes without ever yielding. Give `cache_peer` a real
suspension point and updates that have already advanced the `pts` will start
disappearing at cancellation, with no gap left for `getDifference` to recover.

## Traps discovered the hard way

- **`RpcError::name` has the number stripped out.** `FLOOD_WAIT_42` arrives as
  `name = "FLOOD_WAIT"`, `value = Some(42)`. Matching `is("FLOOD_WAIT_*")`
  silently never fires. There is a regression test for exactly this.
- **`RpcError::is` only understands a leading or trailing `*`.** A middle
  wildcard like `CHAT_SEND_*_FORBIDDEN` degrades into an exact comparison that
  never matches. Use the common prefix.
- **`forward_messages` and `send_album` can partially succeed**, returning `None`
  for the messages they refused. Treating that as success loses half an album;
  treating it as total failure duplicates the half that arrived. See the delivery
  ladder above for what is done instead.
- **`send_album` unwraps `InputMedia::media`.** `copy_media` sets it from
  `Media::to_raw_input_media`, which is `None` for a link preview — so filtering
  album members on "has media" rather than "converts to an input media" hands
  `grammers` a `None` to unwrap and takes the process down.
- **`Media` being `Some` does not mean there are bytes.** Polls, locations,
  contacts, dice and link previews are all modelled as media with no file
  location behind them. Asking to download one creates an empty file before the
  download reports failure, so `Snapshotter::capture` tests
  `to_raw_input_location` before queueing anything.
- **Album members are not buffered in posting order.** Each arrives on its own
  spawned task and they race for the buffer lock, so the group has to be sorted
  by message ID before delivery. Telegram numbers the members consecutively.
- **Config stores Bot API dialog IDs** (`-100…`), but every API call needs a
  `PeerRef` (ID *plus* the access hash bound to this account). The bridge is the
  session peer cache, warmed by `dialogs::fetch_all`. Calling it after login is
  not optional — `grammers` also needs it to resolve update gaps.
- **The dashboard and the logger both want the terminal.** `ratatui` takes an
  alternate screen buffer on stdout while `tracing` writes to stderr, and one
  warning repaints over the frame. `ui::logger::defer`/`resume` hold log output
  in a bounded buffer for the lifetime of the dashboard and replay it afterwards.
  Suppressing it instead would hide the flood waits and delivery failures the
  dashboard exists to show.
- **Values a hand-edited config can hold that nothing downstream can survive**
  are rejected by `Config::validate`, not handled at runtime: `max_in_flight = 0`
  gives the semaphore no permits and hangs every delivery *and* the shutdown that
  waits for them, and `max_attempts = 0` skips the retry loop body entirely.

### Where files live

Paths come from the `directories` crate, which follows the XDG spec on Linux,
Apple's guidance on macOS and the Known Folder API on Windows. Do not replace
this with a `~/.tgfwd` dotdir: it would be wrong on Windows and would violate the
one part of this that has an actual written specification.

The real problem those paths create — being unguessable, and containing a space
on macOS — is solved by `tgfwd config path` and `tgfwd config edit`, not by
moving the files. `TGFWD_HOME` overrides everything and lays a whole profile out
under one root, which is what the tests and multi-account setups use.

## Conventions

- All code, comments and documentation are **English**. This is an open-source
  project.
- Comments explain *why*, not *what*. If a line needs a comment to say what it
  does, rename something instead.
- Tests live next to the code in `#[cfg(test)] mod tests`. Test names are
  sentences: `a_deleted_source_degrades_to_the_snapshot`.
- Prefer removing dead code over `#[allow(dead_code)]`.
- `unsafe` is forbidden crate-wide, including in tests. If a test seems to need
  it (e.g. `env::set_var`), restructure the code so it does not.

## Commands

```sh
cargo test                    # 109 tests, all offline
cargo clippy --all-targets    # must be clean; pedantic is on
cargo fmt
cargo run -- --help
```

Exceptions to clippy's pedantic set live in `Cargo.toml` under `[lints.clippy]`,
each with a written justification. Add to that list only with a reason.

## What happens when the network drops

Traced rather than assumed, because the behaviour is not what it looks like from
here and the obvious test — pull the network and watch — appears to show nothing
working.

Dropping the link does not fail the socket. Nothing is being sent and nothing is
expected, so the connection simply goes quiet. `grammers` builds connections on
demand and drops failed ones from its pool, which means a brief outage is
absorbed silently and deliberately: a hiccup should not kill the process.

The recovery is driven by `NO_UPDATES_TIMEOUT`, fifteen minutes in
`grammers-session`. After that long without an update the message box asks for a
difference, that request fails while offline — the default retry policy gives up
after one attempt on an I/O error — and the error surfaces through
`UpdateStream::next`, ending the run with a non-zero exit for a supervisor to act
on. Once the link returns, the same difference mechanism replays what was missed,
so an outage costs latency rather than messages, except for anything deleted
while it lasted.

The gap this leaves is observability, not correctness: for up to fifteen minutes
a dead link is indistinguishable from a quiet channel, and the dashboard shows
nothing amiss. Closing that would take an active heartbeat, which is a deliberate
addition of periodic traffic rather than a bug fix.

## Not done yet

CI, release packaging, and publishing to crates.io are deliberately out of scope
so far.
