use std::path::Path;
use std::sync::Mutex;
use std::time::Duration;

use async_trait::async_trait;
use mistarr_clients::{
    ClientError, ClientFile, ClientInfo, ClientTorrentId, SeedPolicy, TorrentSource, TorrentStatus,
};

use super::*;
use crate::app::testutil::state_with;
use crate::app::testutil::TestDir;
use crate::app::Options;
use crate::jobs::gate::MENU;

/// A client whose global limits are recorded; `down` makes every limit call fail.
struct Mock {
    calls: Mutex<Vec<String>>,
    up: Mutex<UploadLimit>,
    down: Mutex<bool>,
}

impl Mock {
    fn new(up: UploadLimit) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            up: Mutex::new(up),
            down: Mutex::new(false),
        })
    }

    fn log(&self, call: String) -> mistarr_clients::Result<()> {
        if *self.down.lock().expect("lock") {
            return Err(ClientError::Unreachable("down".into()));
        }
        self.calls.lock().expect("lock").push(call);
        Ok(())
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }

    fn set_down(&self, down: bool) {
        *self.down.lock().expect("lock") = down;
    }

    fn up(&self) -> UploadLimit {
        *self.up.lock().expect("lock")
    }
}

fn nope<T>() -> mistarr_clients::Result<T> {
    Err(ClientError::Protocol("not scripted".into()))
}

