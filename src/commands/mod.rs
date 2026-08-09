//! Implementations of the CLI subcommands.

pub mod config;
pub mod route;
pub mod start;

use std::io;

use clap::CommandFactory;
use color_eyre::eyre::{Context, Result, bail};

use crate::cli::Cli;
use crate::config::{Config, Paths, TelegramConfig};
use crate::session::FileSession;
use crate::telegram::{Connection, auth, dialogs};
use crate::ui::prompts::{self, TerminalLoginPrompt};
use crate::ui::theme::{self, Level};

/// Print a status line in the same visual language as the logger.
pub fn say(level: Level, message: impl AsRef<str>) {
    eprintln!("{} {}", level.paint(level.glyph()), message.as_ref());
}

/// Walk the user through obtaining API credentials, then save them.
///
/// Telegram requires a per-developer application key that cannot be shipped in
/// the binary, so this is unavoidable. The least it can do is explain exactly
/// where to click.
async fn ask_for_credentials(config: &mut Config, paths: &Paths) -> Result<()> {
    eprintln!();
    say(
        Level::Info,
        "This tool needs a Telegram API key of your own.",
    );
    eprintln!("  It takes about a minute, and it is free:");
    eprintln!(
        "    1. Open {}",
        theme::accent("https://my.telegram.org/auth")
    );
    eprintln!("    2. Sign in, then choose \"API development tools\"");
    eprintln!("    3. Create an application — any name and description will do");
    eprintln!(
        "    4. Copy the {} and {} it shows you",
        theme::bold("api_id"),
        theme::bold("api_hash")
    );
    eprintln!();

    let api_id = prompts::text("api_id", Some("a number, e.g. 1234567")).await?;
    let api_id: i32 = api_id
        .trim()
        .parse()
        .wrap_err("api_id must be a whole number")?;

    let api_hash = prompts::text("api_hash", Some("a 32-character hex string")).await?;
    let api_hash = api_hash.trim().to_owned();

    if api_hash.len() != 32 {
        bail!(
            "api_hash should be 32 characters long, but that one is {}",
            api_hash.len()
        );
    }

    config.telegram = TelegramConfig { api_id, api_hash };
    config.save(paths)?;

    say(
        Level::Success,
        format!("saved to {}", paths.config_file().display()),
    );
    Ok(())
}

/// Load the config, prompting for credentials if they are missing.
pub async fn load_config_with_credentials(paths: &Paths) -> Result<Config> {
    let mut config = Config::load(paths)?;
    if !config.telegram.is_complete() {
        ask_for_credentials(&mut config, paths).await?;
    }
    Ok(config)
}

/// Open a connection and make sure it is signed in.
///
/// Returns the connection plus the account, so callers can greet the user.
pub async fn connect_signed_in(
    paths: &Paths,
    config: &Config,
) -> Result<(Connection, grammers_client::peer::User)> {
    let connection = Connection::open(paths, &config.telegram)?;

    let mut prompt = TerminalLoginPrompt;
    let user =
        auth::ensure_signed_in(&connection.client, &config.telegram.api_hash, &mut prompt).await?;

    connection.flush_session()?;
    Ok((connection, user))
}

/// `tgfwd login`
pub async fn login(paths: &Paths) -> Result<()> {
    let config = load_config_with_credentials(paths).await?;
    let (connection, user) = connect_signed_in(paths, &config).await?;

    say(
        Level::Success,
        format!("signed in as {}", theme::bold(&user.full_name())),
    );

    // Warming the peer cache now means the route pickers work offline-ish and
    // that `grammers` can resolve update gaps later.
    say(Level::Info, "loading your chats…");
    let chats = dialogs::fetch_all(&connection.client).await?;
    connection.flush_session()?;

    say(
        Level::Success,
        format!(
            "found {} chats — run {} next",
            chats.len(),
            theme::accent("tgfwd route add")
        ),
    );

    connection.shutdown().await
}

/// `tgfwd logout`
pub async fn logout(paths: &Paths) -> Result<()> {
    let config = Config::load(paths)?;
    let session_path = paths.session_file();

    if !session_path.exists() {
        say(Level::Info, "not signed in, nothing to do");
        return Ok(());
    }

    if !prompts::confirm("Sign out and delete the stored session?", false).await? {
        say(Level::Info, "cancelled");
        return Ok(());
    }

    // Best effort: revoke server-side, but always remove the local file, since
    // leaving a stale credential behind is worse than a failed revocation.
    if config.telegram.is_complete() {
        match Connection::open(paths, &config.telegram) {
            Ok(connection) => {
                if let Err(error) = auth::sign_out(&connection.client).await {
                    say(
                        Level::Warn,
                        format!("could not revoke the session server-side: {error}"),
                    );
                }
                let _ = connection.shutdown().await;
            }
            Err(error) => say(Level::Warn, format!("could not connect: {error}")),
        }
    }

    fs_err::remove_file(&session_path)?;
    say(Level::Success, "signed out");
    Ok(())
}

