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

Three things are written to disk, and they are not equally sensitive:

| What | Why it matters |
|---|---|
| `session.json` | **A live credential.** Whoever holds it is signed into the Telegram account, without a password or a login code. |
| `config.toml` | Your `api_id` and `api_hash`. These identify the *application*, not you, but they are still yours and are not meant to be published. |
| media cache | The bodies of messages this account can see, kept for `snapshot.ttl` so a deleted post can still be delivered. Not a credential, but other people's content. |

All three are created readable and writable by the owner alone, with the
permissions applied *as the file is created* rather than tightened afterwards —
a create-then-`chmod` leaves the contents exposed for as long as the write takes.
The cache is included in that deliberately: `grammers` downloads through
`File::create`, which would leave the mode to the umask, so the file is created
before the download rather than by it.

`tgfwd config path` prints where the configuration lives and `tgfwd status`
prints all three. `tgfwd logout` revokes the session server-side and deletes the
local file.

## In scope

- Anything that discloses the session file, the configuration or the media
  cache to another user or process on the same machine, including through
  permissions, temporary files, logs, error messages or crash output.
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