#[async_trait]
impl DownloadClient for Mock {
    async fn probe(&self) -> mistarr_clients::Result<ClientInfo> {
        nope()
    }
    async fn add(
        &self,
        _src: TorrentSource,
        _dir: &Path,
        _wanted: &[u32],
        _seed: SeedPolicy,
    ) -> mistarr_clients::Result<ClientTorrentId> {
        nope()
    }
    async fn set_wanted(&self, _id: &ClientTorrentId, _w: &[u32]) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn set_seed_policy(
        &self,
        _id: &ClientTorrentId,
        _seed: SeedPolicy,
    ) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn start(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn stop(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn status(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<TorrentStatus> {
        nope()
    }
    async fn files(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<Vec<ClientFile>> {
        nope()
    }
    async fn remove(&self, _id: &ClientTorrentId, _data: bool) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn set_rate_limits(
        &self,
        down: Option<u32>,
        up: Option<u32>,
    ) -> mistarr_clients::Result<()> {
        let (down, up) = (down.unwrap_or(0), up.unwrap_or(0));
        self.log(format!("limits:{down}/{up}"))?;
        let mut limit = self.up.lock().expect("lock");
        limit.enabled = up > 0;
        if up > 0 {
            limit.kbps = up;
        }
        Ok(())
    }
    async fn upload_limit(&self) -> mistarr_clients::Result<UploadLimit> {
        self.log("read".into())?;
        Ok(self.up())
    }
    async fn set_upload_limit(&self, limit: UploadLimit) -> mistarr_clients::Result<()> {
        self.log(format!("restore:{}/{}", limit.enabled, limit.kbps))?;
        *self.up.lock().expect("lock") = limit;
        Ok(())
    }
    async fn pause_uploads(&self) -> mistarr_clients::Result<()> {
        self.log("pause".into())?;
        *self.up.lock().expect("lock") = UploadLimit {
            enabled: true,
            kbps: 0,
        };
        Ok(())
    }
}

const USER_LIMIT: UploadLimit = UploadLimit {
    enabled: true,
    kbps: 40,
};

fn fast(o: &mut Options) {
    o.corename_poll = Duration::from_millis(10);
}

/// App state at the menu with `mock` as its client and `follow_gate` running.
fn start(mock: &Arc<Mock>, marker: Option<Marker>) -> (TestDir, Arc<AppState>) {
    let (dir, app) = state_with(fast);
    app.set_client(Arc::clone(mock) as Arc<dyn DownloadClient>);
    if let Some(m) = marker {
        app.db
            .write_blocking(move |c| settings::set_json(c, keys::UPLOADS_PAUSED, &m))
            .expect("marker");
    }
    app.gate.set_corename(Some(MENU.into()));
    tokio::spawn(follow_gate(Arc::clone(&app)));
    (dir, app)
}

async fn wait_calls(mock: &Mock, n: usize) -> Vec<String> {
    for _ in 0..300 {
        if mock.calls().len() >= n {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    mock.calls()
}

async fn stored(app: &AppState) -> Option<Marker> {
    app.db
        .read(|c| settings::get_json::<Marker>(c, keys::UPLOADS_PAUSED))
        .await
        .expect("read")
}

#[tokio::test]
async fn uploads_pause_while_a_core_runs_and_the_limit_comes_back() {
    let mock = Mock::new(USER_LIMIT);
    let (_dir, app) = start(&mock, None);
    let mut live = app.events.subscribe(None).live;
    app.gate.set_corename(Some("SNES".into()));
    let calls = wait_calls(&mock, 3).await;
    assert_eq!(calls, ["read", "limits:512/64", "pause"]);
    assert!(app.uploads_paused());
    assert_eq!(
        stored(&app).await,
        Some(Marker {
            kind: ClientKind::Transmission,
            limit: USER_LIMIT
        })
    );
    let ev = live.recv().await.expect("status event");
    assert_eq!(ev.kind, EventKind::Status);
    assert!(ev.data.contains(r#""uploads_paused":true"#), "{}", ev.data);

    app.gate.set_corename(Some("N64".into()));
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(calls[3..], ["restore:true/40", "limits:0/0"]);
    assert!(!app.uploads_paused());
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn the_setting_off_leaves_uploads_alone() {
    let mock = Mock::new(USER_LIMIT);
    let (_dir, app) = start(&mock, None);
    app.update_config(|c| c.transfer.pause_uploads_while_playing = false);
    app.gate.set_corename(Some("SNES".into()));
    wait_calls(&mock, 1).await;
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 2).await;
    assert_eq!(calls, ["limits:512/64", "limits:0/0"]);
    assert!(!app.uploads_paused());
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn turning_the_setting_off_mid_game_restores_then_sends_the_core_limits() {
    let mock = Mock::new(USER_LIMIT);
    let (_dir, app) = start(&mock, None);
    app.gate.set_corename(Some("SNES".into()));
    wait_calls(&mock, 3).await;
    app.update_config(|c| c.transfer.pause_uploads_while_playing = false);
    app.limits_wake.notify_one();
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(calls[3..], ["restore:true/40", "limits:512/64"]);
    assert!(!app.uploads_paused());
}

#[tokio::test]
async fn a_restart_at_the_menu_restores_the_recorded_limit() {
    let mock = Mock::new(UploadLimit {
        enabled: true,
        kbps: 0,
    });
    let marker = Marker {
        kind: ClientKind::Transmission,
        limit: UploadLimit {
            enabled: false,
            kbps: 7,
        },
    };
    let (_dir, app) = start(&mock, Some(marker));
    let calls = wait_calls(&mock, 1).await;
    assert_eq!(calls, ["restore:false/7"]);
    assert_eq!(mock.up(), marker.limit);
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn a_restart_during_a_game_keeps_the_first_recorded_limit() {
    let mock = Mock::new(UploadLimit {
        enabled: true,
        kbps: 0,
    });
    let marker = Marker {
        kind: ClientKind::Transmission,
        limit: USER_LIMIT,
    };
    let (_dir, app) = state_with(fast);
    app.set_client(Arc::clone(&mock) as Arc<dyn DownloadClient>);
    app.db
        .write_blocking(move |c| settings::set_json(c, keys::UPLOADS_PAUSED, &marker))
        .expect("marker");
    app.gate.set_corename(Some("SNES".into()));
    tokio::spawn(follow_gate(Arc::clone(&app)));
    let calls = wait_calls(&mock, 2).await;
    assert_eq!(calls, ["limits:512/64", "pause"]);
    assert_eq!(stored(&app).await, Some(marker));
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 4).await;
    assert_eq!(calls[2..], ["restore:true/40", "limits:0/0"]);
}

#[tokio::test]
async fn an_unreachable_client_is_retried_until_it_answers() {
    let mock = Mock::new(USER_LIMIT);
    let (_dir, app) = start(&mock, None);
    mock.set_down(true);
    app.gate.set_corename(Some("SNES".into()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(mock.calls().is_empty());
    assert!(!app.uploads_paused());
    mock.set_down(false);
    let calls = wait_calls(&mock, 3).await;
    assert_eq!(calls, ["read", "limits:512/64", "pause"]);
    mock.set_down(true);
    app.gate.set_corename(Some(MENU.into()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(app.uploads_paused());
    assert!(stored(&app).await.is_some());
    mock.set_down(false);
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(calls[3..], ["restore:true/40", "limits:0/0"]);
    assert!(!app.uploads_paused());
}

#[test]
fn limits_follow_the_config() {
    let l = LimitsConfig {
        down_kbps_menu: 9,
        ..LimitsConfig::default()
    };
    assert_eq!(limits_for(&l, false), (9, 0));
    assert_eq!(limits_for(&l, true), (512, 64));
}
