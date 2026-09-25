use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;

use async_trait::async_trait;
use mistarr_clients::{
    ClientError, ClientFile, ClientInfo, SeedPolicy, TorrentSource, TorrentStatus,
};

use super::*;
use crate::app::testutil::{state_with, TestDir};
use crate::app::Options;
use crate::db::deferred::{self, Op};
use crate::freeze::fake::FakeClient;
use crate::freeze::Signal;
use crate::jobs::gate::MENU;
use crate::jobs::transfer::Deselect;
use crate::jobs::Scheduler;
use serde_json::json;

/// A client whose global limits live in memory and whose calls are recorded;
/// `down` makes every call fail.
struct Mock {
    calls: Mutex<Vec<String>>,
    down_limit: Mutex<RateLimit>,
    up_limit: Mutex<RateLimit>,
    unreachable: Mutex<bool>,
    refuse_seed: Mutex<bool>,
    pid: Mutex<Option<u32>>,
    alt_up: Mutex<Option<RateLimit>>,
}

impl Mock {
    fn new(up: RateLimit) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            down_limit: Mutex::new(RateLimit::default()),
            up_limit: Mutex::new(up),
            unreachable: Mutex::new(false),
            refuse_seed: Mutex::new(false),
            pid: Mutex::new(None),
            alt_up: Mutex::new(None),
        })
    }

    fn slot(&self, dir: Direction) -> &Mutex<RateLimit> {
        match dir {
            Direction::Down => &self.down_limit,
            Direction::Up => &self.up_limit,
        }
    }

    fn log(&self, call: String) -> mistarr_clients::Result<()> {
        if *self.unreachable.lock().expect("lock") {
            return Err(ClientError::Unreachable("down".into()));
        }
        self.calls.lock().expect("lock").push(call);
        Ok(())
    }

    fn calls(&self) -> Vec<String> {
        self.calls.lock().expect("lock").clone()
    }

    fn set_unreachable(&self, down: bool) {
        *self.unreachable.lock().expect("lock") = down;
    }

    fn up(&self) -> RateLimit {
        *self.up_limit.lock().expect("lock")
    }

    fn set_up(&self, limit: RateLimit) {
        *self.up_limit.lock().expect("lock") = limit;
    }

    fn set_alt(&self, alt: RateLimit) {
        *self.alt_up.lock().expect("lock") = Some(alt);
    }

    fn alt(&self) -> Option<RateLimit> {
        *self.alt_up.lock().expect("lock")
    }

    fn set_pid(&self, pid: u32) {
        *self.pid.lock().expect("lock") = Some(pid);
    }
}

