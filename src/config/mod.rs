//! Loading, validating and persisting the configuration file.

mod model;
mod paths;

use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;

use color_eyre::eyre::{Context, Result, bail};

pub use self::model::{
    Config, DeliveryMode, DispatchPolicy, Filter, MediaKind, PeerLink, Route, SnapshotPolicy,
    TelegramConfig,
};
pub use self::paths::Paths;

/// Header written above a freshly generated config file.
const CONFIG_HEADER: &str = "\
# tgfwd configuration
#
# Peer IDs use the Bot API dialog form (the -100… numbers you see in Telegram
# Desktop). Titles are labels only; run `tgfwd route sync` to refresh them.
#
# Edit this by hand if you like, or use `tgfwd route add` for a guided flow.

";

impl Config {
    /// Read the configuration, returning defaults when the file does not exist.
    ///
    /// A missing file is a normal first-run state, not an error. A malformed file
    /// *is* an error, and the message points at the offending file.
    pub fn load(paths: &Paths) -> Result<Self> {
        let path = paths.config_file();
        if !path.exists() {
            return Ok(Self::default());
        }

        let text = fs_err::read_to_string(&path)
            .wrap_err_with(|| format!("failed to read {}", path.display()))?;

        toml::from_str(&text).wrap_err_with(|| format!("{} is not valid config", path.display()))
    }

    /// Write the configuration atomically.
    ///
    /// The write goes to a sibling temporary file which is then renamed over the
    /// target, so a crash mid-write cannot leave a truncated config behind.
    pub fn save(&self, paths: &Paths) -> Result<()> {
        paths.ensure_dirs()?;
        let path = paths.config_file();

        let mut text = String::from(CONFIG_HEADER);
        let body = toml::to_string_pretty(self).wrap_err("failed to serialize config")?;
        text.push_str(&body);

        let tmp = path.with_extension("toml.tmp");
        fs_err::write(&tmp, text.as_bytes())
            .wrap_err_with(|| format!("failed to write {}", tmp.display()))?;
        fs_err::rename(&tmp, &path)
            .wrap_err_with(|| format!("failed to replace {}", path.display()))?;

        Ok(())
    }

    /// Look up a route by its identifier.
    pub fn route(&self, id: &str) -> Option<&Route> {
        self.routes.iter().find(|route| route.id == id)
    }

    /// Routes that are enabled and therefore participate in forwarding.
    pub fn active_routes(&self) -> impl Iterator<Item = &Route> {
        self.routes.iter().filter(|route| route.enabled)
    }

    /// Reject configurations that cannot work, and explain why.
    ///
    /// This runs before the client connects so that mistakes surface immediately
    /// rather than as a confusing runtime failure.
    ///
    /// Route-level checks apply to enabled routes only, matching the cycle
    /// check: a route that is switched off moves nothing, and refusing to save
    /// the file because of it would leave no way to park a broken route while
    /// fixing it. Enabling one runs this again.
    pub fn validate(&self) -> Result<()> {
        let mut problems = self.defaults.problems();
        let mut seen_ids = HashSet::new();

        for route in &self.routes {
            let label = if route.id.is_empty() {
                "<unnamed route>".to_owned()
            } else {
                format!("route '{}'", route.id)
            };

            if route.id.is_empty() {
                problems.push("a route has an empty id".to_owned());
            } else if !seen_ids.insert(route.id.as_str()) {
                problems.push(format!("duplicate route id '{}'", route.id));
            }

            if route.sources.is_empty() {
                problems.push(format!("{label} has no sources"));
            }
            if route.targets.is_empty() {
                problems.push(format!("{label} has no targets"));
            }

            if !route.enabled {
                continue;
            }

            let sources: HashSet<i64> = route.sources.iter().map(|peer| peer.id).collect();
            for target in &route.targets {
                if sources.contains(&target.id) {
                    problems.push(format!(
                        "{label} forwards {} back into itself",
                        target.label()
                    ));
                }
            }
        }

        if let Some(cycle) = self.find_cycle() {
            problems.push(format!(
                "these chats forward into each other in a loop, which would \
                 multiply messages without end: {cycle}"
            ));
        }

        if problems.is_empty() {
            return Ok(());
        }

        let mut message = String::from("the configuration has problems:");
        for problem in &problems {
            let _ = write!(message, "\n  - {problem}");
        }
        bail!(message)
    }

