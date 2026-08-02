//! The forwarding engine.
//!
//! # Shape of the hot path
//!
//! An update arrives, and within the same tick it is either discarded or
//! *captured*. Capture is synchronous and cannot fail, which is what makes the
//! rest of the pipeline safe to take its time: once a message is captured, a
//! publisher deleting it one second later can no longer take it away.
//!
//! Delivery then fans out to every target concurrently. Targets never queue
//! behind each other — a slow or rate-limited chat must not delay the other
//! nine — and each target independently walks the strategy ladder described in
//! [`delivery`].

pub mod delivery;
pub mod failure;
pub mod filter;
pub mod router;
pub mod snapshot;
pub mod stats;

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

use color_eyre::eyre::Result;
use grammers_client::client::UpdateStream;
use grammers_client::update::Update;
use tokio::sync::{Mutex, Semaphore};
use tokio::task::JoinSet;

use crate::config::Config;
use crate::session::FileSession;
use crate::telegram::dialogs;

use self::delivery::{Dispatcher, Payload};
use self::filter::Candidate;
use self::router::{EchoGuard, Router};
use self::snapshot::{Snapshot, Snapshotter};
use self::stats::Stats;

/// How long to wait for the remaining parts of an album to arrive.
///
/// Telegram delivers the members of a grouped post as separate updates within a
/// few hundred milliseconds. Waiting is safe because every part has already been
/// captured by the time the timer starts: the delay costs latency, never
/// content.
const ALBUM_WINDOW: Duration = Duration::from_millis(400);

/// How often to persist the session and sweep the media cache.
const HOUSEKEEPING_INTERVAL: Duration = Duration::from_secs(30);

/// The running forwarder.
pub struct Engine {
    session: Arc<FileSession>,
    router: Arc<Router>,
    dispatcher: Arc<Dispatcher>,
    snapshotter: Snapshotter,
    stats: Arc<Stats>,
    echo: Arc<EchoGuard>,
    permits: Arc<Semaphore>,
    /// In-progress albums, keyed by Telegram's grouped ID.
    albums: Arc<Mutex<HashMap<i64, Vec<Arc<Snapshot>>>>>,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("sources", &self.router.source_count())
            .finish_non_exhaustive()
    }
}

