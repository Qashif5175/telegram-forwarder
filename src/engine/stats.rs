//! Live counters and a recent-event feed.
//!
//! Both the log output and the dashboard read from here, so the numbers a user
//! sees are the same numbers in both views.

use std::collections::VecDeque;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

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

/// How many latency samples to keep per route for the rolling average.
const LATENCY_WINDOW: usize = 64;

/// How many recent events the dashboard can show.
const EVENT_BUFFER: usize = 256;

/// One line in the recent-activity feed.
#[derive(Debug, Clone)]
pub struct Event {
    pub at: chrono::DateTime<chrono::Local>,
    pub route: String,
    pub outcome: Outcome,
    pub detail: String,
}

/// The result of one delivery attempt, as shown in the feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Delivered,
    Rescued,
    Failed,
    Filtered,
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
    latencies: Mutex<VecDeque<Duration>>,
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
    /// Mean of the recent latency window.
    pub average_latency: Option<Duration>,
    /// Slowest delivery in the recent window.
    pub worst_latency: Option<Duration>,
}

/// Everything the dashboard needs to render one frame.
#[derive(Debug, Clone)]
pub struct Snapshot {
    pub uptime: Duration,
    pub routes: Vec<RouteSnapshot>,
    pub events: Vec<Event>,
    pub in_flight: u64,
    /// Deliveries currently sleeping on a server-issued wait.
    pub waiting: u64,
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
#[derive(Debug)]
pub struct Stats {
    started: Instant,
    routes: DashMap<String, RouteCounters>,
    events: Mutex<VecDeque<Event>>,
    in_flight: AtomicU64,
    waiting: AtomicU64,
}

impl Default for Stats {
    fn default() -> Self {
        Self::new()
    }
}

impl Stats {
    pub fn new() -> Self {
        Self {
            started: Instant::now(),
            routes: DashMap::new(),
            events: Mutex::new(VecDeque::with_capacity(EVENT_BUFFER)),
            in_flight: AtomicU64::new(0),
            waiting: AtomicU64::new(0),
        }
    }

    /// Record a successful delivery.
    ///
    /// `rescued` marks deliveries that would have been lost without the snapshot,
    /// which is the number that justifies the whole design.
    pub fn delivered(
        &self,
        route: &str,
        strategy: Strategy,
        latency: Duration,
        rescued: bool,
        detail: impl Into<String>,
    ) {
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

        if let Ok(mut window) = entry.latencies.lock() {
            window.push_back(latency);
            while window.len() > LATENCY_WINDOW {
                window.pop_front();
            }
        }
        drop(entry);

        self.push_event(
            route,
            if rescued {
                Outcome::Rescued
            } else {
                Outcome::Delivered
            },
            detail,
        );
    }

    /// Record a delivery that could not be completed.
    pub fn failed(&self, route: &str, detail: impl Into<String>) {
        self.route(route).failed.fetch_add(1, Ordering::Relaxed);
        self.push_event(route, Outcome::Failed, detail);
    }

    /// Record a message dropped by a filter.
    pub fn filtered(&self, route: &str, detail: impl Into<String>) {
        self.route(route).filtered.fetch_add(1, Ordering::Relaxed);
        self.push_event(route, Outcome::Filtered, detail);
    }

    /// Track a delivery entering flight; the returned guard decrements on drop.
    pub fn begin_delivery(&self) -> InFlightGuard<'_> {
        self.in_flight.fetch_add(1, Ordering::Relaxed);
        InFlightGuard { stats: self }
    }

