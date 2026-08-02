//! The on-disk configuration schema.
//!
//! The file is TOML and is meant to be readable and hand-editable. Peers are
//! stored using [Bot API dialog IDs] (the `-100…` form shown by Telegram Desktop
//! and most tooling) rather than the bare `MTProto` identifiers, so a human
//! reading the file recognises the numbers.
//!
//! Every peer entry also carries a `title`, which is *not* authoritative — it is
//! a label refreshed by `tgfwd route sync` purely so the file is legible.
//!
//! [Bot API dialog IDs]: https://core.telegram.org/api/bots/ids

use std::collections::BTreeSet;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

/// Root of the configuration file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// API credentials obtained from <https://my.telegram.org>.
    #[serde(default)]
    pub telegram: TelegramConfig,

    /// Settings applied to every route unless the route overrides them.
    #[serde(default)]
    pub defaults: Defaults,

    /// Forwarding rules. Order is preserved and is the order shown in the UI.
    ///
    /// Skipped when empty so a freshly generated file does not open with a
    /// bare `route = []` above everything else.
    #[serde(default, rename = "route", skip_serializing_if = "Vec::is_empty")]
    pub routes: Vec<Route>,
}

/// Telegram application credentials.
///
/// These identify the *application*, not the account. They are not secret in the
/// way a session file is, but they are still per-developer and are not committed.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TelegramConfig {
    /// `api_id` from my.telegram.org.
    #[serde(default)]
    pub api_id: i32,
    /// `api_hash` from my.telegram.org.
    #[serde(default)]
    pub api_hash: String,
}

impl TelegramConfig {
    /// Whether both credentials have been filled in.
    pub fn is_complete(&self) -> bool {
        self.api_id != 0 && !self.api_hash.is_empty()
    }
}

/// Redacts `api_hash` so it never lands in logs or error reports.
impl fmt::Debug for TelegramConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TelegramConfig")
            .field("api_id", &self.api_id)
            .field("api_hash", &"<redacted>")
            .finish()
    }
}

/// Settings that apply to every route unless overridden.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
#[derive(Default)]
pub struct Defaults {
    /// How a message should be delivered. See [`DeliveryMode`].
    #[serde(default)]
    pub mode: DeliveryMode,

    /// Anti-deletion snapshot policy.
    #[serde(default)]
    pub snapshot: SnapshotPolicy,

    /// Pacing and retry behaviour.
    #[serde(default)]
    pub dispatch: DispatchPolicy,
}

/// How a message is reproduced in the target chat.
///
/// The distinction matters because a native forward is a single cheap RPC that
/// keeps the "Forwarded from" attribution, but it *requires the source message to
/// still exist*. When a publisher deletes a post one second after posting it, a
/// forward that has not been issued yet will simply fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum DeliveryMode {
    /// Race a native forward; if it fails, re-send from the local snapshot.
    ///
    /// This keeps attribution whenever Telegram allows it and still delivers when
    /// the source is gone or the source channel forbids forwarding.
    #[default]
    Auto,

    /// Never forward natively. Always reproduce the message as if it were our own.
    ///
    /// Loses attribution, but is immune to source deletion and to channels that
    /// set the "restrict saving content" flag.
    Copy,

    /// Only ever use a native forward, and report a failure if it does not work.
    ///
    /// Cheapest and fastest, but the least resilient.
    Forward,
}

impl DeliveryMode {
    /// Whether this mode is allowed to issue a native forward.
    pub fn may_forward(self) -> bool {
        matches!(self, Self::Auto | Self::Forward)
    }

    /// Whether this mode is allowed to reproduce the message as our own.
    pub fn may_copy(self) -> bool {
        matches!(self, Self::Auto | Self::Copy)
    }
}

impl fmt::Display for DeliveryMode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Auto => "auto",
            Self::Copy => "copy",
            Self::Forward => "forward",
        };
        f.write_str(s)
    }
}

/// Controls the local snapshot taken to survive source deletion.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SnapshotPolicy {
    /// Whether to download media bytes in the background as insurance.
    ///
    /// Text and media *references* are always captured — they cost nothing. This
    /// flag only controls whether the bytes themselves are fetched, which is the
    /// only thing that survives a file reference becoming invalid.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Skip downloading media larger than this, in bytes.
    ///
    /// Large files rarely finish downloading before a fast deletion anyway, and
    /// re-uploading them is slow. `0` disables the limit.
    #[serde(default = "default_snapshot_max_bytes")]
    pub max_bytes: u64,

    /// How long a snapshot stays on disk before being swept.
    #[serde(default = "default_snapshot_ttl", with = "humantime_serde")]
    pub ttl: Duration,
}

impl Default for SnapshotPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            max_bytes: default_snapshot_max_bytes(),
            ttl: default_snapshot_ttl(),
        }
    }
}

/// Pacing, retry and concurrency behaviour.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DispatchPolicy {
    /// Minimum gap between two deliveries to the *same* target chat.
    ///
    /// Telegram rate-limits per destination, so this is enforced per target
    /// rather than globally: fanning out to ten different channels at once is
    /// fine, hammering one channel is not.
    #[serde(default = "default_per_target_interval", with = "humantime_serde")]
    pub per_target_interval: Duration,

    /// How many delivery attempts to make before giving up on a target.
    #[serde(default = "default_max_attempts")]
    pub max_attempts: u32,

    /// Upper bound on how long to honour a server-issued `FLOOD_WAIT`.
    ///
    /// Telegram occasionally answers with waits measured in hours. Sleeping that
    /// long silently is worse than reporting a failure, so anything above this is
    /// treated as a hard failure for that target.
    #[serde(default = "default_max_flood_wait", with = "humantime_serde")]
    pub max_flood_wait: Duration,

    /// How many messages may be in flight across all routes at once.
    #[serde(default = "default_max_in_flight")]
    pub max_in_flight: usize,
}

