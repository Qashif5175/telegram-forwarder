# Security policy

## Reporting a vulnerability

Use GitHub's [private vulnerability
reporting](https://github.com/awdr74100/telegram-forwarder/security/advisories/new).
It opens a report only the maintainers can see, so nothing is disclosed while a
fix is being written. Please do not open a public issue for a security problem.

Include what you would put in a bug report — version, platform, what happened —
plus the smallest set of steps that reproduces it. **Do not attach your
configuration file or session file** (see below).

There is no bounty, and no guaranteed response time. This is a small project.

## What this tool holds

Two files matter, and they are not equally sensitive:

| File | What it is |
|---|---|
| `session.json` | **A live credential.** Whoever holds it is signed into the Telegram account, without a password or a login code. It is written `chmod 600` — created with those permissions, not tightened afterwards. |
| `config.toml` | Your `api_id` and `api_hash`. These identify the *application*, not you, but they are still yours and are not meant to be published. |

`tgfwd config path` prints where they live. `tgfwd logout` revokes the session
server-side and deletes the local file.

## In scope

- Anything that discloses the session file or its contents to another user or
  process on the same machine, including through permissions, temporary files,
  logs, error messages or crash output.
- Anything that writes credentials somewhere that is not the session or config
  file.
- A path where messages are delivered to a chat that no configured route names.
- Anything that lets a message from a watched chat cause this tool to execute
  something. Message content is data, and is never interpreted.

## Not in scope

These are design decisions, not defects. If you think one of them should change,
please open an ordinary issue rather than a security report.

- **The session file being a credential at all.** Telegram's protocol offers no
  weaker form of persistent authorization for a user account. Storing it is what
  makes it possible not to log in on every start, and logging in is the most
  rate-limited thing this tool does.
- **`config.toml` holding `api_hash` in plain text.** It is an application
  identifier that every third-party Telegram client stores the same way, and it
  cannot be shipped inside the binary.
- **Automating a user account.** Whether that is acceptable is between you and
  Telegram's terms of service, and doing it aggressively can get an account
  limited. The defaults pace conservatively for exactly that reason.
- **Forwarding content you are not licensed to redistribute.** The tool moves
  what you tell it to move.

## Supported versions

The most recent release, only. There are no maintenance branches.
