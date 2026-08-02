//! Getting a captured message into a target chat.
//!
//! Delivery is a ladder of three strategies, each weaker and more expensive than
//! the last, and each able to survive a failure the one above it cannot:
//!
//! 1. **Forward** — one RPC, keeps "Forwarded from" attribution, and does not
//!    re-upload anything. Fails if the source message is gone or the source chat
//!    restricts saving content.
//! 2. **Copy** — re-send as our own message, reusing the original media by
//!    reference. Immune to the source being deleted, still no re-upload. Fails
//!    if the file reference has gone stale.
//! 3. **Rehost** — re-send using the bytes captured by the snapshot. Works even
//!    when Telegram no longer acknowledges the original file at all.
//!
//! Which rungs are available is set by [`DeliveryMode`]; failures move down the
//! ladder, and only a failure at the bottom is a real failure.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};

use color_eyre::eyre::{Result, bail};
use grammers_client::media::InputMedia;
use grammers_client::message::InputMessage;
use grammers_client::{Client, InvocationError};
use grammers_session::types::PeerRef;
use tokio::sync::Mutex;

use crate::config::{DeliveryMode, DispatchPolicy, MediaKind};

use super::failure::{self, Degrade, Disposition};
use super::router::{EchoGuard, Target};
use super::snapshot::{MediaCache, Snapshot};
use super::stats::{Stats, Strategy};

/// One or more messages delivered as a unit.
///
/// Telegram albums must be sent together or they arrive as unrelated messages,
/// so the album is the delivery unit rather than the individual message.
#[derive(Debug)]
pub struct Payload {
    /// The captured messages, in the order they were posted.
    pub snapshots: Vec<Arc<Snapshot>>,
    /// Reference to the chat they came from, needed for a native forward.
    pub source: PeerRef,
}

impl Payload {
    /// Whether this payload is a grouped album rather than a lone message.
    pub fn is_album(&self) -> bool {
        self.snapshots.len() > 1
    }

    /// Message IDs in the source chat.
    fn message_ids(&self) -> Vec<i32> {
        self.snapshots.iter().map(|snap| snap.message_id).collect()
    }

    /// The snapshot that carries the caption and drives logging.
    fn primary(&self) -> &Arc<Snapshot> {
        &self.snapshots[0]
    }

    /// Age of the oldest snapshot, i.e. end-to-end latency once delivered.
    fn latency(&self) -> Duration {
        self.primary().captured_at.elapsed()
    }
}

/// Enforces a minimum gap between deliveries into the same chat.
///
/// Telegram rate-limits per destination, so this is keyed by target: fanning out
/// to ten chats at once is fine and is exactly what we want, while hammering one
/// chat earns a flood wait that would stall everything queued behind it.
#[derive(Debug)]
pub struct Pacer {
    interval: Duration,
    last_sent: Mutex<HashMap<i64, Instant>>,
}

impl Pacer {
    pub fn new(interval: Duration) -> Self {
        Self {
            interval,
            last_sent: Mutex::new(HashMap::new()),
        }
    }

    /// Wait until it is this target's turn, then claim the slot.
    ///
    /// The slot is reserved while still holding the lock, so two concurrent
    /// deliveries to the same chat queue up instead of both deciding they may
    /// go now.
    pub async fn acquire(&self, target_id: i64) {
        if self.interval.is_zero() {
            return;
        }

        let sleep_for = {
            let mut last = self.last_sent.lock().await;
            let now = Instant::now();

            let wait = last
                .get(&target_id)
                .map(|previous| {
                    let elapsed = now.duration_since(*previous);
                    self.interval.saturating_sub(elapsed)
                })
                .unwrap_or_default();

            last.insert(target_id, now + wait);
            wait
        };

        if !sleep_for.is_zero() {
            tokio::time::sleep(sleep_for).await;
        }
    }
}

/// Shared machinery for delivering payloads.
pub struct Dispatcher {
    client: Client,
    stats: Arc<Stats>,
    echo: Arc<EchoGuard>,
    pacer: Pacer,
    policy: DispatchPolicy,
}

/// Hand-written because `grammers`' `Client` is not `Debug`.
impl std::fmt::Debug for Dispatcher {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Dispatcher")
            .field("policy", &self.policy)
            .finish_non_exhaustive()
    }
}

impl Dispatcher {
    pub fn new(
        client: Client,
        stats: Arc<Stats>,
        echo: Arc<EchoGuard>,
        policy: DispatchPolicy,
    ) -> Self {
        Self {
            client,
            stats,
            echo,
            pacer: Pacer::new(policy.per_target_interval),
            policy,
        }
    }

