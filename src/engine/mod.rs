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
    /// In-progress albums.
    ///
    /// Keyed by chat as well as by Telegram's grouped ID: the group ID is only
    /// meaningful within a chat, and merging two chats' albums would deliver
    /// each of them to the other's targets.
    albums: Arc<Mutex<HashMap<AlbumKey, Vec<Arc<Snapshot>>>>>,
}

/// A chat and the grouped ID of one album within it.
type AlbumKey = (i64, i64);

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
        let router = Arc::new(Router::build(config));

        let stats = Arc::new(Stats::new());
        for route in router.routes() {
            stats.register_route(route);
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
            router,
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
        let mut stream_failure = None;

        loop {
            // Reap finished delivery tasks so the set does not grow unbounded.
            // A panicking task would otherwise vanish without a trace.
            while let Some(finished) = tasks.try_join_next() {
                if let Err(error) = finished
                    && !error.is_cancelled()
                {
                    tracing::error!(%error, "a delivery task panicked");
                }
            }

            tokio::select! {
                () = &mut shutdown => break,

                _ = housekeeping.tick() => {
                    match self.session.flush() {
                        Ok(true) => tracing::debug!("session persisted"),
                        Ok(false) => {}
                        // Worth saying out loud: a session that cannot be written
                        // means the next start has to log in again, and logging in
                        // is the most flood-limited thing this tool does.
                        Err(error) => tracing::warn!(%error, "could not persist the session"),
                    }
                    self.snapshotter.sweep();
                }

                update = updates.next() => {
                    match update {
                        Ok(update) => self.handle(update, &mut tasks),
                        Err(error) => {
                            tracing::error!(%error, "update stream failed");
                            stream_failure = Some(error);
                            break;
                        }
                    }
                }
            }
        }

        if !tasks.is_empty() {
            tracing::info!(
                pending = tasks.len(),
                "finishing in-flight deliveries — press Ctrl+C again to leave them"
            );
        }
        while tasks.join_next().await.is_some() {}

        // Persisting the update state lets a restart resume where we stopped
        // rather than replaying or skipping messages.
        if let Err(error) = updates.sync_update_state().await {
            tracing::warn!(%error, "could not persist update state");
        }
        self.session.flush()?;

        // Losing the update stream is not a clean stop. Exiting zero would tell
        // a supervisor everything went fine and leave forwarding silently dead.
        if let Some(error) = stream_failure {
            return Err(color_eyre::eyre::eyre!(
                "the connection to Telegram was lost: {error}"
            ));
        }

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
            self.buffer_album((chat_id, group), snapshot, tasks);
        } else {
            let context = self.context();
            dispatch(&context, &[snapshot], tasks);
        }
    }

    /// Collect album members, flushing the group once it stops growing.
    fn buffer_album(&self, key: AlbumKey, snapshot: Arc<Snapshot>, tasks: &mut JoinSet<()>) {
        let albums = Arc::clone(&self.albums);
        let context = self.context();

        tasks.spawn(async move {
            let is_first = {
                let mut buffered = albums.lock().await;
                let entry = buffered.entry(key).or_default();
                entry.push(snapshot);
                entry.len() == 1
            };

            // Only the first member of a group runs the timer; later members
            // just join the buffer before it fires.
            if !is_first {
                return;
            }

            tokio::time::sleep(ALBUM_WINDOW).await;
            let mut members = albums.lock().await.remove(&key).unwrap_or_default();
            if members.is_empty() {
                return;
            }

            order_album(&mut members);

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
            echo: Arc::clone(&self.echo),
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
    echo: Arc<EchoGuard>,
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
        // An album carries its caption on one member, not necessarily the first,
        // so a keyword filter has to see the whole post. Judging it by one part
        // would let `exclude` miss a banned word and `include` drop a group that
        // does mention its keyword.
        text: album_text(snapshots),
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
            let message_id = primary.message_id;

            tasks.spawn(async move {
                let Ok(_permit) = context.permits.acquire().await else {
                    return;
                };

                // The guard is populated once a delivery returns, which can race
                // the update announcing that same delivery. Checking again here,
                // after queueing, closes most of that window without the false
                // positives a content-based guess would bring.
                if context.echo.is_own(source_chat, message_id) {
                    tracing::debug!(route = %route, "skipping a message this tool produced");
                    return;
                }

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
                    .deliver(&route, payload, &target, target_ref, mode)
                    .await
                {
                    tracing::warn!(route = %route, chat = %target.label, %error, "delivery failed");
                }
            });
        }
    }
}

/// Put an album back into the order it was posted in.
///
/// The buffer holds arrival order, and every member is buffered by its own task
/// racing for the same lock, so it is not posting order even when the updates
/// themselves arrive in sequence. Telegram numbers the members of a group
/// consecutively, which makes the message ID the authority.
fn order_album(members: &mut [Arc<Snapshot>]) {
    members.sort_by_key(|snapshot| snapshot.message_id);
}

/// Every distinct piece of text in a post, for filtering purposes.
fn album_text(snapshots: &[Arc<Snapshot>]) -> String {
    let mut parts = snapshots
        .iter()
        .map(|snapshot| snapshot.text.as_str())
        .filter(|text| !text.is_empty());

    let Some(first) = parts.next() else {
        return String::new();
    };

    // The overwhelmingly common case is a single caption; keep it allocation-free
    // in shape by only building a joined string when there is more than one.
    parts.fold(first.to_owned(), |mut joined, part| {
        joined.push('\n');
        joined.push_str(part);
        joined
    })
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

#[cfg(test)]
mod tests {
    use super::*;

    fn album(texts: &[&str]) -> Vec<Arc<Snapshot>> {
        texts
            .iter()
            .enumerate()
            .map(|(index, text)| {
                Arc::new(Snapshot::for_test(
                    i32::try_from(index).expect("small") + 1,
                    text,
                ))
            })
            .collect()
    }

    #[test]
    fn a_lone_caption_is_the_filter_text_unchanged() {
        assert_eq!(album_text(&album(&["breaking news", ""])), "breaking news");
    }

    #[test]
    fn every_part_of_an_album_is_visible_to_the_filter() {
        // The caption is not guaranteed to be on the member that happens to be
        // first, so judging the post by one part would let `exclude` miss a
        // banned word sitting on another.
        let text = album_text(&album(&["", "sponsored", "quiz"]));
        assert!(text.contains("sponsored"), "{text}");
        assert!(text.contains("quiz"), "{text}");
    }

    #[test]
    fn a_captionless_album_yields_no_text() {
        assert_eq!(album_text(&album(&["", "", ""])), "");
        assert_eq!(album_text(&[]), "");
    }

    #[test]
    fn an_album_buffered_out_of_order_is_posted_in_order() {
        // Each member is buffered by its own task racing for the same lock, so
        // arrival order is not posting order even when the updates are in
        // sequence. Delivering them shuffled scrambles the album in the target.
        let mut members = album(&["a", "b", "c"]);
        members.reverse();
        order_album(&mut members);

        let ids: Vec<i32> = members.iter().map(|member| member.message_id).collect();
        assert_eq!(ids, vec![1, 2, 3]);
        // The caption seen by the filter follows the same order.
        assert_eq!(album_text(&members), "a\nb\nc");
    }
}
