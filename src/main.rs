//! `tgfwd` — many-to-many Telegram forwarding.
//!
//! See `AGENTS.md` for the architecture and the reasoning behind it.

mod cli;
mod commands;
mod config;
mod engine;
mod session;
mod telegram;
mod ui;

use clap::Parser;
use color_eyre::eyre::Result;

use crate::cli::{Cli, Command, RouteCommand};
use crate::config::Paths;
use crate::ui::prompts;
use crate::ui::theme::{self, Level};

/// Exit code used when the user cancels a prompt, matching the shell convention
/// for "terminated by Ctrl+C".
const EXIT_CANCELLED: i32 = 130;

fn main() -> Result<()> {
    color_eyre::install()?;

    let cli = Cli::parse();
    ui::logger::init(cli.verbose).map_err(|err| color_eyre::eyre::eyre!("{err}"))?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;

    match runtime.block_on(dispatch(cli)) {
        Ok(()) => Ok(()),
        Err(error) if prompts::is_cancellation(&error) => {
            commands::say(Level::Info, "cancelled");
            std::process::exit(EXIT_CANCELLED);
        }
        Err(error) => {
            // Almost everything that fails here is a situation the user can fix:
            // a wrong route name, a missing login, a chat they left. Rendering
            // those through `color_eyre` buries the sentence that matters under
            // a location and a backtrace notice. Real bugs still surface in
            // full, because panics go through color_eyre's panic hook instead.
            commands::say(Level::Error, error.to_string());
            for cause in error.chain().skip(1) {
                eprintln!("    {}", theme::dim(&format!("caused by: {cause}")));
            }
            std::process::exit(1);
        }
    }
}

async fn dispatch(cli: Cli) -> Result<()> {
    let paths = Paths::resolve()?;

    match cli.command {
        Command::Login => commands::login(&paths).await,
        Command::Logout => commands::logout(&paths).await,
        Command::Status => commands::status(&paths),
        Command::Doctor => commands::doctor(&paths).await,
        Command::Completions { shell } => {
            commands::completions(shell);
            Ok(())
        }
        Command::Start { tui, catch_up } => commands::start::run(&paths, tui, catch_up).await,
        Command::Route { action } => match action {
            RouteCommand::Add => commands::route::add(&paths).await,
            RouteCommand::List => commands::route::list(&paths),
            RouteCommand::Edit { route } => commands::route::edit(&paths, route).await,
            RouteCommand::Remove { route } => commands::route::remove(&paths, route).await,
            RouteCommand::Enable { route } => {
                commands::route::set_enabled(&paths, route, true).await
            }
            RouteCommand::Disable { route } => {
                commands::route::set_enabled(&paths, route, false).await
            }
            RouteCommand::Sync => commands::route::sync(&paths).await,
        },
    }
}