    /// Deliver `payload` into `target`, walking down the strategy ladder.
    pub async fn deliver(
        &self,
        route: &str,
        payload: &Payload,
        target: &Target,
        target_ref: PeerRef,
        mode: DeliveryMode,
    ) -> Result<()> {
        let _in_flight = self.stats.begin_delivery();

        let ladder = Self::ladder(mode);
        let mut last_error: Option<String> = None;

        for (index, strategy) in ladder.iter().copied().enumerate() {
            match self.attempt(payload, target, target_ref, strategy).await {
                Outcome::Delivered => {
                    // Anything past the first rung only worked because the
                    // snapshot existed; that is the number worth surfacing.
                    let rescued = index > 0;
                    self.stats.delivered(
                        route,
                        strategy,
                        payload.latency(),
                        rescued,
                        format!("{} {} {}", target.label, "←", payload.primary().describe()),
                    );

                    if rescued {
                        tracing::info!(
                            target_chat = %target.label,
                            via = strategy.label(),
                            "rescued a message the source had already deleted"
                        );
                    }

                    return Ok(());
                }

                Outcome::Degraded(reason) => {
                    tracing::debug!(
                        target_chat = %target.label,
                        from = strategy.label(),
                        why = reason.reason(),
                        "falling back to a weaker delivery strategy"
                    );
                    last_error = Some(reason.reason().to_owned());
                }

                Outcome::Failed(message) => {
                    self.stats
                        .failed(route, format!("{}: {message}", target.label));
                    bail!("delivery to {} failed: {message}", target.label);
                }
            }
        }

        let reason = last_error.unwrap_or_else(|| "every delivery strategy failed".to_owned());
        self.stats
            .failed(route, format!("{}: {reason}", target.label));
        bail!("delivery to {} failed: {reason}", target.label)
    }

    /// The strategies available under a given mode, strongest first.
    fn ladder(mode: DeliveryMode) -> Vec<Strategy> {
        let mut ladder = Vec::with_capacity(3);
        if mode.may_forward() {
            ladder.push(Strategy::Forward);
        }
        if mode.may_copy() {
            ladder.push(Strategy::Copy);
            ladder.push(Strategy::Rehost);
        }
        ladder
    }

    /// Run one strategy, retrying it while the failures are retryable.
    async fn attempt(
        &self,
        payload: &Payload,
        target: &Target,
        target_ref: PeerRef,
        strategy: Strategy,
    ) -> Outcome {
        for attempt in 0..self.policy.max_attempts {
            self.pacer.acquire(target.id).await;

            let result = match strategy {
                Strategy::Forward => self.send_forward(payload, target_ref).await,
                Strategy::Copy => self.send_copy(payload, target_ref).await,
                Strategy::Rehost => match self.send_rehost(payload, target_ref).await {
                    Ok(ids) => Ok(ids),
                    // A missing snapshot is not a Telegram failure, so it cannot
                    // be classified; it simply ends the ladder.
                    Err(RehostError::NoBytes) => {
                        return Outcome::Failed("no snapshot available to re-upload".to_owned());
                    }
                    Err(RehostError::Api(err)) => Err(err),
                },
            };

            match result {
                Ok(message_ids) => {
                    // Record what we produced so the router does not treat our
                    // own delivery as new source material.
                    for id in message_ids {
                        self.echo.remember(target.id, id);
                    }
                    return Outcome::Delivered;
                }

                Err(error) => match failure::classify(&error) {
                    Disposition::Wait(delay) => {
                        if delay > self.policy.max_flood_wait {
                            return Outcome::Failed(format!(
                                "Telegram asked for a {}s wait, beyond the configured limit",
                                delay.as_secs()
                            ));
                        }
                        tracing::warn!(
                            target_chat = %target.label,
                            seconds = delay.as_secs(),
                            "waiting out a rate limit"
                        );
                        let _waiting = self.stats.begin_wait();
                        tokio::time::sleep(delay).await;
                    }

                    Disposition::Backoff => {
                        tokio::time::sleep(failure::backoff_delay(attempt)).await;
                    }

                    Disposition::Degrade(reason) => return Outcome::Degraded(reason),

                    Disposition::Fatal(message) => return Outcome::Failed(message.to_owned()),
                },
            }
        }

        Outcome::Failed(format!(
            "gave up after {} attempts",
            self.policy.max_attempts
        ))
    }

