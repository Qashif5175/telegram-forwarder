//! `tgfwd start` — bring the engine up and keep it running.

use std::sync::Arc;

use color_eyre::eyre::{Result, bail};

use crate::config::{Config, Paths};
use crate::engine::Engine;
use crate::ui::theme::{self, Level};

use super::say;

/// Start forwarding until interrupted.
pub async fn run(paths: &Paths, catch_up: bool) -> Result<()> {
    let config = super::load_config_with_credentials(paths).await?;
    config.validate()?;

    if config.active_routes().next().is_none() {
        if config.routes.is_empty() {
            bail!("no routes configured — run `tgfwd route add` first");
        }
        bail!("every route is disabled — enable one with `tgfwd route enable <id>`");
    }

    let (mut connection, user) = super::connect_signed_in(paths, &config).await?;
    say(
        Level::Success,
        format!("signed in as {}", theme::bold(&user.full_name())),
    );

    // Refreshing dialogs does two things: it keeps the peer cache complete, so
    // configured chats resolve, and it is what lets `grammers` recover from
    // update gaps rather than silently missing messages.
    say(Level::Info, "refreshing chat list…");
    let chats = crate::telegram::dialogs::fetch_all(&connection.client).await?;
    connection.flush_session()?;
    tracing::debug!(count = chats.len(), "peer cache warmed");

    let updates = connection.stream_updates(catch_up).await?;

    let engine = Engine::new(
        connection.client.clone(),
        Arc::clone(&connection.session),
        &config,
        paths.media_cache_dir().to_path_buf(),
    );
    let stats = engine.stats();

    announce(&config, engine.source_count());

    // Ctrl+C is the shutdown signal for both presentation modes.
    let (shutdown_tx, shutdown_rx) = tokio::sync::watch::channel(false);
    watch_for_interrupt(shutdown_tx.clone());

    let engine_shutdown = {
        let mut rx = shutdown_rx.clone();
        async move {
            // `wait_for` returns as soon as the flag is already set.
            let _ = rx.wait_for(|flag| *flag).await;
        }
    };

    let result = engine.run(updates, engine_shutdown).await;

    // Reported before the result is propagated: the numbers are worth seeing
    // whether or not the run ended cleanly.
    report_totals(&stats);
    result?;

    say(Level::Info, "shutting down…");
    connection.shutdown().await
}

/// Turn Ctrl+C into a shutdown request, and a second one into an exit.
///
/// A delivery sitting out a server-issued flood wait can hold the shutdown for
/// as long as `max_flood_wait` allows — five minutes by default. Waiting is the
/// right default, because those messages are still going to arrive, but the user
/// has to be able to change their mind. `tokio` keeps its handler installed for
/// the life of the process, so without this loop every press after the first is
/// swallowed and there is no way out short of `kill`.
fn watch_for_interrupt(shutdown: tokio::sync::watch::Sender<bool>) {
    tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_err() {
            return;
        }
        let _ = shutdown.send(true);

        if tokio::signal::ctrl_c().await.is_ok() {
            say(Level::Warn, "interrupted again — leaving without waiting");
            // Deliberately abrupt: the alternative is ignoring the user.
            std::process::exit(130);
        }
    });
}

/// Print what is about to happen, so the user can confirm it matches intent.
fn announce(config: &Config, sources: usize) {
    let routes: Vec<_> = config.active_routes().collect();
    let targets: usize = routes.iter().map(|route| route.targets.len()).sum();

    eprintln!();
    say(
        Level::Success,
        format!(
            "watching {} chat(s) across {} route(s), delivering to {} target(s)",
            theme::bold(&sources.to_string()),
            theme::bold(&routes.len().to_string()),
            theme::bold(&targets.to_string()),
        ),
    );
    say(Level::Info, theme::dim("press Ctrl+C to stop"));
    eprintln!();
}

/// Summarise the session on exit.
///
/// This is the only place aggregates are shown. A scrolling log announces each
/// delivery as it happens but cannot answer "how did the run go", and for a tool
/// left running unattended that question is asked at the end.
fn report_totals(stats: &crate::engine::stats::Stats) {
    let snapshot = stats.snapshot();
    let totals = snapshot.totals();

    eprintln!();
    say(
        Level::Info,
        format!(
            "delivered {} · rescued {} · failed {} · filtered {}",
            totals.delivered, totals.rescued, totals.failed, totals.filtered
        ),
    );

    // Which rungs of the ladder did the work. `copy` and `rehost` are the ones
    // that cost something to reach, so seeing how often they were needed is what
    // says whether the fallbacks are earning their place.
    if totals.delivered > 0 {
        say(
            Level::Info,
            theme::dim(&format!(
                "by forward {} · copy {} · rehost {}",
                totals.by_forward, totals.by_copy, totals.by_rehost
            )),
        );
    }

    // Per route, but only when there is more than one to tell apart.
    if snapshot.routes.len() > 1 {
        for route in &snapshot.routes {
            eprintln!(
                "    {}  {}",
                theme::accent(&route.route),
                theme::dim(&format!(
                    "delivered {} · rescued {} · failed {} · filtered {}",
                    route.delivered, route.rescued, route.failed, route.filtered
                ))
            );
        }
    }

    if totals.rescued > 0 {
        say(
            Level::Success,
            format!(
                "{} message(s) would have been lost without the local snapshot",
                totals.rescued
            ),
        );
    }
}
