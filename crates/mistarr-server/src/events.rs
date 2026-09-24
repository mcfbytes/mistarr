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

/// One published event. `data` is the serialised JSON payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Event {
    /// Increasing id within this process, starting at 1; sent as the SSE `id:`.
    pub id: u64,
    /// The event name.
    pub kind: EventKind,
    /// JSON text for the `data:` line.
    pub data: String,
}

/// Fan-out of server events to SSE connections.
pub struct EventBus {
    ring: Mutex<Ring>,
    tx: broadcast::Sender<Arc<Event>>,
}

struct Ring {
    next_id: u64,
    events: VecDeque<Arc<Event>>,
}

/// What a new subscriber receives: events to replay, then the live stream.
pub struct Subscription {
    /// Events after the client's `Last-Event-ID`, oldest first.
    pub replay: Vec<Arc<Event>>,
    /// Every event published after `replay` was taken.
    pub live: broadcast::Receiver<Arc<Event>>,
}

impl Default for EventBus {
    fn default() -> Self {
        Self::new()
    }
}

impl EventBus {
    /// An empty bus.
    ///
    /// ```
    /// assert_eq!(mistarr_server::events::EventBus::new().latest_id(), 0);
    /// ```
    #[must_use]
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(RING_SIZE);
        Self {
            ring: Mutex::new(Ring {
                next_id: 1,
                events: VecDeque::with_capacity(RING_SIZE),
            }),
            tx,
        }
    }

    /// Records and broadcasts an event, returning its id.
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
        let id = ring.next_id;
        ring.next_id += 1;
        let event = Arc::new(Event { id, kind, data });
        if ring.events.len() == RING_SIZE {
            ring.events.pop_front();
        }
        ring.events.push_back(Arc::clone(&event));
        // Sending under the lock keeps replay and live free of gaps and duplicates.
        let _ = self.tx.send(event);
        id
    }

    /// Subscribes, replaying ring events newer than `last_id`. An id the ring
    /// has never issued, as after a restart, replays the whole ring.
    ///
    /// ```
    /// use mistarr_server::events::{EventBus, EventKind};
    /// let bus = EventBus::new();
    /// bus.publish(EventKind::Status, &1);
    /// bus.publish(EventKind::Status, &2);
    /// assert_eq!(bus.subscribe(Some(1)).replay.len(), 1);
    /// assert!(bus.subscribe(None).replay.is_empty());
    /// ```
    pub fn subscribe(&self, last_id: Option<u64>) -> Subscription {
        let ring = self.ring.lock().unwrap_or_else(PoisonError::into_inner);
        let live = self.tx.subscribe();
        let replay = match last_id {
            None => Vec::new(),
            Some(last) if last >= ring.next_id => ring.events.iter().cloned().collect(),
            Some(last) => ring
                .events
                .iter()
                .filter(|e| e.id > last)
                .cloned()
                .collect(),
        };
        Subscription { replay, live }
    }

    /// The id of the newest event, or 0 before the first.
    ///
    /// ```
    /// use mistarr_server::events::{EventBus, EventKind};
    /// let bus = EventBus::new();
    /// bus.publish(EventKind::FileChanged, &0);
    /// assert_eq!(bus.latest_id(), 1);
    /// ```
    #[must_use]
    pub fn latest_id(&self) -> u64 {
        self.ring
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .next_id
            - 1
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ring_keeps_the_newest_events() {
        let bus = EventBus::new();
        for i in 0..300 {
            bus.publish(EventKind::FileChanged, &i);
        }
        let replay = bus.subscribe(Some(0)).replay;
        assert_eq!(replay.len(), RING_SIZE);
        assert_eq!(replay[0].id, 300 - RING_SIZE as u64 + 1);
        assert_eq!(replay.last().map(|e| e.data.as_str()), Some("299"));
    }

    #[test]
    fn replay_after_id_and_after_restart() {
        let bus = EventBus::new();
        for i in 0..5 {
            bus.publish(EventKind::Status, &i);
        }
        let ids: Vec<_> = bus.subscribe(Some(3)).replay.iter().map(|e| e.id).collect();
        assert_eq!(ids, [4, 5]);
        assert!(bus.subscribe(Some(5)).replay.is_empty());
        assert_eq!(bus.subscribe(Some(999)).replay.len(), 5);
    }

    #[tokio::test]
    async fn live_receives_after_subscribe() {
        let bus = EventBus::new();
        bus.publish(EventKind::Status, &"old");
        let mut sub = bus.subscribe(None);
        bus.publish(EventKind::ImportDone, &serde_json::json!({"title_id": 1}));
        let got = sub.live.recv().await.expect("event");
        assert_eq!(got.id, 2);
        assert_eq!(got.kind, EventKind::ImportDone);
        assert_eq!(got.data, r#"{"title_id":1}"#);
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
