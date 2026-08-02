//! Command-line surface.

use clap::{Parser, Subcommand};
use clap_complete::Shell;

/// Many-to-many Telegram forwarding, built for posts that get deleted seconds
/// after they appear.
#[derive(Debug, Parser)]
#[command(name = "tgfwd", version, about, long_about = None)]
pub struct Cli {
    /// Increase log detail. Repeat for more (`-vv` includes the Telegram stack).
    #[arg(short, long, action = clap::ArgAction::Count, global = true)]
    pub verbose: u8,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Sign in to Telegram and store the session.
    Login,

    /// Sign out and delete the stored session.
    Logout,

    /// Create and manage forwarding routes.
    Route {
        #[command(subcommand)]
        action: RouteCommand,
    },

    /// Start forwarding.
    Start {
        /// Show the live dashboard instead of scrolling logs.
        #[arg(long)]
        tui: bool,

        /// Also process messages that arrived while this tool was not running.
        #[arg(long)]
        catch_up: bool,
    },

    /// Show the configured routes and account without connecting.
    Status,

    /// Work with the configuration file directly.
    Config {
        #[command(subcommand)]
        action: ConfigCommand,
    },

    /// Check the configuration and connection for problems.
    Doctor,

    /// Print a shell completion script.
    Completions {
        /// Shell to generate completions for.
        shell: Shell,
    },
}

#[derive(Debug, Subcommand)]
pub enum ConfigCommand {
    /// Open the configuration file in your editor.
    Edit,

    /// Print the path to the configuration file, for scripts and editors.
    Path,
}

#[derive(Debug, Subcommand)]
pub enum RouteCommand {
    /// Create a route by picking chats from your dialog list.
    Add,

    /// List configured routes.
    List,

    /// Change a route's sources, targets, delivery mode or filter.
    Edit {
        /// Not needed — leave it out and pick from a list. Naming a route
        /// (see `tgfwd route list`) skips the prompt, for scripts.
        route: Option<String>,
    },

    /// Delete a route.
    Remove {
        /// Not needed — leave it out and pick from a list. Naming a route
        /// (see `tgfwd route list`) skips the prompt, for scripts.
        route: Option<String>,
    },

    /// Turn a route on.
    Enable {
        /// Not needed — leave it out and pick from a list. Naming a route
        /// (see `tgfwd route list`) skips the prompt, for scripts.
        route: Option<String>,
    },

    /// Turn a route off without deleting it.
    Disable {
        /// Not needed — leave it out and pick from a list. Naming a route
        /// (see `tgfwd route list`) skips the prompt, for scripts.
        route: Option<String>,
    },

    /// Refresh the chat titles stored in the config file.
    Sync,
}