    /// Strategy 1: a native Telegram forward.
    async fn send_forward(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Vec<i32>, InvocationError> {
        let ids = payload.message_ids();
        let results = self
            .client
            .forward_messages(target_ref, &ids, payload.source)
            .await?;

        // A forward can partially succeed: Telegram returns `None` for the
        // messages it refused. Treating that as success would silently lose
        // half an album.
        let delivered: Vec<i32> = results
            .iter()
            .flatten()
            .map(grammers_client::message::Message::id)
            .collect();

        if delivered.len() < ids.len() {
            return Err(InvocationError::Rpc(grammers_client::sender::RpcError {
                code: 400,
                name: "MESSAGE_ID_INVALID".to_owned(),
                value: None,
                caused_by: None,
            }));
        }

        Ok(delivered)
    }

    /// Strategy 2: re-send as our own message, reusing the media by reference.
    async fn send_copy(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Vec<i32>, InvocationError> {
        if payload.is_album() {
            let album: Vec<InputMedia> = payload
                .snapshots
                .iter()
                .filter_map(|snap| {
                    let media = snap.media.as_ref()?;
                    Some(
                        InputMedia::new()
                            .caption(snap.text.clone())
                            .fmt_entities(snap.entities.clone())
                            .copy_media(media),
                    )
                })
                .collect();

            if !album.is_empty() {
                let sent = self.client.send_album(target_ref, album).await?;
                return Ok(sent
                    .iter()
                    .flatten()
                    .map(grammers_client::message::Message::id)
                    .collect());
            }
        }

        let snap = payload.primary();
        let mut input = InputMessage::new()
            .text(snap.text.clone())
            .fmt_entities(snap.entities.clone());

        if let Some(media) = &snap.media {
            input = input.copy_media(media);
        }

        let sent = self.client.send_message(target_ref, input).await?;
        Ok(vec![sent.id()])
    }

    /// Strategy 3: re-upload from the snapshotted bytes.
    async fn send_rehost(
        &self,
        payload: &Payload,
        target_ref: PeerRef,
    ) -> Result<Vec<i32>, RehostError> {
        let snap = payload.primary();

        // A text-only message never needed bytes in the first place.
        if snap.media.is_none() {
            if snap.text.is_empty() {
                return Err(RehostError::NoBytes);
            }
            let input = InputMessage::new()
                .text(snap.text.clone())
                .fmt_entities(snap.entities.clone());
            let sent = self
                .client
                .send_message(target_ref, input)
                .await
                .map_err(RehostError::Api)?;
            return Ok(vec![sent.id()]);
        }

        let MediaCache::Ready(path) = snap.await_media().await else {
            return Err(RehostError::NoBytes);
        };

        let uploaded = self
            .client
            .upload_file(&path)
            .await
            .map_err(|err| RehostError::Api(InvocationError::Io(err)))?;

        let mut input = InputMessage::new()
            .text(snap.text.clone())
            .fmt_entities(snap.entities.clone());

        // Photos must be sent as photos to stay viewable inline; everything else
        // travels as a document.
        input = if snap.kind == MediaKind::Photo {
            input.photo(uploaded)
        } else {
            input.document(uploaded)
        };

        let sent = self
            .client
            .send_message(target_ref, input)
            .await
            .map_err(RehostError::Api)?;

        Ok(vec![sent.id()])
    }
}

/// Result of running one strategy to completion.
#[derive(Debug)]
enum Outcome {
    Delivered,
    /// This strategy cannot work; try the next rung down.
    Degraded(Degrade),
    /// Nothing further will help.
    Failed(String),
}

/// Rehosting can fail for a reason that is not a Telegram error.
#[derive(Debug)]
enum RehostError {
    /// The snapshot has no bytes to re-upload.
    NoBytes,
    Api(InvocationError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_mode_offers_the_whole_ladder() {
        assert_eq!(
            Dispatcher::ladder(DeliveryMode::Auto),
            vec![Strategy::Forward, Strategy::Copy, Strategy::Rehost]
        );
    }

    #[test]
    fn forward_mode_never_falls_back_to_copying() {
        // Deliberate: a user choosing `forward` wants attribution or nothing.
        assert_eq!(
            Dispatcher::ladder(DeliveryMode::Forward),
            vec![Strategy::Forward]
        );
    }

    #[test]
    fn copy_mode_skips_the_native_forward() {
        assert_eq!(
            Dispatcher::ladder(DeliveryMode::Copy),
            vec![Strategy::Copy, Strategy::Rehost]
        );
    }

    #[tokio::test]
    async fn the_pacer_spaces_out_deliveries_to_one_chat() {
        let pacer = Pacer::new(Duration::from_millis(120));
        let start = Instant::now();

        pacer.acquire(-1001).await;
        pacer.acquire(-1001).await;

        assert!(
            start.elapsed() >= Duration::from_millis(110),
            "the second delivery should have waited"
        );
    }

    #[tokio::test]
    async fn the_pacer_does_not_delay_different_chats() {
        let pacer = Pacer::new(Duration::from_millis(200));
        let start = Instant::now();

        // Fanning out across many targets is the whole point; it must be fast.
        pacer.acquire(-1001).await;
        pacer.acquire(-1002).await;
        pacer.acquire(-1003).await;

        assert!(
            start.elapsed() < Duration::from_millis(100),
            "different chats must not queue behind each other"
        );
    }

    #[tokio::test]
    async fn a_zero_interval_disables_pacing() {
        let pacer = Pacer::new(Duration::ZERO);
        let start = Instant::now();
        for _ in 0..50 {
            pacer.acquire(-1001).await;
        }
        assert!(start.elapsed() < Duration::from_millis(50));
    }
}
