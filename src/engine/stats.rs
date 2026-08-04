//! Per-route counters, summarised when the run ends.
//!
//! Deliveries are announced as they happen by the log output; these are the
//! aggregates, which is the part a scrolling log cannot show. They are read once,
//! on the way out.

use std::sync::atomic::{AtomicU64, Ordering};

use dashmap::DashMap;

/// How a message ended up being delivered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Strategy {
    /// A native Telegram forward, attribution intact.
    Forward,
    /// Re-sent as our own message, reusing the original file reference.
    Copy,
    /// Re-sent as our own message, re-uploading bytes from the local snapshot.
    Rehost,
}

impl Strategy {
    pub fn label(self) -> &'static str {
        match self {
            Self::Forward => "forward",
            Self::Copy => "copy",
            Self::Rehost => "rehost",
        }
    }
}

/// Per-route counters.
#[derive(Debug, Default)]
struct RouteCounters {
    delivered: AtomicU64,
    failed: AtomicU64,
    filtered: AtomicU64,
    /// Deliveries that only succeeded because of the local snapshot.
    rescued: AtomicU64,
    by_forward: AtomicU64,
    by_copy: AtomicU64,
    by_rehost: AtomicU64,
}

/// A point-in-time copy of one route's numbers.
#[derive(Debug, Clone, Default)]
pub struct RouteSnapshot {
    pub route: String,
    pub delivered: u64,
    pub failed: u64,
    pub filtered: u64,
    pub rescued: u64,
    pub by_forward: u64,
    pub by_copy: u64,
    pub by_rehost: u64,
}

/// Every route's numbers, taken together.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub routes: Vec<RouteSnapshot>,
}

impl Snapshot {
    /// Totals across every route.
    pub fn totals(&self) -> RouteSnapshot {
        let mut total = RouteSnapshot {
            route: "total".to_owned(),
            ..RouteSnapshot::default()
        };

        for route in &self.routes {
            total.delivered += route.delivered;
            total.failed += route.failed;
            total.filtered += route.filtered;
            total.rescued += route.rescued;
            total.by_forward += route.by_forward;
            total.by_copy += route.by_copy;
            total.by_rehost += route.by_rehost;
        }

        total
    }
}

/// Shared, lock-light statistics.
#[derive(Debug, Default)]
pub struct Stats {
    routes: DashMap<String, RouteCounters>,
}

impl Stats {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record a successful delivery.
    ///
    /// `rescued` marks deliveries that would have been lost without the snapshot,
    /// which is the number that justifies the whole design.
    pub fn delivered(&self, route: &str, strategy: Strategy, rescued: bool) {
        let entry = self.route(route);
        entry.delivered.fetch_add(1, Ordering::Relaxed);
        match strategy {
            Strategy::Forward => entry.by_forward.fetch_add(1, Ordering::Relaxed),
            Strategy::Copy => entry.by_copy.fetch_add(1, Ordering::Relaxed),
            Strategy::Rehost => entry.by_rehost.fetch_add(1, Ordering::Relaxed),
        };
        if rescued {
            entry.rescued.fetch_add(1, Ordering::Relaxed);
        }
    }

    /// Record a delivery that could not be completed.
    pub fn failed(&self, route: &str) {
        self.route(route).failed.fetch_add(1, Ordering::Relaxed);
    }

    /// Record a message dropped by a filter.
    pub fn filtered(&self, route: &str) {
        self.route(route).filtered.fetch_add(1, Ordering::Relaxed);
    }

    /// Make sure a route appears in the summary even with no traffic.
    ///
    /// A route that moved nothing and a route that was never configured look the
    /// same in a report built only from what happened, and those have completely
    /// different causes.
    pub fn register_route(&self, route: &str) {
        self.route(route);
    }

    fn route(&self, route: &str) -> dashmap::mapref::one::Ref<'_, String, RouteCounters> {
        if !self.routes.contains_key(route) {
            self.routes.entry(route.to_owned()).or_default();
        }
        self.routes.get(route).expect("just inserted")
    }

    /// Copy the current numbers for reporting.
    pub fn snapshot(&self) -> Snapshot {
        let mut routes: Vec<RouteSnapshot> = self
            .routes
            .iter()
            .map(|entry| {
                let counters = entry.value();
                RouteSnapshot {
                    route: entry.key().clone(),
                    delivered: counters.delivered.load(Ordering::Relaxed),
                    failed: counters.failed.load(Ordering::Relaxed),
                    filtered: counters.filtered.load(Ordering::Relaxed),
                    rescued: counters.rescued.load(Ordering::Relaxed),
                    by_forward: counters.by_forward.load(Ordering::Relaxed),
                    by_copy: counters.by_copy.load(Ordering::Relaxed),
                    by_rehost: counters.by_rehost.load(Ordering::Relaxed),
                }
            })
            .collect();

        routes.sort_by(|a, b| a.route.cmp(&b.route));
        Snapshot { routes }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_per_route() {
        let stats = Stats::new();
        stats.delivered("a", Strategy::Forward, false);
        stats.delivered("a", Strategy::Copy, true);
        stats.failed("b");

        let snapshot = stats.snapshot();
        let a = snapshot.routes.iter().find(|r| r.route == "a").unwrap();

        assert_eq!(a.delivered, 2);
        assert_eq!(a.rescued, 1);
        assert_eq!(a.by_forward, 1);
        assert_eq!(a.by_copy, 1);

        let b = snapshot.routes.iter().find(|r| r.route == "b").unwrap();
        assert_eq!(b.failed, 1);
    }

    #[test]
    fn totals_add_up_across_routes() {
        let stats = Stats::new();
        stats.delivered("a", Strategy::Forward, false);
        stats.delivered("b", Strategy::Rehost, true);

        let totals = stats.snapshot().totals();
        assert_eq!(totals.delivered, 2);
        assert_eq!(totals.rescued, 1);
        assert_eq!(totals.by_rehost, 1);
    }

    #[test]
    fn a_registered_route_shows_up_before_any_traffic() {
        let stats = Stats::new();
        stats.register_route("idle");

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.routes.len(), 1);
        assert_eq!(snapshot.routes[0].delivered, 0);
    }
}