    /// Detect a forwarding cycle across all enabled routes.
    ///
    /// Each enabled route contributes an edge from every source to every target.
    /// A cycle in that graph means a message would be forwarded back into a chat
    /// that is already being watched, and would keep going around forever.
    ///
    /// Returns a human-readable rendering of one cycle, if any exists.
    fn find_cycle(&self) -> Option<String> {
        let mut graph: HashMap<i64, Vec<i64>> = HashMap::new();
        let mut labels: HashMap<i64, String> = HashMap::new();

        for route in self.active_routes() {
            for source in &route.sources {
                labels.entry(source.id).or_insert_with(|| source.label());
                for target in &route.targets {
                    labels.entry(target.id).or_insert_with(|| target.label());
                    graph.entry(source.id).or_default().push(target.id);
                }
            }
        }

        // Iterative depth-first search tracking the active path, so the cycle can
        // be reported in the order a message would actually travel.
        let mut visited: HashSet<i64> = HashSet::new();
        let mut on_path: HashSet<i64> = HashSet::new();
        let mut path: Vec<i64> = Vec::new();

        for &start in graph.keys() {
            if visited.contains(&start) {
                continue;
            }

            // (node, whether we are entering or leaving it)
            let mut stack = vec![(start, false)];
            while let Some((node, leaving)) = stack.pop() {
                if leaving {
                    on_path.remove(&node);
                    path.pop();
                    continue;
                }

                if on_path.contains(&node) {
                    let cut = path.iter().position(|&n| n == node).unwrap_or(0);
                    let render =
                        |id: &i64| labels.get(id).cloned().unwrap_or_else(|| id.to_string());
                    let mut chain: Vec<String> = path[cut..].iter().map(render).collect();
                    chain.push(render(&node));
                    return Some(chain.join(" -> "));
                }

                if !visited.insert(node) {
                    continue;
                }

                on_path.insert(node);
                path.push(node);
                stack.push((node, true));

                for &next in graph.get(&node).into_iter().flatten() {
                    stack.push((next, false));
                }
            }
        }

        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(id: i64) -> PeerLink {
        PeerLink {
            id,
            title: format!("chat{id}"),
            username: None,
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
    fn accepts_a_plain_many_to_many_route() {
        let config = Config {
            routes: vec![route("mirror", &[-1001, -1002], &[-2001, -2002])],
            ..Config::default()
        };
        config.validate().unwrap();
    }

    #[test]
    fn rejects_dispatch_limits_that_would_deliver_nothing() {
        // Both of these parse happily and then fail silently at runtime: zero
        // permits blocks every delivery task forever, and zero attempts skips
        // the retry loop body entirely.
        let text = "\
[defaults.dispatch]
max_in_flight = 0
max_attempts = 0
";
        let config: Config = toml::from_str(text).unwrap();
        let err = config.validate().unwrap_err().to_string();

        assert!(err.contains("max_in_flight"), "{err}");
        assert!(err.contains("max_attempts"), "{err}");
    }

    #[test]
    fn rejects_a_snapshot_ttl_that_expires_immediately() {
        let text = "\
[defaults.snapshot]
enabled = true
ttl = \"0s\"
";
        let config: Config = toml::from_str(text).unwrap();
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("snapshot.ttl"), "{err}");
    }

    #[test]
    fn the_album_window_is_tunable_and_defaults_to_something_short() {
        // Every other timing knob is configurable; this one used to be a
        // constant, which meant a source whose album parts arrive slowly had no
        // remedy short of a rebuild.
        let text = "\
[defaults.dispatch]
album_window = \"1s\"
";
        let config: Config = toml::from_str(text).unwrap();
        assert_eq!(
            config.defaults.dispatch.album_window,
            std::time::Duration::from_secs(1)
        );
        config.validate().unwrap();

        let fallback = Config::default().defaults.dispatch.album_window;
        assert!(!fallback.is_zero(), "grouping is on unless asked otherwise");
        assert!(fallback < std::time::Duration::from_secs(2), "{fallback:?}");
    }

    #[test]
    fn a_disabled_snapshot_may_have_any_ttl() {
        let text = "\
[defaults.snapshot]
enabled = false
ttl = \"0s\"
";
        let config: Config = toml::from_str(text).unwrap();
        config.validate().unwrap();
    }

    #[test]
    fn the_default_configuration_is_valid() {
        Config::default().validate().unwrap();
    }

    #[test]
    fn rejects_a_route_that_targets_its_own_source() {
        let config = Config {
            routes: vec![route("self", &[-1001], &[-1001])],
            ..Config::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("back into itself"), "{err}");
    }

    #[test]
    fn a_disabled_route_is_not_held_against_the_file() {
        // It moves nothing, and refusing to save would leave no way to park a
        // broken route while fixing it. `route enable` validates again.
        let mut broken = route("self", &[-1001], &[-1001]);
        broken.enabled = false;
        let config = Config {
            routes: vec![broken],
            ..Config::default()
        };
        config.validate().unwrap();
    }

    #[test]
    fn rejects_duplicate_route_ids() {
        let config = Config {
            routes: vec![route("dup", &[-1], &[-2]), route("dup", &[-3], &[-4])],
            ..Config::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("duplicate route id"), "{err}");
    }

    #[test]
    fn detects_a_two_route_cycle() {
        // A -> B and B -> A: every message would bounce forever.
        let config = Config {
            routes: vec![
                route("out", &[-1001], &[-1002]),
                route("back", &[-1002], &[-1001]),
            ],
            ..Config::default()
        };
        let err = config.validate().unwrap_err().to_string();
        assert!(err.contains("loop"), "{err}");
    }

    #[test]
    fn detects_a_three_route_cycle() {
        let config = Config {
            routes: vec![
                route("a", &[-1], &[-2]),
                route("b", &[-2], &[-3]),
                route("c", &[-3], &[-1]),
            ],
            ..Config::default()
        };
        assert!(config.find_cycle().is_some());
    }

    #[test]
    fn a_shared_target_is_not_a_cycle() {
        // Two sources fanning into one target is the normal many-to-many case.
        let config = Config {
            routes: vec![route("a", &[-1], &[-3]), route("b", &[-2], &[-3])],
            ..Config::default()
        };
        assert!(config.find_cycle().is_none());
        config.validate().unwrap();
    }

    #[test]
    fn a_chain_is_not_a_cycle() {
        let config = Config {
            routes: vec![route("a", &[-1], &[-2]), route("b", &[-2], &[-3])],
            ..Config::default()
        };
        assert!(config.find_cycle().is_none());
    }

    #[test]
    fn disabled_routes_do_not_create_cycles() {
        let mut back = route("back", &[-1002], &[-1001]);
        back.enabled = false;
        let config = Config {
            routes: vec![route("out", &[-1001], &[-1002]), back],
            ..Config::default()
        };
        assert!(config.find_cycle().is_none());
    }

    #[test]
    fn round_trips_through_toml() {
        let config = Config {
            routes: vec![route("mirror", &[-1001], &[-2001])],
            ..Config::default()
        };
        let text = toml::to_string_pretty(&config).unwrap();
        let parsed: Config = toml::from_str(&text).unwrap();
        assert_eq!(parsed.routes.len(), 1);
        assert_eq!(parsed.routes[0].id, "mirror");
        assert_eq!(parsed.routes[0].sources[0].id, -1001);
    }
}