impl Default for DispatchPolicy {
    fn default() -> Self {
        Self {
            per_target_interval: default_per_target_interval(),
            max_attempts: default_max_attempts(),
            max_flood_wait: default_max_flood_wait(),
            max_in_flight: default_max_in_flight(),
        }
    }
}

/// A single many-to-many forwarding rule.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Route {
    /// Stable human-chosen identifier, used in the CLI and in logs.
    pub id: String,

    /// Whether this route participates in forwarding.
    #[serde(default = "default_true")]
    pub enabled: bool,

    /// Chats to watch.
    pub sources: Vec<PeerLink>,

    /// Chats to deliver into.
    pub targets: Vec<PeerLink>,

    /// Overrides [`Defaults::mode`] for this route only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mode: Option<DeliveryMode>,

    /// Content filter. An empty filter passes everything.
    #[serde(default, skip_serializing_if = "Filter::is_empty")]
    pub filter: Filter,
}

impl Route {
    /// The effective delivery mode, resolving the route override against defaults.
    pub fn mode(&self, defaults: &Defaults) -> DeliveryMode {
        self.mode.unwrap_or(defaults.mode)
    }
}

/// A reference to a Telegram chat, stored in a form a human can recognise.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PeerLink {
    /// Bot API dialog ID, e.g. `-1001234567890` for a channel.
    pub id: i64,

    /// Display name, refreshed by `tgfwd route sync`. Never used for resolution.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub title: String,

    /// Public `@username` without the `@`, when the chat has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
}

impl PeerLink {
    /// A label suitable for logs and the TUI, preferring the human title.
    pub fn label(&self) -> String {
        if !self.title.is_empty() {
            self.title.clone()
        } else if let Some(username) = &self.username {
            format!("@{username}")
        } else {
            self.id.to_string()
        }
    }
}

impl fmt::Display for PeerLink {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.label(), self.id)
    }
}

/// Content filter applied before a message is dispatched.
///
/// All conditions are `ANDed`. Keyword matching is case-insensitive substring
/// matching against the message text (or media caption).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Filter {
    /// If non-empty, the message must contain at least one of these.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub include: Vec<String>,

    /// If the message contains any of these, it is dropped.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub exclude: Vec<String>,

    /// If non-empty, only these media kinds pass.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    pub kinds: BTreeSet<MediaKind>,

    /// Drop messages that carry no media at all.
    #[serde(default, skip_serializing_if = "is_false")]
    pub require_media: bool,

    /// Drop messages that are themselves forwards from somewhere else.
    #[serde(default, skip_serializing_if = "is_false")]
    pub skip_forwarded: bool,
}

impl Filter {
    /// Whether this filter would accept everything, in which case it is omitted
    /// from the serialized config to keep the file tidy.
    pub fn is_empty(&self) -> bool {
        self == &Self::default()
    }
}

/// Coarse classification of a message's payload, used by [`Filter::kinds`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum MediaKind {
    /// No media: a plain text message.
    Text,
    Photo,
    Video,
    /// A GIF, which Telegram models as a soundless looping video document.
    Animation,
    Audio,
    /// A voice note.
    Voice,
    /// A round video message.
    VideoNote,
    Document,
    Sticker,
    Poll,
    Contact,
    /// A location, venue or live location.
    Geo,
    /// Anything this tool does not classify, including future Telegram types.
    Other,
}

impl fmt::Display for MediaKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let s = match self {
            Self::Text => "text",
            Self::Photo => "photo",
            Self::Video => "video",
            Self::Animation => "animation",
            Self::Audio => "audio",
            Self::Voice => "voice",
            Self::VideoNote => "video-note",
            Self::Document => "document",
            Self::Sticker => "sticker",
            Self::Poll => "poll",
            Self::Contact => "contact",
            Self::Geo => "geo",
            Self::Other => "other",
        };
        f.write_str(s)
    }
}

impl MediaKind {
    /// Every kind, in the order they should be offered in a picker.
    pub const ALL: [Self; 13] = [
        Self::Text,
        Self::Photo,
        Self::Video,
        Self::Animation,
        Self::Audio,
        Self::Voice,
        Self::VideoNote,
        Self::Document,
        Self::Sticker,
        Self::Poll,
        Self::Contact,
        Self::Geo,
        Self::Other,
    ];
}

const fn default_true() -> bool {
    true
}

const fn is_false(value: &bool) -> bool {
    !*value
}

const fn default_snapshot_max_bytes() -> u64 {
    50 * 1024 * 1024
}

const fn default_snapshot_ttl() -> Duration {
    Duration::from_secs(60 * 60)
}

const fn default_per_target_interval() -> Duration {
    Duration::from_millis(300)
}

const fn default_max_attempts() -> u32 {
    5
}

const fn default_max_flood_wait() -> Duration {
    Duration::from_secs(300)
}

const fn default_max_in_flight() -> usize {
    64
}