/// `tgfwd status`
pub fn status(paths: &Paths) -> Result<()> {
    let config = Config::load(paths)?;

    eprintln!();
    eprintln!(
        "  {}   {}",
        theme::dim("config "),
        paths.config_file().display()
    );
    eprintln!(
        "  {}   {}",
        theme::dim("session"),
        paths.session_file().display()
    );
    // Listed because it is easy to forget it is there at all: it holds the
    // bodies of snapshotted messages, and the README points here rather than at
    // its own table of platform paths.
    eprintln!(
        "  {}   {}",
        theme::dim("cache  "),
        paths.media_cache_dir().display()
    );
    eprintln!("  {}   {}", theme::dim("account"), describe_account(paths));
    eprintln!();

    if config.routes.is_empty() {
        say(
            Level::Info,
            format!("no routes yet — run {}", theme::accent("tgfwd route add")),
        );
        return Ok(());
    }

    route::print_routes(&config);
    eprintln!("  {}", theme::dim("edit the file with `tgfwd config edit`"));
    Ok(())
}

/// Describe the stored session without connecting.
///
/// The session file exists as soon as anything at all is persisted, so its
/// presence is not evidence that a login ever succeeded; the authorization key
/// inside it is.
fn describe_account(paths: &Paths) -> String {
    if !paths.session_file().exists() {
        return "not signed in — run `tgfwd login`".to_owned();
    }

    match FileSession::load(paths.session_file()).and_then(|session| session.has_authorization()) {
        Ok(true) => theme::accent("signed in"),
        Ok(false) => "session is not authorized — run `tgfwd login`".to_owned(),
        Err(error) => format!("session could not be read: {error}"),
    }
}

/// `tgfwd doctor`
pub async fn doctor(paths: &Paths) -> Result<()> {
    let mut problems = 0;

    // 1. Configuration.
    let config = match Config::load(paths) {
        Ok(config) => {
            say(Level::Success, "config file parses");
            config
        }
        Err(error) => {
            say(Level::Error, format!("config file: {error}"));
            return Err(error);
        }
    };

    match config.validate() {
        Ok(()) => say(Level::Success, "route definitions are consistent"),
        Err(error) => {
            say(Level::Error, format!("{error}"));
            problems += 1;
        }
    }

    if !config.telegram.is_complete() {
        say(
            Level::Error,
            "API credentials are missing — run `tgfwd login`",
        );
        problems += 1;
    }

    // 2. Session.
    if !paths.session_file().exists() {
        say(Level::Error, "not signed in — run `tgfwd login`");
        return finish_doctor(problems + 1);
    }

    let session = FileSession::load(paths.session_file())?;
    say(
        Level::Success,
        format!(
            "session holds {} cached chats",
            session.cached_peer_count()?
        ),
    );

    if config.routes.is_empty() {
        say(Level::Info, "no routes configured yet");
        return finish_doctor(problems);
    }

    // 3. Every configured chat must still be resolvable, or delivery will fail
    // at runtime with a much less obvious error.
    let mut unreachable = Vec::new();
    for route in &config.routes {
        for peer in route.sources.iter().chain(route.targets.iter()) {
            if dialogs::resolve(&session, peer.id).await?.is_none() {
                unreachable.push(format!("{} (route '{}')", peer, route.id));
            }
        }
    }

    if unreachable.is_empty() {
        say(Level::Success, "every configured chat is reachable");
    } else {
        say(
            Level::Error,
            "some chats cannot be resolved by this account:",
        );
        for chat in &unreachable {
            eprintln!("    - {chat}");
        }
        eprintln!(
            "    {}",
            theme::dim(
                "this usually means the account left the chat; `tgfwd login` refreshes the cache"
            )
        );
        problems += unreachable.len();
    }

    finish_doctor(problems)
}

fn finish_doctor(problems: usize) -> Result<()> {
    eprintln!();
    if problems == 0 {
        say(Level::Success, "no problems found");
        return Ok(());
    }

    // Each problem was already printed with an explanation above; this is just
    // the count, and `main` renders it without backtrace noise.
    bail!("{problems} problem(s) found — see above")
}

/// `tgfwd completions <shell>`
pub fn completions(shell: clap_complete::Shell) {
    let mut command = Cli::command();
    let name = command.get_name().to_string();
    clap_complete::generate(shell, &mut command, name, &mut io::stdout());
}

/// Load the dialog list, connecting if necessary.
///
/// Route management needs the chat list, and fetching it fresh is what keeps
/// titles and memberships accurate.
pub async fn fetch_chats(
    paths: &Paths,
    config: &Config,
) -> Result<(Connection, Vec<dialogs::DialogEntry>)> {
    let (connection, _user) = connect_signed_in(paths, config).await?;
    say(Level::Info, "loading your chats…");
    let chats = dialogs::fetch_all(&connection.client).await?;
    connection.flush_session()?;
    Ok((connection, chats))
}
