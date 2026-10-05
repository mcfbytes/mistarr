//! The SSE event bus: a ring of recent events for `Last-Event-ID` replay plus
//! a broadcast channel for live subscribers. Event names are in `docs/API.md` "Events".

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, PoisonError};

use mistarr_core::PlatformId;
use serde::Serialize;
use serde_json::Value;
use tokio::sync::broadcast;

use crate::db::downloads::DownloadState;
use crate::db::files::FileState;
use crate::db::ids::{DatVersionId, DownloadId, FileId, JobId, SourceId, TitleId};
use crate::db::imports::ImportAction;
use crate::db::jobs::JobState;
use crate::db::sources::SourceState;
use crate::jobs::JobKind;
use crate::status::Status;

/// Events kept for replay.
pub const RING_SIZE: usize = 256;

/// The `event:` name of an SSE message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum EventKind {
    /// `status`.
    Status,
    /// `job.progress`.
    JobProgress,
    /// `dat.loaded`.
    DatLoaded,
    /// `dat.rejected`.
    DatRejected,
    /// `source.changed`.
    SourceChanged,
    /// `download.changed`.
    DownloadChanged,
    /// `import.done`.
    ImportDone,
    /// `file.changed`.
    FileChanged,
}

impl EventKind {
    /// Every name, in declaration order.
    pub const ALL: [Self; 8] = [
        Self::Status,
        Self::JobProgress,
        Self::DatLoaded,
        Self::DatRejected,
        Self::SourceChanged,
        Self::DownloadChanged,
        Self::ImportDone,
        Self::FileChanged,
    ];

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

/// The `job.progress` payload; `docs/API.md` "Events".
#[derive(Debug, Clone, Serialize)]
pub struct JobProgress<'a> {
    /// The job.
    pub id: JobId,
    /// What it does.
    pub kind: JobKind,
    /// Its state now.
    pub state: JobState,
    /// The file, platform or source it is about.
    pub detail: Option<&'a str>,
    /// Its progress, `null` while queued.
    pub progress: &'a Value,
}

/// The `dat.loaded` payload.
#[derive(Debug, Clone, Serialize)]
pub struct DatLoaded<'a> {
    /// The version just stored.
    pub dat_version_id: DatVersionId,
    /// The file as dropped.
    pub file: &'a str,
    /// The platform it was bound to, when it was.
    pub platform_id: Option<&'a PlatformId>,
}

/// The `dat.rejected` payload.
#[derive(Debug, Clone, Serialize)]
pub struct DatRejected<'a> {
    /// The file as dropped.
    pub file: &'a str,
    /// Why it was refused.
    pub reason: &'a str,
}

/// The `source.changed` payload.
#[derive(Debug, Clone, Serialize)]
pub struct SourceChanged<'a> {
    /// The source.
    pub source_id: SourceId,
    /// Its state now.
    pub state: SourceState,
    /// Its platform, when bound.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub platform_id: Option<&'a PlatformId>,
}

/// The `download.changed` payload.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct DownloadChanged {
    /// The download.
    pub download_id: DownloadId,
    /// Its state now.
    pub state: DownloadState,
    /// Its progress, 0 to 1.
    pub progress: f64,
}

/// The `import.done` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct ImportDone {
    /// The title the file belongs to.
    pub title_id: TitleId,
    /// The file placed, kept or renamed.
    pub file_id: FileId,
    /// What was done.
    pub action: ImportAction,
}

/// The `file.changed` payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FileChanged {
    /// The file.
    pub file_id: FileId,
    /// Its state now.
    pub state: FileState,
}

