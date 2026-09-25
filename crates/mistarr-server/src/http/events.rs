//! `GET /events`: the SSE stream with `Last-Event-ID` replay.

use std::collections::VecDeque;
use std::convert::Infallible;
use std::sync::Arc;
use std::time::Duration;

use axum::extract::State;
use axum::http::HeaderMap;
use axum::response::sse::{Event as SseEvent, KeepAlive, Sse};
use axum::routing::get;
use axum::Router;
use futures_util::stream::{self, Stream};
use tokio::sync::{broadcast, watch};
use tokio::time::{Interval, MissedTickBehavior};

use crate::app::AppState;
use crate::events::{Event, EventKind};

/// Interval of SSE comment lines that keep proxies from closing the stream.
const KEEP_ALIVE: Duration = Duration::from_secs(15);

/// Sent first when the replay cannot cover everything the client missed.
const RESYNC: &str = "resync";

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new().route("/events", get(events))
}

struct Conn {
    app: Arc<AppState>,
    replay: VecDeque<Arc<Event>>,
    live: broadcast::Receiver<Arc<Event>>,
    tick: Interval,
    shutdown: watch::Receiver<bool>,
    resync: bool,
    greeted: bool,
}

async fn events(
    State(app): State<Arc<AppState>>,
    headers: HeaderMap,
) -> Sse<impl Stream<Item = Result<SseEvent, Infallible>>> {
    let last = headers.get("last-event-id").and_then(|v| v.to_str().ok());
    let sub = app.events.subscribe(last);
    let period = app.options.status_interval;
    let mut tick = tokio::time::interval_at(tokio::time::Instant::now() + period, period);
    tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let conn = Conn {
        shutdown: app.shutdown_signal(),
        app,
        replay: sub.replay.into(),
        live: sub.live,
        tick,
        resync: !sub.complete,
        greeted: false,
    };
    Sse::new(stream::unfold(conn, next)).keep_alive(KeepAlive::new().interval(KEEP_ALIVE))
}

/// `resync` if the replay may have gaps, replayed events, a `status` snapshot,
/// then live events and a periodic `status`. Ends on shutdown or when the
/// subscriber lags past the broadcast buffer.
async fn next(mut conn: Conn) -> Option<(Result<SseEvent, Infallible>, Conn)> {
    if conn.resync {
        conn.resync = false;
        return Some((Ok(SseEvent::default().event(RESYNC).data("{}")), conn));
    }
    if let Some(ev) = conn.replay.pop_front() {
        return Some((Ok(to_sse(&ev)), conn));
    }
    if !conn.greeted {
        conn.greeted = true;
        let status = status_event(&conn.app).await;
        return Some((Ok(status), conn));
    }
    let live = tokio::select! {
        received = conn.live.recv() => match received {
            Ok(ev) => Some(ev),
            // The client reconnects with Last-Event-ID and replays from the ring.
            Err(broadcast::error::RecvError::Lagged(_) | broadcast::error::RecvError::Closed) => return None,
        },
        _ = conn.tick.tick() => None,
        _ = conn.shutdown.wait_for(|s| *s) => return None,
    };
    let sse = match live {
        Some(ev) => to_sse(&ev),
        None => status_event(&conn.app).await,
    };
    Some((Ok(sse), conn))
}

/// A transient event, sequence 0, goes out without an id so it never moves `Last-Event-ID`.
fn to_sse(ev: &Event) -> SseEvent {
    let sse = SseEvent::default().event(ev.kind.as_str()).data(&ev.data);
    if ev.seq == 0 {
        sse
    } else {
        sse.id(ev.id())
    }
}

/// A `status` event without an id, so it does not move the client's `Last-Event-ID`.
async fn status_event(app: &AppState) -> SseEvent {
    let status = crate::status::snapshot(app).await;
    let data = serde_json::to_string(&status).unwrap_or_else(|_| "null".to_owned());
    SseEvent::default()
        .event(EventKind::Status.as_str())
        .data(data)
}