fn name(dir: Direction) -> &'static str {
    match dir {
        Direction::Down => "down",
        Direction::Up => "up",
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
    async fn set_wanted(&self, id: &ClientTorrentId, w: &[u32]) -> mistarr_clients::Result<()> {
        self.log(format!("wanted:{id}:{w:?}"))
    }
    async fn set_seed_policy(
        &self,
        id: &ClientTorrentId,
        _seed: SeedPolicy,
    ) -> mistarr_clients::Result<()> {
        self.log(format!("seed:{id}"))?;
        if *self.refuse_seed.lock().expect("lock") {
            return Err(ClientError::Protocol("refused".into()));
        }
        Ok(())
    }
    async fn start(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn stop(&self, id: &ClientTorrentId) -> mistarr_clients::Result<()> {
        self.log(format!("stop:{id}"))
    }
    async fn status(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<TorrentStatus> {
        nope()
    }
    async fn files(&self, _id: &ClientTorrentId) -> mistarr_clients::Result<Vec<ClientFile>> {
        nope()
    }
    async fn remove(&self, id: &ClientTorrentId, _data: bool) -> mistarr_clients::Result<()> {
        self.log(format!("remove:{id}"))
    }
    async fn rate_limit(&self, dir: Direction) -> mistarr_clients::Result<RateLimit> {
        self.log(format!("read:{}", name(dir)))?;
        Ok(*self.slot(dir).lock().expect("lock"))
    }
    async fn set_rate_limit(&self, dir: Direction, l: RateLimit) -> mistarr_clients::Result<()> {
        self.log(format!("set:{}:{}/{}", name(dir), l.enabled, l.kbps))?;
        *self.slot(dir).lock().expect("lock") = l;
        Ok(())
    }
    async fn process_id(&self) -> mistarr_clients::Result<Option<u32>> {
        self.log("pid".into())?;
        Ok(*self.pid.lock().expect("lock"))
    }
    async fn alt_up_limit(&self) -> mistarr_clients::Result<Option<RateLimit>> {
        let alt = self.alt();
        if alt.is_some() {
            self.log("read:alt".into())?;
        }
        Ok(alt)
    }
    async fn set_alt_up_rate(&self, kbps: u32) -> mistarr_clients::Result<()> {
        self.log(format!("set:alt:{kbps}"))?;
        if let Some(alt) = self.alt_up.lock().expect("lock").as_mut() {
            alt.kbps = kbps;
        }
        Ok(())
    }
}

const OWN: RateLimit = RateLimit {
    enabled: true,
    kbps: 40,
};

fn remote(url: &str) -> ClientEndpoint {
    ClientEndpoint {
        kind: ClientKind::Transmission,
        url: url.into(),
    }
}

/// An rtorrent socket that refuses at once, so restoring through it fails fast.
fn gone_socket() -> ClientEndpoint {
    ClientEndpoint {
        kind: ClientKind::Rtorrent,
        url: "/nonexistent/mistarr-test.sock".into(),
    }
}

fn nas() -> ClientEndpoint {
    remote("http://192.0.2.5:9091/transmission/rpc")
}

fn fast(o: &mut Options) {
    o.corename_poll = Duration::from_millis(10);
    o.hold_recheck = Duration::from_secs(3600);
}

/// App state with `mock` as the client at `at`, CORENAME `core`, and `follow_gate` running.
fn start_at(
    mock: &Arc<Mock>,
    at: ClientEndpoint,
    core: &str,
    tune: impl FnOnce(&mut Options),
    saved: Option<&str>,
) -> (TestDir, Arc<AppState>) {
    let (dir, app) = state_with(|o| {
        fast(o);
        tune(o);
    });
    app.set_client_at(at, Arc::clone(mock) as Arc<dyn DownloadClient>);
    if let Some(text) = saved {
        let text = text.to_owned();
        app.db
            .write_blocking(move |c| settings::set(c, keys::CLIENT_SAVED_LIMITS, &text))
            .expect("saved");
    }
    app.gate.set_corename(Some(core.into()));
    tokio::spawn(follow_gate(Arc::clone(&app)));
    (dir, app)
}

fn start(mock: &Arc<Mock>) -> (TestDir, Arc<AppState>) {
    start_at(mock, nas(), MENU, |_| {}, None)
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

async fn wait_for(what: &str, mut f: impl FnMut() -> bool) {
    for _ in 0..300 {
        if f() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for {what}");
}

async fn stored(app: &AppState) -> Option<SavedLimits> {
    app.db
        .read(|c| settings::get_json::<SavedLimits>(c, keys::CLIENT_SAVED_LIMITS))
        .await
        .expect("read")
}

fn saved_json(client: &ClientEndpoint, down: Option<RateLimit>, up: Option<RateLimit>) -> String {
    let s = SavedLimits {
        client: client.clone(),
        down,
        up,
        alt_up: None,
    };
    serde_json::to_string(&s).expect("json")
}

#[tokio::test]
async fn uploads_are_held_while_a_core_runs_and_the_own_limits_come_back() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start(&mock);
    let mut live = app.events.subscribe(None).live;
    app.gate.set_corename(Some("SNES".into()));
    let calls = wait_calls(&mock, 4).await;
    assert_eq!(
        calls,
        ["read:down", "read:up", "set:down:true/512", "set:up:true/0"]
    );
    assert_eq!(app.client_hold(), Some(ClientHold::Uploads));
    let saved = stored(&app).await.expect("saved");
    assert_eq!(
        (saved.down, saved.up),
        (Some(RateLimit::default()), Some(OWN))
    );
    let ev = live.recv().await.expect("status event");
    assert_eq!(ev.kind, EventKind::Status);
    assert!(
        ev.data.contains(r#""client_hold":"uploads""#),
        "{}",
        ev.data
    );

    app.gate.set_corename(Some("N64".into()));
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 6).await;
    assert_eq!(calls[4..], ["set:down:false/0", "set:up:true/40"]);
    assert_eq!(mock.up(), OWN, "a zero menu limit leaves the client's own");
    assert_eq!(app.client_hold(), None);
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn a_nonzero_menu_limit_replaces_the_own_one_until_it_is_zero() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start_at(&mock, nas(), MENU, |_| {}, None);
    app.update_config(|c| c.limits.up_kbps_menu = 10);
    app.limits_wake.notify_one();
    assert_eq!(wait_calls(&mock, 2).await, ["read:up", "set:up:true/10"]);
    app.gate.set_corename(Some("SNES".into()));
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(
        calls[2..],
        ["read:down", "set:down:true/512", "set:up:true/0"]
    );
    assert_eq!(stored(&app).await.and_then(|s| s.up), Some(OWN));
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 7).await;
    assert_eq!(calls[5..], ["set:down:false/0", "set:up:true/10"]);
    app.update_config(|c| c.limits.up_kbps_menu = 0);
    app.limits_wake.notify_one();
    let calls = wait_calls(&mock, 8).await;
    assert_eq!(calls[7..], ["set:up:true/40"]);
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn the_setting_off_leaves_uploads_to_the_core_limit() {
    let mock = Mock::new(RateLimit::default());
    let (_dir, app) = start(&mock);
    app.update_config(|c| c.transfer.pause_client_while_playing = false);
    app.gate.set_corename(Some("SNES".into()));
    let calls = wait_calls(&mock, 4).await;
    assert_eq!(
        calls,
        [
            "read:down",
            "read:up",
            "set:down:true/512",
            "set:up:true/64"
        ]
    );
    assert_eq!(app.client_hold(), None);
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 6).await;
    assert_eq!(calls[4..], ["set:down:false/0", "set:up:false/0"]);
}

#[tokio::test]
async fn with_no_core_limits_and_the_setting_off_the_client_is_left_alone() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start(&mock);
    app.update_config(|c| {
        c.transfer.pause_client_while_playing = false;
        c.limits.down_kbps_core = 0;
        c.limits.up_kbps_core = 0;
    });
    app.gate.set_corename(Some("SNES".into()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    app.gate.set_corename(Some(MENU.into()));
    tokio::time::sleep(Duration::from_millis(100)).await;
    assert!(mock.calls().is_empty(), "{:?}", mock.calls());
}

#[tokio::test]
async fn turning_the_setting_off_mid_game_sets_the_core_upload_limit() {
    let mock = Mock::new(RateLimit::default());
    let (_dir, app) = start(&mock);
    app.gate.set_corename(Some("SNES".into()));
    wait_calls(&mock, 4).await;
    app.update_config(|c| c.transfer.pause_client_while_playing = false);
    app.limits_wake.notify_one();
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(calls[4..], ["set:up:true/64"]);
    assert_eq!(app.client_hold(), None);
    assert_eq!(
        stored(&app).await.and_then(|s| s.up),
        Some(RateLimit::default())
    );
}

#[tokio::test]
async fn a_restart_at_the_menu_restores_the_saved_limits() {
    let mock = Mock::new(RateLimit::HELD);
    let own = RateLimit {
        enabled: false,
        kbps: 7,
    };
    let saved = saved_json(&nas(), None, Some(own));
    let (_dir, app) = start_at(&mock, nas(), MENU, |_| {}, Some(&saved));
    assert_eq!(wait_calls(&mock, 1).await, ["set:up:false/7"]);
    assert_eq!(mock.up(), own);
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn a_restart_during_a_game_keeps_the_saved_limit_and_holds_again() {
    let mock = Mock::new(RateLimit::HELD);
    let saved = saved_json(&nas(), None, Some(OWN));
    let (_dir, app) = start_at(&mock, nas(), "SNES", |_| {}, Some(&saved));
    let calls = wait_calls(&mock, 3).await;
    assert_eq!(calls, ["read:down", "set:down:true/512", "set:up:true/0"]);
    assert_eq!(stored(&app).await.and_then(|s| s.up), Some(OWN));
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(calls[3..], ["set:down:false/0", "set:up:true/40"]);
}

#[tokio::test]
async fn an_unreadable_saved_record_is_retried_and_never_overwritten() {
    let mock = Mock::new(RateLimit::HELD);
    let (_dir, app) = start_at(&mock, nas(), "SNES", |_| {}, Some("not json"));
    tokio::time::sleep(Duration::from_millis(150)).await;
    assert!(mock.calls().is_empty(), "{:?}", mock.calls());
    let raw = app
        .db
        .read(|c| settings::get(c, keys::CLIENT_SAVED_LIMITS))
        .await
        .expect("read");
    assert_eq!(raw.as_deref(), Some("not json"));
    let fixed = saved_json(&nas(), None, Some(OWN));
    app.db
        .write(move |c| settings::set(c, keys::CLIENT_SAVED_LIMITS, &fixed))
        .await
        .expect("fix");
    let calls = wait_calls(&mock, 3).await;
    assert_eq!(calls, ["read:down", "set:down:true/512", "set:up:true/0"]);
    assert_eq!(stored(&app).await.and_then(|s| s.up), Some(OWN));
}

#[tokio::test]
async fn an_unreachable_client_is_retried_until_it_answers() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start(&mock);
    mock.set_unreachable(true);
    app.gate.set_corename(Some("SNES".into()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(mock.calls().is_empty());
    assert_eq!(app.client_hold(), None);
    mock.set_unreachable(false);
    let calls = wait_calls(&mock, 4).await;
    assert_eq!(calls[2..], ["set:down:true/512", "set:up:true/0"]);
    mock.set_unreachable(true);
    app.gate.set_corename(Some(MENU.into()));
    tokio::time::sleep(Duration::from_millis(60)).await;
    assert!(stored(&app).await.is_some());
    mock.set_unreachable(false);
    let calls = wait_calls(&mock, 6).await;
    assert_eq!(calls[4..], ["set:down:false/0", "set:up:true/40"]);
    assert_eq!(app.client_hold(), None);
}

#[test]
fn the_retry_wait_doubles_up_to_a_minute() {
    let base = Duration::from_secs(2);
    assert_eq!(backoff(base, 1), base);
    assert_eq!(backoff(base, 3), Duration::from_secs(8));
    assert_eq!(backoff(base, 40), RETRY_MAX);
}

#[tokio::test]
async fn a_new_client_while_held_gets_its_own_limit_saved_and_held() {
    let first = Mock::new(OWN);
    let (_dir, app) = start_at(&first, gone_socket(), MENU, |_| {}, None);
    app.update_config(|c| c.limits.down_kbps_core = 0);
    app.gate.set_corename(Some("SNES".into()));
    assert_eq!(
        wait_calls(&first, 3).await,
        ["pid", "read:up", "set:up:true/0"]
    );
    let second = Mock::new(RateLimit::kbps(9));
    let other = remote("http://192.0.2.6:9091/transmission/rpc");
    app.set_client_at(
        other.clone(),
        Arc::clone(&second) as Arc<dyn DownloadClient>,
    );
    assert_eq!(wait_calls(&second, 2).await, ["read:up", "set:up:true/0"]);
    let saved = stored(&app).await.expect("saved");
    assert_eq!((saved.client, saved.up), (other, Some(RateLimit::kbps(9))));
    assert_eq!(app.client_hold(), Some(ClientHold::Uploads));
    assert_eq!(first.calls().len(), 3, "the old handle is not used again");
}

#[tokio::test]
async fn uploads_that_leave_their_hold_are_held_again() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start_at(
        &mock,
        nas(),
        MENU,
        |o| o.hold_recheck = Duration::from_millis(30),
        None,
    );
    app.update_config(|c| c.limits.down_kbps_core = 0);
    app.gate.set_corename(Some("SNES".into()));
    wait_calls(&mock, 2).await;
    mock.set_up(RateLimit::default());
    wait_for("the hold again", || {
        mock.calls()
            .iter()
            .filter(|c| *c == "set:up:true/0")
            .count()
            == 2
    })
    .await;
    assert_eq!(mock.up(), RateLimit::HELD);
    assert_eq!(stored(&app).await.and_then(|s| s.up), Some(OWN));
}

#[test]
fn the_client_entry_names_the_client_and_ignores_a_freeze() {
    let (_dir, app) = state_with(|_| {});
    assert!(app.client_entry().is_none());
    app.set_client_at(nas(), Mock::new(OWN) as Arc<dyn DownloadClient>);
    assert_eq!(app.client_entry().map(|(e, _)| e), Some(nas()));
    assert!(app.client().is_some());
    assert!(app.set_client_hold(Some(ClientHold::Frozen)));
    assert!(!app.set_client_hold(Some(ClientHold::Frozen)));
    assert!(app.client_frozen());
    assert!(app.client().is_none(), "nothing talks to a frozen client");
    assert!(app.client_entry().is_some());
    assert!(app.set_client_hold(Some(ClientHold::Uploads)));
    assert_eq!(app.client_hold(), Some(ClientHold::Uploads));
    assert!(app.client().is_some());
}

fn on_board(o: &mut Options) {
    o.proc_dir = PathBuf::from("/proc");
}

fn local_rtorrent() -> ClientEndpoint {
    ClientEndpoint {
        kind: ClientKind::Rtorrent,
        url: "127.0.0.1:5000".into(),
    }
}

#[tokio::test]
async fn a_client_on_the_board_is_frozen_while_a_core_runs() {
    let proc = FakeClient::rtorrent();
    let mock = Mock::new(OWN);
    mock.set_pid(proc.pid());
    let (_dir, app) = start_at(&mock, local_rtorrent(), MENU, on_board, None);
    let mut live = app.events.subscribe(None).live;
    app.gate.set_corename(Some("SNES".into()));
    wait_for("the freeze", || proc.state() == 'T').await;
    assert_eq!(app.client_hold(), Some(ClientHold::Frozen));
    assert!(app.client().is_none());
    let recorded = freeze::read_file(&app.options.frozen_file, freeze::euid()).expect("read");
    assert_eq!(recorded.map(|f| f.pid), Some(proc.pid()));
    assert_eq!(
        mock.calls(),
        ["pid"],
        "no limits are sent to a frozen client"
    );
    let ev = live.recv().await.expect("status");
    assert!(ev.data.contains(r#""client_hold":"frozen""#), "{}", ev.data);

    app.gate.set_corename(Some(MENU.into()));
    wait_for("the resume", || proc.state() != 'T').await;
    wait_for("the hold cleared", || app.client_hold().is_none()).await;
    assert!(app.client().is_some());
    assert_eq!(
        freeze::read_file(&app.options.frozen_file, freeze::euid()).expect("read"),
        None
    );
}

#[tokio::test]
async fn a_client_resumed_elsewhere_is_frozen_again() {
    let proc = FakeClient::rtorrent();
    let mock = Mock::new(OWN);
    mock.set_pid(proc.pid());
    let (_dir, app) = start_at(
        &mock,
        local_rtorrent(),
        MENU,
        |o| {
            on_board(o);
            o.hold_recheck = Duration::from_millis(30);
        },
        None,
    );
    app.gate.set_corename(Some("SNES".into()));
    wait_for("the freeze", || proc.state() == 'T').await;
    Kill::new(Path::new("kill"))
        .send(proc.pid(), Signal::Cont)
        .expect("cont");
    wait_for("the freeze again", || proc.state() == 'T').await;
    assert_eq!(app.client_hold(), Some(ClientHold::Frozen));
}

#[tokio::test]
async fn a_process_that_is_not_the_client_is_never_frozen() {
    let mut other = Command::new("sleep").arg("60").spawn().expect("sleep");
    let mock = Mock::new(OWN);
    mock.set_pid(other.id());
    let (_dir, app) = start_at(&mock, local_rtorrent(), MENU, on_board, None);
    app.update_config(|c| c.limits.down_kbps_core = 0);
    app.gate.set_corename(Some("SNES".into()));
    let calls = wait_calls(&mock, 3).await;
    assert_eq!(calls, ["pid", "read:up", "set:up:true/0"]);
    assert_eq!(app.client_hold(), Some(ClientHold::Uploads));
    assert_ne!(
        freeze::stat(Path::new("/proc"), other.id())
            .expect("stat")
            .0,
        'T'
    );
    let _ = other.kill();
    let _ = other.wait();
}

#[tokio::test]
async fn a_frozen_client_is_resumed_at_startup_unless_a_core_still_runs() {
    let proc = FakeClient::rtorrent();
    let (dir, app) = state_with(on_board);
    let kill = Kill::new(Path::new("kill"));
    freeze::freeze(
        Path::new("/proc"),
        &kill,
        &app.options.frozen_file,
        proc.pid(),
    )
    .expect("freeze");
    wait_for("the freeze", || proc.state() == 'T').await;

    app.gate.set_corename(Some("SNES".into()));
    recover_frozen(&app).await;
    assert_eq!(proc.state(), 'T', "a running core keeps it frozen");
    assert!(app.client_frozen());

    app.gate.set_corename(Some(MENU.into()));
    recover_frozen(&app).await;
    wait_for("the resume", || proc.state() != 'T').await;
    assert!(!app.client_frozen());
    assert!(!app.options.frozen_file.exists());
    drop(dir);
}

#[tokio::test]
async fn shutdown_resumes_a_frozen_client() {
    let proc = FakeClient::rtorrent();
    let (_dir, app) = state_with(on_board);
    let kill = Kill::new(Path::new("kill"));
    freeze::freeze(
        Path::new("/proc"),
        &kill,
        &app.options.frozen_file,
        proc.pid(),
    )
    .expect("freeze");
    app.set_client_hold(Some(ClientHold::Frozen));
    thaw_for_shutdown(&app).await;
    wait_for("the resume", || proc.state() != 'T').await;
    assert!(!app.options.frozen_file.exists());
    assert!(!app.client_frozen());
}

#[test]
fn targets_follow_the_config() {
    let l = LimitsConfig {
        down_kbps_menu: 9,
        ..LimitsConfig::default()
    };
    assert_eq!(
        target(&l, false, false),
        Target {
            down: Some(RateLimit::kbps(9)),
            up: None
        }
    );
    assert_eq!(
        target(&l, true, false),
        Target {
            down: Some(RateLimit::kbps(512)),
            up: Some(RateLimit::kbps(64))
        }
    );
}

/// A source whose torrent `t` is in the client, under seed policy `none`.
fn source_in_client(app: &AppState) -> sources::SourceId {
    app.db
        .write_blocking(|c| {
            let id = sources::insert(
                c,
                &sources::NewSource {
                    infohash: &"0b".repeat(20),
                    display_name: "Synthetic Set",
                    origin_file: "set.torrent",
                    state: sources::SourceState::Bound,
                    reason: None,
                    added_at: 0,
                },
            )?;
            sources::set_client_id(c, id, Some("t"))?;
            Ok(id)
        })
        .expect("source")
}

async fn waiting(app: &AppState) -> Vec<Op> {
    let kept = app.db.read(deferred::get).await.expect("deferred");
    kept.into_iter().map(|e| e.op).collect()
}

/// App state with the scheduler running and `mock` as a frozen client.
fn frozen_client(mock: &Arc<Mock>) -> (TestDir, Arc<AppState>) {
    let (dir, app) = state_with(fast);
    Scheduler::start(&app);
    app.set_client_at(nas(), Arc::clone(mock) as Arc<dyn DownloadClient>);
    app.set_client_hold(Some(ClientHold::Frozen));
    (dir, app)
}

#[tokio::test]
async fn a_cancel_during_a_game_waits_for_the_client_and_runs_at_the_resume() {
    let mock = Mock::new(OWN);
    let (_dir, app) = frozen_client(&mock);
    let source = source_in_client(&app);
    Scheduler::run_inline(&app, Arc::new(Deselect { source_id: source }))
        .await
        .expect("deselect");
    assert!(mock.calls().is_empty(), "nothing reaches a frozen client");
    assert_eq!(waiting(&app).await, [Op::Deselect(source)]);

    app.set_client_hold(None);
    replay_deferred(&app).await.expect("replay");
    assert_eq!(wait_calls(&mock, 2).await, ["stop:t", "wanted:t:[]"]);
    assert!(waiting(&app).await.is_empty());
}

#[tokio::test]
async fn a_finished_torrent_is_released_at_the_resume() {
    let mock = Mock::new(OWN);
    let (_dir, app) = frozen_client(&mock);
    let source = source_in_client(&app);
    app.db
        .write_blocking(move |c| {
            let rom = sources::fixtures::seed_rom(c, "nes", "Example Quest (USA).nes", 16, &[])?;
            crate::db::downloads_import::insert_fixture(c, rom, source, 0, "done", None)?;
            Ok(())
        })
        .expect("placed");
    crate::jobs::import::release_source(&app, source).await;
    assert!(mock.calls().is_empty());
    assert_eq!(waiting(&app).await, [Op::Release(source)]);

    app.set_client_hold(None);
    replay_deferred(&app).await.expect("replay");
    assert_eq!(mock.calls(), ["remove:t"]);
    let row = app
        .db
        .read(move |c| sources::get(c, source))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(row.client_id, None);
    assert!(waiting(&app).await.is_empty());
}

#[tokio::test]
async fn seed_policies_changed_during_a_game_apply_at_the_resume() {
    let mock = Mock::new(OWN);
    let (_dir, app) = frozen_client(&mock);
    source_in_client(&app);
    defer(&app, Op::Seed).await;
    assert_eq!(waiting(&app).await, [Op::Seed]);
    assert!(mock.calls().is_empty());

    app.set_client_hold(None);
    replay_deferred(&app).await.expect("replay");
    assert_eq!(mock.calls(), ["seed:t"]);
    assert!(waiting(&app).await.is_empty());
}

#[tokio::test]
async fn detection_during_a_game_is_kept_for_the_resume() {
    let mock = Mock::new(OWN);
    let (_dir, app) = frozen_client(&mock);
    crate::jobs::detect_client::detect_and_store(&app, false)
        .await
        .expect("detect");
    assert_eq!(waiting(&app).await, [Op::Detect]);
    assert!(mock.calls().is_empty());
}

#[tokio::test]
async fn work_kept_before_a_restart_runs_when_the_gate_starts() {
    let mock = Mock::new(OWN);
    let (dir, app) = state_with(fast);
    Scheduler::start(&app);
    let source = source_in_client(&app);
    app.db
        .write_blocking(move |c| deferred::add(c, Op::Deselect(source), 0))
        .expect("kept");
    let _gate = start_at_state(&mock, &app);
    assert_eq!(wait_calls(&mock, 2).await, ["stop:t", "wanted:t:[]"]);
    wait_for_async(&app).await;
    drop(dir);
}

fn start_at_state(mock: &Arc<Mock>, app: &Arc<AppState>) -> tokio::task::JoinHandle<()> {
    app.set_client_at(nas(), Arc::clone(mock) as Arc<dyn DownloadClient>);
    app.gate.set_corename(Some(MENU.into()));
    tokio::spawn(follow_gate(Arc::clone(app)))
}

async fn wait_for_async(app: &AppState) {
    for _ in 0..300 {
        if waiting(app).await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the kept work stayed");
}

#[tokio::test]
async fn a_core_limit_never_raises_the_client_above_its_own() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start(&mock);
    app.update_config(|c| {
        c.transfer.pause_client_while_playing = false;
        c.limits.down_kbps_core = 0;
        c.limits.up_kbps_core = 100;
    });
    app.gate.set_corename(Some("SNES".into()));
    assert_eq!(wait_calls(&mock, 2).await, ["read:up", "set:up:true/40"]);
    app.update_config(|c| c.limits.up_kbps_core = 8);
    app.limits_wake.notify_one();
    let calls = wait_calls(&mock, 3).await;
    assert_eq!(calls[2..], ["set:up:true/8"]);
}

#[tokio::test]
async fn held_uploads_also_hold_the_alternate_rate_and_put_it_back() {
    let mock = Mock::new(OWN);
    mock.set_alt(RateLimit {
        enabled: true,
        kbps: 25,
    });
    let (_dir, app) = start(&mock);
    app.update_config(|c| c.limits.down_kbps_core = 0);
    app.gate.set_corename(Some("SNES".into()));
    let calls = wait_calls(&mock, 4).await;
    assert_eq!(calls, ["read:up", "read:alt", "set:up:true/0", "set:alt:0"]);
    assert_eq!(stored(&app).await.and_then(|s| s.alt_up), Some(25));
    app.gate.set_corename(Some(MENU.into()));
    let calls = wait_calls(&mock, 6).await;
    assert_eq!(calls[4..], ["set:up:true/40", "set:alt:25"]);
    assert_eq!(mock.alt().map(|a| a.kbps), Some(25));
    assert_eq!(stored(&app).await, None);
}

#[tokio::test]
async fn limits_of_a_client_no_longer_in_use_are_set_aside_and_tried_again() {
    let saved = SavedLimits {
        client: remote("http://192.0.2.7:9/transmission/rpc"),
        down: None,
        up: Some(OWN),
        alt_up: Some(5),
    };
    let old = Mock::new(RateLimit::HELD);
    restore_into(old.as_ref(), &saved).await.expect("restore");
    assert_eq!(old.calls(), ["set:up:true/40", "set:alt:5"]);

    let mock = Mock::new(RateLimit::kbps(9));
    let text = serde_json::to_string(&SavedLimits {
        client: gone_socket(),
        ..saved
    })
    .expect("json");
    let (_dir, app) = start_at(&mock, nas(), "SNES", |_| {}, Some(&text));
    app.update_config(|c| c.limits.down_kbps_core = 0);
    app.limits_wake.notify_one();
    wait_for("the new client held", || {
        mock.calls().contains(&"set:up:true/0".to_owned())
    })
    .await;
    let now = stored(&app).await.expect("saved");
    assert_eq!((now.client, now.up), (nas(), Some(RateLimit::kbps(9))));
    let before = crate::unix_now();
    let mut previous = Vec::new();
    for _ in 0..300 {
        previous = previous_list(&app).await;
        if previous.first().is_some_and(|p| p.tries == 1) {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(previous.len(), 1, "the old client's limits are kept");
    assert_eq!(previous[0].saved.client, gone_socket());
    assert_eq!(previous[0].tries, 1);
    assert!(previous[0].next_at >= before + 59, "{previous:?}");
}

async fn previous_list(app: &AppState) -> Vec<PreviousLimits> {
    app.db
        .read(|c| settings::get_json::<Vec<PreviousLimits>>(c, keys::CLIENT_PREVIOUS_LIMITS))
        .await
        .expect("read")
        .unwrap_or_default()
}

fn set_previous(app: &AppState, list: &[PreviousLimits]) {
    let text = serde_json::to_string(list).expect("json");
    app.db
        .write_blocking(move |c| settings::set(c, keys::CLIENT_PREVIOUS_LIMITS, &text))
        .expect("previous");
}

#[tokio::test]
async fn limits_set_aside_a_day_ago_are_dropped_after_a_failed_try() {
    let now = crate::unix_now();
    let day = i64::try_from(PREVIOUS_KEEP.as_secs()).expect("secs");
    let mock = Mock::new(OWN);
    let (_dir, app) = state_with(fast);
    app.set_client_at(nas(), Arc::clone(&mock) as Arc<dyn DownloadClient>);
    let stale = PreviousLimits {
        saved: SavedLimits {
            client: gone_socket(),
            down: None,
            up: Some(OWN),
            alt_up: None,
        },
        since: now - day - 1,
        tries: 20,
        next_at: now,
    };
    set_previous(&app, &[stale]);
    tokio::spawn(follow_gate(Arc::clone(&app)));
    for _ in 0..300 {
        if previous_list(&app).await.is_empty() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("the stale limits stayed");
}

#[tokio::test]
async fn with_no_client_the_saved_limits_are_set_aside_and_taken_back() {
    let (_dir, app) = state_with(fast);
    let text = saved_json(&gone_socket(), None, Some(OWN));
    app.db
        .write_blocking(move |c| settings::set(c, keys::CLIENT_SAVED_LIMITS, &text))
        .expect("saved");
    tokio::spawn(follow_gate(Arc::clone(&app)));
    for _ in 0..300 {
        if !previous_list(&app).await.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert_eq!(previous_list(&app).await[0].saved.up, Some(OWN));
    assert_eq!(stored(&app).await, None);

    let mock = Mock::new(RateLimit::HELD);
    app.set_client_at(gone_socket(), Arc::clone(&mock) as Arc<dyn DownloadClient>);
    app.gate.set_corename(Some("SNES".into()));
    wait_for("the hold", || {
        mock.calls().contains(&"set:up:true/0".to_owned())
    })
    .await;
    assert_eq!(
        mock.calls()[0],
        "set:up:true/40",
        "the limit set aside is put back, never read while it is held"
    );
    assert_eq!(stored(&app).await.and_then(|s| s.up), Some(OWN));
    assert!(previous_list(&app).await.is_empty());
}

#[tokio::test]
async fn no_stop_is_sent_once_shutdown_began() {
    let proc = FakeClient::rtorrent();
    let mock = Mock::new(OWN);
    mock.set_pid(proc.pid());
    let (_dir, app) = state_with(on_board);
    app.set_client_at(
        local_rtorrent(),
        Arc::clone(&mock) as Arc<dyn DownloadClient>,
    );
    app.begin_shutdown();
    let done = freeze_client(&app, &local_rtorrent(), mock.as_ref()).await;
    assert!(done.is_err(), "a stop during shutdown is refused");
    thaw_for_shutdown(&app).await;
    assert_ne!(proc.state(), 'T');
    assert!(!app.options.frozen_file.exists());
    assert!(!app.client_frozen());
}

#[tokio::test]
async fn a_planted_record_is_ignored_and_its_process_left_alone() {
    let other = FakeClient::spawn("helper");
    let (_dir, app) = state_with(on_board);
    let (_, starttime) = freeze::stat(Path::new("/proc"), other.pid()).expect("stat");
    let record = Frozen {
        pid: other.pid(),
        starttime,
    };
    freeze::write_file(&app.options.frozen_file, record).expect("record");
    Kill::new(Path::new("kill"))
        .send(other.pid(), Signal::Stop)
        .expect("stop");
    assert_eq!(other.wait_state(|s| s == 'T'), 'T');
    recover_frozen(&app).await;
    assert_eq!(other.state(), 'T', "only a client is ever resumed");
    assert!(!app.options.frozen_file.exists());
    Kill::new(Path::new("kill"))
        .send(other.pid(), Signal::Cont)
        .expect("cont");
}

#[tokio::test]
async fn work_kept_for_a_client_that_died_during_the_game_waits_until_it_is_back() {
    let mock = Mock::new(OWN);
    let (_dir, app) = frozen_client(&mock);
    let source = source_in_client(&app);
    app.db
        .write_blocking(move |c| {
            let rom = sources::fixtures::seed_rom(c, "nes", "Example Quest (USA).nes", 16, &[])?;
            crate::db::downloads_import::insert_fixture(c, rom, source, 0, "done", None)?;
            Ok(())
        })
        .expect("placed");
    defer(&app, Op::Seed).await;
    defer(&app, Op::Deselect(source)).await;
    assert!(!crate::jobs::import::release_source(&app, source).await);

    mock.set_unreachable(true);
    app.set_client_hold(None);
    assert!(
        replay_deferred(&app).await.is_err(),
        "a refusing client is retried"
    );
    let kept = [Op::Seed, Op::Deselect(source), Op::Release(source)];
    assert_eq!(waiting(&app).await, kept);
    let tries = app.db.read(deferred::get).await.expect("kept");
    assert!(tries.iter().all(|e| e.tries == 1), "{tries:?}");

    app.clear_client();
    replay_deferred(&app).await.expect("no client waits");
    assert_eq!(waiting(&app).await, kept, "nothing is lost with no client");

    mock.set_unreachable(false);
    app.set_client_at(nas(), Arc::clone(&mock) as Arc<dyn DownloadClient>);
    replay_deferred(&app).await.expect("replay");
    assert_eq!(
        mock.calls(),
        ["seed:t", "stop:t", "wanted:t:[]", "remove:t"]
    );
    assert!(waiting(&app).await.is_empty());
    let row = app
        .db
        .read(move |c| sources::get(c, source))
        .await
        .expect("get")
        .expect("row");
    assert_eq!(
        row.client_id, None,
        "a torrent under \"none\" stops seeding"
    );
}

#[tokio::test]
async fn a_deselect_with_no_client_waits_for_one() {
    let mock = Mock::new(OWN);
    let (_dir, app) = state_with(fast);
    let source = source_in_client(&app);
    Scheduler::run_inline(&app, Arc::new(Deselect { source_id: source }))
        .await
        .expect("deselect");
    assert_eq!(waiting(&app).await, [Op::Deselect(source)]);
    app.set_client_at(nas(), Arc::clone(&mock) as Arc<dyn DownloadClient>);
    replay_deferred(&app).await.expect("replay");
    assert_eq!(mock.calls(), ["stop:t", "wanted:t:[]"]);
}

#[test]
fn a_due_previous_entry_never_wakes_the_gate_while_the_client_is_frozen() {
    let saved = SavedLimits {
        client: gone_socket(),
        down: None,
        up: Some(OWN),
        alt_up: None,
    };
    let a = Applied {
        client: None,
        have: Target::default(),
        alt_read: false,
        alt_held: false,
        saved: None,
        previous: vec![PreviousLimits {
            saved,
            since: 0,
            tries: 0,
            next_at: 0,
        }],
        frozen: None,
        refused: None,
    };
    assert_eq!(
        a.previous_at(true),
        None,
        "no try runs while frozen, so none is due"
    );
    let due = a.previous_at(false).expect("due");
    assert!(due <= Instant::now());
}

#[tokio::test]
async fn a_refused_kept_work_neither_stops_the_recheck_nor_retries_faster_than_the_backoff() {
    let mock = Mock::new(OWN);
    let (_dir, app) = start_at(
        &mock,
        nas(),
        MENU,
        |o| o.hold_recheck = Duration::from_millis(30),
        None,
    );
    app.update_config(|c| c.limits.down_kbps_core = 0);
    source_in_client(&app);
    *mock.refuse_seed.lock().expect("lock") = true;
    app.gate.set_corename(Some("SNES".into()));
    wait_calls(&mock, 2).await;
    defer(&app, Op::Seed).await;
    mock.set_up(RateLimit::default());
    wait_for("the hold again", || {
        mock.calls()
            .iter()
            .filter(|c| *c == "set:up:true/0")
            .count()
            == 2
    })
    .await;
    tokio::time::sleep(Duration::from_millis(300)).await;
    let tries = mock
        .calls()
        .iter()
        .filter(|c| c.starts_with("seed:"))
        .count();
    // Retries at 10, 20, 40, 80 and 160 ms after each failure: a handful, not one per wake.
    assert!((2..=8).contains(&tries), "{tries} seed tries");
    assert_eq!(waiting(&app).await, [Op::Seed]);
}

#[tokio::test]
async fn the_same_daemon_under_a_new_address_gets_its_own_limit_back_before_it_is_read() {
    use mistarr_clients::fake::{FakeResponse, FakeServer};
    let fake = FakeServer::start().await.expect("fake");
    let port = fake.addr().port();
    let at = |host: &str| remote(&format!("http://{host}:{port}/transmission/rpc"));
    let handle = |e: &ClientEndpoint| {
        ClientKey {
            kind: e.kind,
            url: e.url.clone(),
            path_map: Vec::new(),
        }
        .build()
        .expect("client")
    };
    let own = json!({ "speed-limit-up": 30, "speed-limit-up-enabled": true });
    let (_dir, app) = state_with(fast);
    app.update_config(|c| c.limits.down_kbps_core = 0);
    fake.push(FakeResponse::success(own.clone()));
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    app.set_client_at(at("localhost"), handle(&at("localhost")));
    app.gate.set_corename(Some("SNES".into()));
    tokio::spawn(follow_gate(Arc::clone(&app)));
    let sets = || {
        fake.bodies()
            .into_iter()
            .filter(|b| b["method"] == "session-set" || b["method"] == "session-get")
            .map(|b| b["arguments"].clone())
            .collect::<Vec<_>>()
    };
    wait_for("the first hold", || sets().len() == 3).await;
    assert_eq!(
        sets()[2],
        json!({ "speed-limit-up": 0, "speed-limit-up-enabled": true })
    );

    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(own.clone()));
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    app.set_client_at(at("127.0.0.1"), handle(&at("127.0.0.1")));
    wait_for("the second hold", || sets().len() == 7).await;
    let calls = sets();
    assert_eq!(
        calls[3], own,
        "the own limit is put back before the new address reads it"
    );
    assert_eq!(
        calls[4],
        json!({ "fields": ["speed-limit-up", "speed-limit-up-enabled"] })
    );
    assert_eq!(
        stored(&app).await.and_then(|s| s.up),
        Some(RateLimit::kbps(30))
    );
}
