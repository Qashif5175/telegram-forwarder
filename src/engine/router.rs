//! Mapping an incoming message to the places it should go.
//!
//! Lookup happens on every single update in every watched chat, so it is a hash
//! lookup against a table built once at startup rather than a scan over routes.

use std::collections::{BTreeSet, HashMap, HashSet, VecDeque};
use std::sync::Mutex;

use crate::config::{Config, DeliveryMode, Filter, PeerLink};

/// Where a message should be delivered, and how.
#[derive(Debug, Clone)]
pub struct Binding {
    /// The route this came from, used in logs and statistics.
    pub route: String,
    /// Chats to deliver into.
    pub targets: Vec<Target>,
    /// Effective delivery mode, with the route override already resolved.
    pub mode: DeliveryMode,
    /// Content filter for this route.
    pub filter: Filter,
}

/// A single delivery destination.
#[derive(Debug, Clone)]
pub struct Target {
    /// Bot API dialog ID.
    pub id: i64,
    /// Human label for logs.
    pub label: String,
}

impl From<&PeerLink> for Target {
    fn from(link: &PeerLink) -> Self {
        Self {
            id: link.id,
            label: link.label(),
        }
    }
}

/// The precomputed source-to-targets table.
#[derive(Debug, Default)]
pub struct Router {
    table: HashMap<i64, Vec<Binding>>,
}

impl Router {
    /// Build the routing table from the enabled routes in `config`.
    pub fn build(config: &Config) -> Self {
        let mut table: HashMap<i64, Vec<Binding>> = HashMap::new();

        for route in config.active_routes() {
            let targets: Vec<Target> = route.targets.iter().map(Target::from).collect();

            for source in &route.sources {
                // A chat that is both a source and a target of the same route
                // would echo forever; config validation rejects it, but guard
                // here too so a hand-edited file cannot cause an incident.
                let targets: Vec<Target> = targets
                    .iter()
                    .filter(|target| target.id != source.id)
                    .cloned()
                    .collect();

                if targets.is_empty() {
                    continue;
                }

                table.entry(source.id).or_default().push(Binding {
                    route: route.id.clone(),
                    targets,
                    mode: route.mode(&config.defaults),
                    filter: route.filter.clone(),
                });
            }
        }

        Self { table }
    }

    /// Bindings for a source chat, or an empty slice when it is not watched.
    pub fn bindings_for(&self, source_id: i64) -> &[Binding] {
        self.table.get(&source_id).map_or(&[], Vec::as_slice)
    }

    /// Whether any route watches this chat.
    pub fn watches(&self, source_id: i64) -> bool {
        self.table.contains_key(&source_id)
    }

    /// Number of source chats in the table.
    pub fn source_count(&self) -> usize {
        self.table.len()
    }

    /// Every route that survived into the table, once each.
    ///
    /// Not the same as the enabled routes in the config: one whose every target
    /// is also its own source is dropped above, and reporting on it afterwards
    /// would show a row that can never move.
    pub fn routes(&self) -> BTreeSet<&str> {
        self.table
            .values()
            .flatten()
            .map(|binding| binding.route.as_str())
            .collect()
    }
}

/// Remembers messages this tool itself produced.
///
/// Without this, a chain like `A -> B` plus `B -> C` would re-forward our own
/// delivery into B onward to C. Checking `outgoing` is not enough: a user who
/// owns a source channel posts outgoing messages there too, and those are
/// exactly the messages they want forwarded.
///
/// The set is bounded and evicts oldest-first, since it only needs to cover the
/// window between us sending a message and the corresponding update arriving.
#[derive(Debug)]
pub struct EchoGuard {
    inner: Mutex<EchoGuardInner>,
    capacity: usize,
}

#[derive(Debug, Default)]
struct EchoGuardInner {
    seen: HashSet<(i64, i32)>,
    order: VecDeque<(i64, i32)>,
}

impl EchoGuard {
    /// Create a guard remembering at most `capacity` recent deliveries.
    pub fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(EchoGuardInner::default()),
            capacity: capacity.max(1),
        }
    }

    /// Take the lock, carrying on through a panic in another thread.
    ///
    /// Poisoning here would otherwise be catastrophic out of proportion to its
    /// cause: every delivery consults this guard, so one panicking task would
    /// turn every subsequent message into a panic of its own. What the lock
    /// protects is a set and a queue mutated by nothing more complicated than
    /// insert and pop, so there is no torn state to protect a reader from —
    /// recovering the contents and continuing is strictly better than taking
    /// the forwarder down with it.
    fn lock(&self) -> std::sync::MutexGuard<'_, EchoGuardInner> {
        self.inner.lock().unwrap_or_else(|poisoned| {
            tracing::warn!("recovering the echo guard after a panic in another task");
            poisoned.into_inner()
        })
    }

    /// Record a message we just produced in `chat_id`.
    pub fn remember(&self, chat_id: i64, message_id: i32) {
        let key = (chat_id, message_id);
        let mut inner = self.lock();

        if inner.seen.insert(key) {
            inner.order.push_back(key);
            while inner.order.len() > self.capacity {
                if let Some(oldest) = inner.order.pop_front() {
                    inner.seen.remove(&oldest);
                }
            }
        }
    }

    /// Whether this message is one we produced.
    pub fn is_own(&self, chat_id: i64, message_id: i32) -> bool {
        self.lock().seen.contains(&(chat_id, message_id))
    }
}