    /// Track a delivery sleeping on a flood wait.
    pub fn begin_wait(&self) -> WaitGuard<'_> {
        self.waiting.fetch_add(1, Ordering::Relaxed);
        WaitGuard { stats: self }
    }

    /// Make sure a route shows up in the dashboard before it has any traffic.
    pub fn register_route(&self, route: &str) {
        self.route(route);
    }

    fn route(&self, route: &str) -> dashmap::mapref::one::Ref<'_, String, RouteCounters> {
        if !self.routes.contains_key(route) {
            self.routes.entry(route.to_owned()).or_default();
        }
        self.routes.get(route).expect("just inserted")
    }

    fn push_event(&self, route: &str, outcome: Outcome, detail: impl Into<String>) {
        let Ok(mut events) = self.events.lock() else {
            return;
        };

        events.push_back(Event {
            at: chrono::Local::now(),
            route: route.to_owned(),
            outcome,
            detail: detail.into(),
        });

        while events.len() > EVENT_BUFFER {
            events.pop_front();
        }
    }

    /// Copy the current numbers for rendering.
    pub fn snapshot(&self) -> Snapshot {
        let mut routes: Vec<RouteSnapshot> = self
            .routes
            .iter()
            .map(|entry| {
                let counters = entry.value();
                let window = counters.latencies.lock().ok();

                let (average, worst) = window.as_ref().map_or((None, None), |window| {
                    if window.is_empty() {
                        (None, None)
                    } else {
                        let sum: Duration = window.iter().sum();
                        (
                            Some(sum / u32::try_from(window.len()).unwrap_or(1)),
                            window.iter().max().copied(),
                        )
                    }
                });

                RouteSnapshot {
                    route: entry.key().clone(),
                    delivered: counters.delivered.load(Ordering::Relaxed),
                    failed: counters.failed.load(Ordering::Relaxed),
                    filtered: counters.filtered.load(Ordering::Relaxed),
                    rescued: counters.rescued.load(Ordering::Relaxed),
                    by_forward: counters.by_forward.load(Ordering::Relaxed),
                    by_copy: counters.by_copy.load(Ordering::Relaxed),
                    by_rehost: counters.by_rehost.load(Ordering::Relaxed),
                    average_latency: average,
                    worst_latency: worst,
                }
            })
            .collect();

        routes.sort_by(|a, b| a.route.cmp(&b.route));

        let events = self
            .events
            .lock()
            .map(|events| events.iter().cloned().collect())
            .unwrap_or_default();

        Snapshot {
            uptime: self.started.elapsed(),
            routes,
            events,
            in_flight: self.in_flight.load(Ordering::Relaxed),
            waiting: self.waiting.load(Ordering::Relaxed),
        }
    }
}

/// Decrements the in-flight counter when dropped.
#[derive(Debug)]
pub struct InFlightGuard<'a> {
    stats: &'a Stats,
}

impl Drop for InFlightGuard<'_> {
    fn drop(&mut self) {
        self.stats.in_flight.fetch_sub(1, Ordering::Relaxed);
    }
}

/// Decrements the waiting counter when dropped.
#[derive(Debug)]
pub struct WaitGuard<'a> {
    stats: &'a Stats,
}

impl Drop for WaitGuard<'_> {
    fn drop(&mut self) {
        self.stats.waiting.fetch_sub(1, Ordering::Relaxed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_accumulate_per_route() {
        let stats = Stats::new();
        stats.delivered(
            "a",
            Strategy::Forward,
            Duration::from_millis(100),
            false,
            "x",
        );
        stats.delivered("a", Strategy::Copy, Duration::from_millis(300), true, "y");
        stats.failed("b", "nope");

        let snapshot = stats.snapshot();
        let a = snapshot.routes.iter().find(|r| r.route == "a").unwrap();

        assert_eq!(a.delivered, 2);
        assert_eq!(a.rescued, 1);
        assert_eq!(a.by_forward, 1);
        assert_eq!(a.by_copy, 1);
        assert_eq!(a.average_latency, Some(Duration::from_millis(200)));
        assert_eq!(a.worst_latency, Some(Duration::from_millis(300)));

        let b = snapshot.routes.iter().find(|r| r.route == "b").unwrap();
        assert_eq!(b.failed, 1);
    }

    #[test]
    fn totals_add_up_across_routes() {
        let stats = Stats::new();
        stats.delivered("a", Strategy::Forward, Duration::from_millis(10), false, "");
        stats.delivered("b", Strategy::Rehost, Duration::from_millis(10), true, "");

        let totals = stats.snapshot().totals();
        assert_eq!(totals.delivered, 2);
        assert_eq!(totals.rescued, 1);
        assert_eq!(totals.by_rehost, 1);
    }

    #[test]
    fn in_flight_returns_to_zero_when_guards_drop() {
        let stats = Stats::new();
        {
            let _one = stats.begin_delivery();
            let _two = stats.begin_delivery();
            assert_eq!(stats.snapshot().in_flight, 2);
        }
        assert_eq!(stats.snapshot().in_flight, 0);
    }

    #[test]
    fn the_event_feed_is_bounded() {
        let stats = Stats::new();
        for i in 0..(EVENT_BUFFER + 50) {
            stats.failed("a", format!("event {i}"));
        }

        let snapshot = stats.snapshot();
        assert_eq!(snapshot.events.len(), EVENT_BUFFER);
        // The oldest entries are the ones dropped.
        assert!(snapshot.events[0].detail.contains("event 50"));
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
