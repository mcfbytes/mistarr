//! The SSE event bus: a ring of recent events for `Last-Event-ID` replay plus
//! a broadcast channel for live subscribers. Event names are in `docs/API.md` "Events".

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use serde::Serialize;
use tokio::sync::broadcast;

/// Events kept for replay.
pub const RING_SIZE: usize = 256;

/// The `event:` name of an SSE message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventKind {
    /// `status`: the `/system/status` body.
    Status,
    /// `job.progress`: `{ id, kind, progress }`.
    JobProgress,
    /// `dat.loaded`: `{ dat_version_id?, file }`.
    DatLoaded,
    /// `dat.rejected`: `{ file, reason }`.
    DatRejected,
    /// `source.changed`: `{ source_id, state, platform_id? }`.
    SourceChanged,
    /// `download.changed`: `{ download_id, state, progress }`.
    DownloadChanged,
    /// `import.done`: `{ title_id, file_id, action }`.
    ImportDone,
    /// `file.changed`: `{ file_id, state }`, throttled by the publisher.
    FileChanged,
}

impl EventKind {
    /// The name sent on the `event:` line.
    ///
    /// ```
    /// use mistarr_server::events::EventKind;
    /// assert_eq!(EventKind::JobProgress.as_str(), "job.progress");
    /// ```
    #[must_use]
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Status => "status",
            Self::JobProgress => "job.progress",
            Self::DatLoaded => "dat.loaded",
            Self::DatRejected => "dat.rejected",
            Self::SourceChanged => "source.changed",
            Self::DownloadChanged => "download.changed",
            Self::ImportDone => "import.done",
            Self::FileChanged => "file.changed",
        }
    }
}

/// Broadcast buffer per subscriber. Larger than the ring, so a subscriber is
/// only closed for lagging once the ring could not have replayed the gap anyway.
pub const CHANNEL_SIZE: usize = RING_SIZE * 4;

/// One published event. `data` is the serialised JSON payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// The publishing bus's epoch.
    pub epoch: u64,
    /// Increasing sequence within the epoch, starting at 1.
    pub seq: u64,
    /// The event name.
    pub kind: EventKind,
    /// JSON text for the `data:` line.
    pub data: String,
}

impl Event {
    /// The SSE `id:` value, `<epoch hex>-<seq>`.
    ///
    /// ```
    /// use mistarr_server::events::{Event, EventKind};
    /// let e = Event { epoch: 255, seq: 3, kind: EventKind::Status, data: "{}".into() };
    /// assert_eq!(e.id(), "ff-3");
    /// ```
    #[must_use]
    pub fn id(&self) -> String {
        format!("{:x}-{}", self.epoch, self.seq)
    }
}

/// Splits an SSE id into `(epoch, seq)`; `None` when it is not one this server issues.
///
/// ```
/// use mistarr_server::events::parse_id;
/// assert_eq!(parse_id("ff-3"), Some((255, 3)));
/// assert_eq!(parse_id("3"), None);
/// ```
#[must_use]
pub fn parse_id(id: &str) -> Option<(u64, u64)> {
    let (epoch, seq) = id.trim().split_once('-')?;
    Some((u64::from_str_radix(epoch, 16).ok()?, seq.parse().ok()?))
}

/// Fan-out of server events to SSE connections.
pub struct EventBus {
    epoch: u64,
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Arc<Event>>,
}

struct Ring {
    next_seq: u64,
    events: VecDeque<Arc<Event>>,
}