impl Default for EchoGuard {
    fn default() -> Self {
        Self::new(4096)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Route;

    fn peer(id: i64) -> PeerLink {
        PeerLink {
            id,
            title: format!("chat{id}"),
            username: None,
        }
    }

    fn config_with(routes: Vec<Route>) -> Config {
        Config {
            routes,
            ..Config::default()
        }
    }

    fn route(id: &str, sources: &[i64], targets: &[i64]) -> Route {
        Route {
            id: id.to_owned(),
            enabled: true,
            sources: sources.iter().copied().map(peer).collect(),
            targets: targets.iter().copied().map(peer).collect(),
            mode: None,
            filter: Filter::default(),
        }
    }

    #[test]
    fn every_source_maps_to_every_target() {
        let router = Router::build(&config_with(vec![route(
            "mirror",
            &[-1001, -1002],
            &[-2001, -2002, -2003],
        )]));

        assert_eq!(router.source_count(), 2);
        for source in [-1001, -1002] {
            let bindings = router.bindings_for(source);
            assert_eq!(bindings.len(), 1);
            assert_eq!(bindings[0].targets.len(), 3);
        }
    }

    #[test]
    fn several_routes_watching_one_chat_all_fire() {
        let router = Router::build(&config_with(vec![
            route("a", &[-1001], &[-2001]),
            route("b", &[-1001], &[-3001]),
        ]));

        let bindings = router.bindings_for(-1001);
        assert_eq!(bindings.len(), 2);
    }

    #[test]
    fn an_unwatched_chat_yields_nothing() {
        let router = Router::build(&config_with(vec![route("a", &[-1001], &[-2001])]));
        assert!(router.bindings_for(-9999).is_empty());
        assert!(!router.watches(-9999));
    }

    #[test]
    fn disabled_routes_are_left_out_of_the_table() {
        let mut disabled = route("off", &[-1001], &[-2001]);
        disabled.enabled = false;
        let router = Router::build(&config_with(vec![disabled]));
        assert_eq!(router.source_count(), 0);
    }

    #[test]
    fn a_target_equal_to_its_source_is_dropped() {
        // Config validation rejects this, but a hand-edited file should not be
        // able to create an echo loop either.
        let router = Router::build(&config_with(vec![route("self", &[-1001], &[-1001, -2001])]));

        let bindings = router.bindings_for(-1001);
        assert_eq!(bindings.len(), 1);
        assert_eq!(bindings[0].targets.len(), 1);
        assert_eq!(bindings[0].targets[0].id, -2001);
    }

    #[test]
    fn a_route_whose_only_target_is_its_source_disappears() {
        let router = Router::build(&config_with(vec![route("self", &[-1001], &[-1001])]));
        assert!(router.bindings_for(-1001).is_empty());
    }

    #[test]
    fn only_routes_that_can_move_something_are_listed() {
        // These are what get registered for the summary, and a line that can
        // never move is worse than no line: it reads as a route silently failing.
        let router = Router::build(&config_with(vec![
            route("live", &[-1001], &[-2001]),
            route("self", &[-3001], &[-3001]),
        ]));

        assert_eq!(router.routes(), BTreeSet::from(["live"]));
    }

    #[test]
    fn a_route_watching_several_chats_is_listed_once() {
        let router = Router::build(&config_with(vec![route(
            "mirror",
            &[-1001, -1002],
            &[-2001],
        )]));
        assert_eq!(router.routes().len(), 1);
    }

    #[test]
    fn the_echo_guard_recognises_our_own_messages() {
        let guard = EchoGuard::new(16);
        guard.remember(-2001, 55);

        assert!(guard.is_own(-2001, 55));
        assert!(!guard.is_own(-2001, 56), "a different message is not ours");
        assert!(
            !guard.is_own(-3001, 55),
            "same id in another chat is not ours"
        );
    }

    #[test]
    fn the_echo_guard_forgets_oldest_first() {
        let guard = EchoGuard::new(2);
        guard.remember(-1, 1);
        guard.remember(-1, 2);
        guard.remember(-1, 3);

        assert!(!guard.is_own(-1, 1), "the oldest entry should be evicted");
        assert!(guard.is_own(-1, 2));
        assert!(guard.is_own(-1, 3));
    }
}
