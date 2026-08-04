# Contributing

Thanks for looking. This file is the short version; everything about *why* the
code is shaped the way it is lives in [AGENTS.md](AGENTS.md), which is worth
reading before changing anything in `src/engine`.

## Before you write code

Open an issue first if the change is more than a fix. This project is built
around one hostile case — a publisher who posts and deletes a second later — and
several things that look like obvious improvements are load-bearing for it.
AGENTS.md lists those under **Non-negotiable design rules**.

## The loop

```sh
just            # list the recipes
cargo test      # while working
just check      # before pushing — exactly what CI runs
just fix        # rewrite formatting
```

`just check` needs a few tools; `just setup` fetches all but one:

```sh
brew install just actionlint   # macOS; on Windows: winget install --id Casey.Just
just setup
```

`actionlint` is written in Go and is not a crate, which is why it is separate.
The toolchain itself is pinned in `rust-toolchain.toml` and installs on first
build.

## Testing against a real account

There is no mock Telegram. Point `TGFWD_HOME` somewhere disposable so your own
setup is untouched:

```sh
export TGFWD_HOME=/tmp/tgfwd-test
cargo run -- route add
cargo run -- start
rm -rf /tmp/tgfwd-test
```

Two chats you control are enough — a channel you can post in, and Saved Messages
as the target.

## House style

- **English** in code, comments, commit messages and documentation.
- Comments explain *why*. If a line needs a comment to say *what* it does, rename
  something instead.
- Tests live beside the code in `#[cfg(test)] mod tests`, named as sentences:
  `a_deleted_source_degrades_to_the_snapshot`.
- Delete dead code rather than allowing it.
- `unsafe` is forbidden, including in tests.

## Commits and pull requests

[Conventional Commits](https://www.conventionalcommits.org): `feat:`, `fix:`,
`docs:`, `refactor:`, `chore:`, `ci:`, `build:`, `test:`, `perf:`, `style:`,
`revert:`. The changelog is generated from them, so the subject line becomes
release notes someone reads.

Pull requests are squash-merged and **the PR title becomes the commit**, so it is
the title that has to follow the convention — CI checks that one, not the
individual commits.

## Reporting a vulnerability

Privately, not in an issue. See [SECURITY.md](SECURITY.md).