/// What the server announces: one variant per event name, carrying its payload.
#[derive(Debug, Clone, Serialize)]
#[serde(untagged)]
#[non_exhaustive]
pub enum Event<'a> {
    /// `status`: the `/system/status` body.
    Status(&'a Status),
    /// `job.progress`.
    JobProgress(JobProgress<'a>),
    /// `dat.loaded`.
    DatLoaded(DatLoaded<'a>),
    /// `dat.rejected`.
    DatRejected(DatRejected<'a>),
    /// `source.changed`.
    SourceChanged(SourceChanged<'a>),
    /// `download.changed`.
    DownloadChanged(DownloadChanged),
    /// `import.done`.
    ImportDone(ImportDone),
    /// `file.changed`, throttled by the publisher.
    FileChanged(FileChanged),
}

impl Event<'_> {
    /// The name this event goes out under.
    ///
    /// ```
    /// use mistarr_server::events::{DatRejected, Event, EventKind};
    /// let e = Event::DatRejected(DatRejected { file: "a.dat", reason: "no" });
    /// assert_eq!(e.kind(), EventKind::DatRejected);
    /// ```
    #[must_use]
    pub fn kind(&self) -> EventKind {
        match self {
            Self::Status(_) => EventKind::Status,
            Self::JobProgress(_) => EventKind::JobProgress,
            Self::DatLoaded(_) => EventKind::DatLoaded,
            Self::DatRejected(_) => EventKind::DatRejected,
            Self::SourceChanged(_) => EventKind::SourceChanged,
            Self::DownloadChanged(_) => EventKind::DownloadChanged,
            Self::ImportDone(_) => EventKind::ImportDone,
            Self::FileChanged(_) => EventKind::FileChanged,
        }
    }
}

/// Broadcast buffer per subscriber. Larger than the ring, so a subscriber is
/// only closed for lagging once the ring could not have replayed the gap anyway.
pub const CHANNEL_SIZE: usize = RING_SIZE * 4;

/// One published event as the bus holds it. `data` is the serialised JSON payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Message {
    /// The publishing bus's epoch.
    pub epoch: u64,
    /// Increasing sequence within the epoch, starting at 1; 0 for a transient event.
    pub seq: u64,
    /// The event name.
    pub kind: EventKind,
    /// JSON text for the `data:` line.
    pub data: String,
}

impl Message {
    /// The SSE `id:` value, `<epoch hex>-<seq>`.
    ///
    /// ```
    /// use mistarr_server::events::{EventKind, Message};
    /// let e = Message { epoch: 255, seq: 3, kind: EventKind::Status, data: "{}".into() };
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

/// The event's JSON, or `null` with a warning when it cannot be serialised.
fn encode(event: &Event<'_>) -> String {
    serde_json::to_string(event).unwrap_or_else(|e| {
        tracing::warn!(event = event.kind().as_str(), error = %e, "event payload not serialisable");
        "null".to_owned()
    })
}

/// Fan-out of server events to SSE connections.
pub struct EventBus {
    epoch: u64,
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Arc<Message>>,
}

struct Ring {
    next_seq: u64,
    events: VecDeque<Arc<Message>>,
}

/// What a new subscriber receives: events to replay, then the live stream.
pub struct Subscription {
    /// Ring events after the client's `Last-Event-ID`, oldest first.
    pub replay: Vec<Arc<Message>>,
    /// False when events the client has not seen may be missing from
    /// `replay`: a different epoch, or an id older than the ring.
    pub complete: bool,
    /// Every event published after `replay` was taken.
    pub live: broadcast::Receiver<Arc<Message>>,
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
        #[expect(
            clippy::cast_possible_truncation,
            reason = "Truncation keeps the low 64 bits, which change every nanosecond."
        )]
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
    /// use mistarr_server::events::{DatRejected, Event, EventBus};
    /// let bus = EventBus::new();
    /// assert_eq!(bus.publish(&Event::DatRejected(DatRejected { file: "a", reason: "b" })), 1);
    /// ```
    pub fn publish(&self, event: &Event<'_>) -> u64 {
        let data = encode(event);
        let mut ring = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let seq = ring.next_seq;
        ring.next_seq += 1;
        let message = Arc::new(self.message(seq, event.kind(), data));
        if ring.events.len() == RING_SIZE {
            ring.events.pop_front();
        }
        ring.events.push_back(Arc::clone(&message));
        // Sending under the lock keeps replay and live free of gaps and duplicates.
        let _ = self.tx.send(message);
        seq
    }

    /// Broadcasts an event to live subscribers only: it takes no ring slot and no
    /// sequence number, so it is sent without an id and never replayed.
    ///
    /// ```
    /// use mistarr_server::events::{DatRejected, Event, EventBus};
    /// let bus = EventBus::new();
    /// bus.publish_transient(&Event::DatRejected(DatRejected { file: "a", reason: "b" }));
    /// assert_eq!(bus.latest_seq(), 0);
    /// ```
    pub fn publish_transient(&self, event: &Event<'_>) {
        let message = self.message(0, event.kind(), encode(event));
        let _ = self.tx.send(Arc::new(message));
    }

    fn message(&self, seq: u64, kind: EventKind, data: String) -> Message {
        Message {
            epoch: self.epoch,
            seq,
            kind,
            data,
        }
    }

    /// Subscribes with the client's `Last-Event-ID`. `None` replays nothing.
    /// An id from this epoch replays the ring after it; any other id replays
    /// the whole ring and reports the replay incomplete.
    ///
    /// ```
    /// use mistarr_server::events::{DatRejected, Event, EventBus};
    /// let bus = EventBus::with_epoch(1);
    /// let e = Event::DatRejected(DatRejected { file: "a", reason: "b" });
    /// bus.publish(&e);
    /// bus.publish(&e);
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
    /// use mistarr_server::events::{DatRejected, Event, EventBus};
    /// let bus = EventBus::new();
    /// bus.publish(&Event::DatRejected(DatRejected { file: "a", reason: "b" }));
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

    fn changed(i: i64) -> Event<'static> {
        Event::FileChanged(FileChanged {
            file_id: FileId(i),
            state: FileState::Verified,
        })
    }

    fn ids(sub: &Subscription) -> Vec<u64> {
        sub.replay.iter().map(|e| e.seq).collect()
    }

    #[test]
    fn ring_keeps_the_newest_events() {
        let bus = EventBus::with_epoch(9);
        for i in 0..300 {
            bus.publish(&changed(i));
        }
        let sub = bus.subscribe(Some("9-0"));
        assert_eq!(sub.replay.len(), RING_SIZE);
        assert!(!sub.complete, "events 1..44 were evicted");
        assert_eq!(sub.replay[0].seq, 300 - RING_SIZE as u64 + 1);
        assert_eq!(
            sub.replay.last().map(|e| e.data.as_str()),
            Some(r#"{"file_id":299,"state":"verified"}"#)
        );
        let from_ring_start = format!("9-{}", 300 - RING_SIZE);
        assert!(bus.subscribe(Some(&from_ring_start)).complete);
    }

    #[test]
    fn replay_after_id_in_the_same_epoch() {
        let bus = EventBus::with_epoch(0xab);
        for i in 0..5 {
            bus.publish(&changed(i));
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
            old.publish(&changed(i));
        }
        let last_seen = format!("{:x}-2", old.epoch());
        let new = EventBus::with_epoch(2);
        for i in 0..5 {
            new.publish(&changed(i));
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
        bus.publish(&changed(1));
        let mut sub = bus.subscribe(None);
        bus.publish(&Event::ImportDone(ImportDone {
            title_id: TitleId(1),
            file_id: FileId(2),
            action: ImportAction::Placed,
        }));
        let got = sub.live.recv().await.expect("event");
        assert_eq!(got.id(), "3-2");
        assert_eq!(got.kind, EventKind::ImportDone);
        assert_eq!(got.data, r#"{"title_id":1,"file_id":2,"action":"placed"}"#);
    }

    #[tokio::test]
    async fn a_transient_event_has_no_sequence_and_no_ring_slot() {
        let bus = EventBus::with_epoch(5);
        let mut sub = bus.subscribe(None);
        bus.publish_transient(&changed(1));
        let got = sub.live.recv().await.expect("event");
        assert_eq!(got.seq, 0);
        assert_eq!(bus.latest_seq(), 0);
        assert!(bus.subscribe(Some("5-0")).replay.is_empty());
    }

    #[tokio::test]
    async fn a_subscriber_can_fall_a_full_ring_behind_without_lagging() {
        let bus = EventBus::with_epoch(4);
        let mut sub = bus.subscribe(None);
        for i in 0..i64::try_from(RING_SIZE + 10).expect("small") {
            bus.publish(&changed(i));
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
    fn payloads_serialise_as_their_own_body() {
        let platform = PlatformId("nes".into());
        let loaded = Event::DatLoaded(DatLoaded {
            dat_version_id: DatVersionId(4),
            file: "a.dat",
            platform_id: Some(&platform),
        });
        assert_eq!(loaded.kind(), EventKind::DatLoaded);
        assert_eq!(
            encode(&loaded),
            r#"{"dat_version_id":4,"file":"a.dat","platform_id":"nes"}"#
        );
        let unbound = Event::SourceChanged(SourceChanged {
            source_id: SourceId(1),
            state: SourceState::Bound,
            platform_id: None,
        });
        assert_eq!(encode(&unbound), r#"{"source_id":1,"state":"bound"}"#);
    }

    #[test]
    fn the_api_doc_lists_exactly_the_event_names() {
        let doc = include_str!("../../../docs/API.md");
        let table = doc.split("\n## Events\n").nth(1).expect("events section");
        let table = table.split("\n## ").next().unwrap_or_default();
        let mut documented: Vec<&str> = table
            .lines()
            .filter(|l| l.starts_with("| `"))
            .flat_map(|l| l.split('|').nth(1).unwrap_or_default().split('/'))
            .map(|n| n.trim().trim_matches('`'))
            .collect();
        let mut named: Vec<&str> = EventKind::ALL.iter().map(|k| k.as_str()).collect();
        documented.sort_unstable();
        named.sort_unstable();
        assert_eq!(documented, named);
    }
}