/// What a new subscriber receives: events to replay, then the live stream.
pub struct Subscription {
    /// Ring events after the client's `Last-Event-ID`, oldest first.
    pub replay: Vec<Arc<Event>>,
    /// False when events the client has not seen may be missing from
    /// `replay`: a different epoch, or an id older than the ring.
    pub complete: bool,
    /// Every event published after `replay` was taken.
    pub live: broadcast::Receiver<Arc<Event>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// An empty bus whose epoch is the current time in nanoseconds, so ids
    /// from a previous process never match.
    ///
    /// ```
    /// assert_eq!(mistarr_server::events::EventBus::new().latest_seq(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map_or(0, |d| d.as_nanos());
        // Truncation keeps the low 64 bits, which change every nanosecond.
        #[allow(clippy::cast_possible_truncation)]
        Self::with_epoch(nanos as u64)
    }

    /// An empty bus with a given epoch.
    ///
    /// ```
    /// assert_eq!(mistarr_server::events::EventBus::with_epoch(7).epoch(), 7);
    /// ```
    #[must_use]
    pub fn with_epoch(epoch: u64) -> Self {
        let (tx, _) = broadcast::channel(CHANNEL_SIZE);
        Self {
            epoch,
            ring: Mutex::new(Ring {
                next_seq: 1,
                events: VecDeque::with_capacity(RING_SIZE),
            }),
            tx,
        }
    }

    /// This bus's epoch.
    ///
    /// ```
    /// let bus = mistarr_server::events::EventBus::new();
    /// assert!(bus.epoch() > 0);
    /// ```
    #[must_use]
    pub fn epoch(&self) -> u64 {
        self.epoch
    }

    /// Records and broadcasts an event, returning its sequence number.
    ///
    /// ```
    /// use mistarr_server::events::{EventBus, EventKind};
    /// let bus = EventBus::new();
    /// assert_eq!(bus.publish(EventKind::Status, &serde_json::json!({})), 1);
    /// ```
    pub fn publish<T: Serialize + ?Sized>(&self, kind: EventKind, data: &T) -> u64 {
        let data = serde_json::to_string(data).unwrap_or_else(|e| {
            tracing::warn!(event = kind.as_str(), error = %e, "event payload not serialisable");
            "null".to_owned()
        });
        let mut ring = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = ring.next_seq;
        ring.next_seq += 1;
        let event = Arc::new(Event {
            epoch: self.epoch,
            seq,
            kind,
            data,
        });
        if ring.events.len() == RING_SIZE {
            ring.events.pop_front();
        }
        ring.events.push_back(Arc::clone(&event));
        // Sending under the lock keeps replay and live free of gaps and duplicates.
        let _ = self.tx.send(event);
        seq
    }

    /// Subscribes with the client's `Last-Event-ID`. `None` replays nothing.
    /// An id from this epoch replays the ring after it; any other id replays
    /// the whole ring and reports the replay incomplete.
    ///
    /// ```
    /// use mistarr_server::events::{EventBus, EventKind};
    /// let bus = EventBus::with_epoch(1);
    /// bus.publish(EventKind::Status, &1);
    /// bus.publish(EventKind::Status, &2);
    /// assert_eq!(bus.subscribe(Some("1-1")).replay.len(), 1);
    /// assert!(bus.subscribe(None).replay.is_empty());
    /// ```
    pub fn subscribe(&self, last_id: Option<&str>) -> Subscription {
        let ring = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let live = self.tx.subscribe();
        let oldest = ring.events.front().map_or(ring.next_seq, |e| e.seq);
        let (replay, complete) = match last_id.map(parse_id) {
            None => (Vec::new(), true),
            Some(Some((epoch, last))) if epoch == self.epoch && last < ring.next_seq => {
                let replay = ring.events.iter().filter(|e| e.seq > last).cloned();
                (replay.collect(), last + 1 >= oldest)
            }
            Some(_) => (ring.events.iter().cloned().collect(), false),
        };
        Subscription {
            replay,
            complete,
            live,
        }
    }

    /// The sequence number of the newest event, or 0 before the first.
    ///
    /// ```
    /// use mistarr_server::events::{EventBus, EventKind};
    /// let bus = EventBus::new();
    /// bus.publish(EventKind::FileChanged, &0);
    /// assert_eq!(bus.latest_seq(), 1);
    /// ```
    #[must_use]
    pub fn latest_seq(&self) -> u64 {
        self.ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_seq
            - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ids(sub: &Subscription) -> Vec<u64> {
        sub.replay.iter().map(|e| e.seq).collect()
    }

    #[test]
    fn ring_keeps_the_newest_events() {
        let bus = EventBus::with_epoch(9);
        for i in 0..300 {
            bus.publish(EventKind::FileChanged, &i);
        }
        let sub = bus.subscribe(Some("9-0"));
        assert_eq!(sub.replay.len(), RING_SIZE);
        assert!(!sub.complete, "events 1..44 were evicted");
        assert_eq!(sub.replay[0].seq, 300 - RING_SIZE as u64 + 1);
        assert_eq!(sub.replay.last().map(|e| e.data.as_str()), Some("299"));
        let from_ring_start = format!("9-{}", 300 - RING_SIZE);
        assert!(bus.subscribe(Some(&from_ring_start)).complete);
    }

    #[test]
    fn replay_after_id_in_the_same_epoch() {
        let bus = EventBus::with_epoch(0xab);
        for i in 0..5 {
            bus.publish(EventKind::Status, &i);
        }
        let sub = bus.subscribe(Some("ab-3"));
        assert_eq!(ids(&sub), [4, 5]);
        assert!(sub.complete);
        assert!(bus.subscribe(Some("ab-5")).replay.is_empty());
        assert_eq!(bus.subscribe(Some("ab-99")).replay.len(), 5);
    }

    #[test]
    fn restart_with_a_smaller_old_id_replays_the_whole_ring() {
        let old = EventBus::with_epoch(1);
        for i in 0..10 {
            old.publish(EventKind::Status, &i);
        }
        let last_seen = format!("{:x}-2", old.epoch());
        let new = EventBus::with_epoch(2);
        for i in 0..5 {
            new.publish(EventKind::DatLoaded, &i);
        }
        let sub = new.subscribe(Some(&last_seen));
        assert_eq!(ids(&sub), [1, 2, 3, 4, 5]);
        assert!(!sub.complete);
        let garbage = new.subscribe(Some("not-an-id"));
        assert_eq!(garbage.replay.len(), 5);
        assert!(!garbage.complete);
    }

    #[test]
    fn epochs_differ_between_buses() {
        let a = EventBus::new();
        std::thread::sleep(std::time::Duration::from_millis(1));
        assert_ne!(a.epoch(), EventBus::new().epoch());
    }

    #[tokio::test]
    async fn live_receives_after_subscribe() {
        let bus = EventBus::with_epoch(3);
        bus.publish(EventKind::Status, &"old");
        let mut sub = bus.subscribe(None);
        bus.publish(EventKind::ImportDone, &serde_json::json!({"title_id": 1}));
        let got = sub.live.recv().await.expect("event");
        assert_eq!(got.id(), "3-2");
        assert_eq!(got.kind, EventKind::ImportDone);
        assert_eq!(got.data, r#"{"title_id":1}"#);
    }

    #[tokio::test]
    async fn a_subscriber_can_fall_a_full_ring_behind_without_lagging() {
        let bus = EventBus::with_epoch(4);
        let mut sub = bus.subscribe(None);
        for i in 0..RING_SIZE + 10 {
            bus.publish(EventKind::FileChanged, &i);
        }
        let first = sub.live.recv().await.expect("not lagged");
        assert_eq!(first.seq, 1);
    }

    #[test]
    fn ids_parse() {
        assert_eq!(parse_id(" 1f-10 "), Some((31, 10)));
        assert_eq!(parse_id("x-1"), None);
        assert_eq!(parse_id("1-x"), None);
    }

    #[test]
    fn names_match_the_api_doc() {
        let names: Vec<_> = [
            EventKind::Status,
            EventKind::JobProgress,
            EventKind::DatLoaded,
            EventKind::DatRejected,
            EventKind::SourceChanged,
            EventKind::DownloadChanged,
            EventKind::ImportDone,
            EventKind::FileChanged,
        ]
        .map(EventKind::as_str)
        .to_vec();
        assert_eq!(
            names,
            [
                "status",
                "job.progress",
                "dat.loaded",
                "dat.rejected",
                "source.changed",
                "download.changed",
                "import.done",
                "file.changed"
            ]
        );
    }
}