impl Engine {
    /// Assemble an engine from a validated configuration.
    pub fn new(
        client: grammers_client::Client,
        session: Arc<FileSession>,
        config: &Config,
        cache_dir: std::path::PathBuf,
    ) -> Self {
        let stats = Arc::new(Stats::new());
        for route in config.active_routes() {
            stats.register_route(&route.id);
        }

        let echo = Arc::new(EchoGuard::default());
        let dispatcher = Arc::new(Dispatcher::new(
            client.clone(),
            Arc::clone(&stats),
            Arc::clone(&echo),
            config.defaults.dispatch.clone(),
        ));

        Self {
            snapshotter: Snapshotter::new(client, cache_dir, config.defaults.snapshot.clone()),
            router: Arc::new(Router::build(config)),
            permits: Arc::new(Semaphore::new(config.defaults.dispatch.max_in_flight)),
            session,
            dispatcher,
            stats,
            echo,
            albums: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    /// Live statistics, shared with the dashboard.
    pub fn stats(&self) -> Arc<Stats> {
        Arc::clone(&self.stats)
    }

    /// Number of chats being watched.
    pub fn source_count(&self) -> usize {
        self.router.source_count()
    }

    /// Run until `shutdown` resolves.
    pub async fn run<F>(self, mut updates: UpdateStream, shutdown: F) -> Result<()>
    where
        F: Future<Output = ()> + Send,
    {
        let mut housekeeping = tokio::time::interval(HOUSEKEEPING_INTERVAL);
        housekeeping.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        // The first tick fires immediately, which we do not want.
        housekeeping.tick().await;

        let mut tasks = JoinSet::new();
        tokio::pin!(shutdown);

        loop {
            // Reap finished delivery tasks so the set does not grow unbounded.
            while tasks.try_join_next().is_some() {}

            tokio::select! {
                () = &mut shutdown => break,

                _ = housekeeping.tick() => {
                    if let Ok(true) = self.session.flush() {
                        tracing::debug!("session persisted");
                    }
                    self.snapshotter.sweep();
                }

                update = updates.next() => {
                    match update {
                        Ok(update) => self.handle(update, &mut tasks),
                        Err(error) => {
                            tracing::error!(%error, "update stream failed");
                            break;
                        }
                    }
                }
            }
        }

        tracing::info!("finishing in-flight deliveries…");
        while tasks.join_next().await.is_some() {}

        // Persisting the update state lets a restart resume where we stopped
        // rather than replaying or skipping messages.
        if let Err(error) = updates.sync_update_state().await {
            tracing::warn!(%error, "could not persist update state");
        }
        self.session.flush()?;

        Ok(())
    }

    /// Route a single update.
    fn handle(&self, update: Update, tasks: &mut JoinSet<()>) {
        let Update::NewMessage(message) = update else {
            return;
        };

        let Some(chat_id) = message.peer_id().bot_api_dialog_id() else {
            return;
        };

        // Never re-process a message this tool produced, or a chain of routes
        // would multiply it without end.
        if self.echo.is_own(chat_id, message.id()) {
            return;
        }

        if !self.router.watches(chat_id) {
            return;
        }

        // Capture first, decide later: everything after this point survives the
        // source being deleted.
        let snapshot = self.snapshotter.capture(chat_id, &message);

        if let Some(group) = snapshot.grouped_id {
            self.buffer_album(group, snapshot, tasks);
        } else {
            let context = self.context();
            dispatch(&context, &[snapshot], tasks);
        }
    }

    /// Collect album members, flushing the group once it stops growing.
    fn buffer_album(&self, group: i64, snapshot: Arc<Snapshot>, tasks: &mut JoinSet<()>) {
        let albums = Arc::clone(&self.albums);
        let context = self.context();

        tasks.spawn(async move {
            let is_first = {
                let mut buffered = albums.lock().await;
                let entry = buffered.entry(group).or_default();
                entry.push(snapshot);
                entry.len() == 1
            };

            // Only the first member of a group runs the timer; later members
            // just join the buffer before it fires.
            if !is_first {
                return;
            }

            tokio::time::sleep(ALBUM_WINDOW).await;
            let members = albums.lock().await.remove(&group).unwrap_or_default();
            if members.is_empty() {
                return;
            }

            let mut inner = JoinSet::new();
            dispatch(&context, &members, &mut inner);
            while inner.join_next().await.is_some() {}
        });
    }

    /// Bundle everything a detached delivery task needs.
    fn context(&self) -> Context {
        Context {
            router: Arc::clone(&self.router),
            dispatcher: Arc::clone(&self.dispatcher),
            session: Arc::clone(&self.session),
            stats: Arc::clone(&self.stats),
            permits: Arc::clone(&self.permits),
        }
    }
}

/// The subset of the engine a detached delivery task needs.
#[derive(Clone)]
struct Context {
    router: Arc<Router>,
    dispatcher: Arc<Dispatcher>,
    session: Arc<FileSession>,
    stats: Arc<Stats>,
    permits: Arc<Semaphore>,
}

/// Fan a payload out to every target of every matching route.
///
/// Filtering happens once per route; delivery is spawned once per target so that
/// the targets proceed independently of each other.
fn dispatch(context: &Context, snapshots: &[Arc<Snapshot>], tasks: &mut JoinSet<()>) {
    let Some(primary) = snapshots.first().cloned() else {
        return;
    };

    let candidate = Candidate {
        text: primary.text.clone(),
        kind: primary.kind,
        is_forward: primary.is_forward,
    };

    for binding in context.router.bindings_for(primary.source_chat) {
        if let Err(rejection) = filter::evaluate(&binding.filter, &candidate) {
            context.stats.filtered(&binding.route, rejection.reason());
            tracing::debug!(
                route = %binding.route,
                why = rejection.reason(),
                "message filtered out"
            );
            continue;
        }

        for target in &binding.targets {
            let context = context.clone();
            let snapshots = snapshots.to_vec();
            let route = binding.route.clone();
            let target = target.clone();
            let mode = binding.mode;
            let source_chat = primary.source_chat;

            tasks.spawn(async move {
                let Ok(_permit) = context.permits.acquire().await else {
                    return;
                };

                let Some(source_ref) = resolve(&context, &route, source_chat, "source").await
                else {
                    return;
                };
                let Some(target_ref) = resolve(&context, &route, target.id, &target.label).await
                else {
                    return;
                };

                let payload = Payload {
                    snapshots,
                    source: source_ref,
                };

                if let Err(error) = context
                    .dispatcher
                    .deliver(&route, &payload, &target, target_ref, mode)
                    .await
                {
                    tracing::warn!(route = %route, chat = %target.label, %error, "delivery failed");
                }
            });
        }
    }
}

/// Resolve a chat ID against the session cache, reporting failures once.
async fn resolve(
    context: &Context,
    route: &str,
    chat_id: i64,
    label: &str,
) -> Option<grammers_session::types::PeerRef> {
    if let Ok(Some(peer)) = dialogs::resolve(&context.session, chat_id).await {
        Some(peer)
    } else {
        // The account is probably no longer a member of the chat. Telling
        // the user which one is far more useful than a generic failure.
        context.stats.failed(
            route,
            format!("{label} ({chat_id}) is not reachable by this account"),
        );
        tracing::warn!(route = %route, chat = %label, chat_id, "chat is not in the session cache");
        None
    }
}
