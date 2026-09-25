use std::path::{Path, PathBuf};
use std::process::{Child, Command};
use std::sync::Mutex;

use async_trait::async_trait;
use mistarr_clients::{ClientError, ClientFile, ClientInfo, TorrentSource, TorrentStatus};

use super::*;
use crate::app::testutil::{state_with, TestDir};
use crate::app::Options;
use crate::jobs::gate::MENU;

/// A client whose global limits live in memory and whose calls are recorded;
/// `down` makes every call fail.
struct Mock {
    calls: Mutex<Vec<String>>,
    down_limit: Mutex<RateLimit>,
    up_limit: Mutex<RateLimit>,
    unreachable: Mutex<bool>,
    pid: Mutex<Option<u32>>,
}

impl Mock {
    fn new(up: RateLimit) -> Arc<Self> {
        Arc::new(Self {
            calls: Mutex::new(Vec::new()),
            down_limit: Mutex::new(RateLimit::default()),
            up_limit: Mutex::new(up),
            unreachable: Mutex::new(false),
            pid: Mutex::new(None),
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
    async fn set_wanted(&self, _id: &ClientTorrentId, _w: &[u32]) -> mistarr_clients::Result<()> {
        nope()
    }
    async fn set_seed_policy(
        &self,
        id: &ClientTorrentId,
        _seed: SeedPolicy,
    ) -> mistarr_clients::Result<()> {
        self.log(format!("seed:{id}"))
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
    let mock = Mock::new(OWN);
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
    assert_eq!(calls[4..], ["set:down:false/0", "set:up:true/40"]);
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
    let mock = Mock::new(OWN);
    let (_dir, app) = start(&mock);
    app.gate.set_corename(Some("SNES".into()));
    wait_calls(&mock, 4).await;
    app.update_config(|c| c.transfer.pause_client_while_playing = false);
    app.limits_wake.notify_one();
    let calls = wait_calls(&mock, 5).await;
    assert_eq!(calls[4..], ["set:up:true/64"]);
    assert_eq!(app.client_hold(), None);
    assert_eq!(stored(&app).await.and_then(|s| s.up), Some(OWN));
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
    let (_dir, app) = start(&first);
    app.update_config(|c| c.limits.down_kbps_core = 0);
    app.gate.set_corename(Some("SNES".into()));
    assert_eq!(wait_calls(&first, 2).await, ["read:up", "set:up:true/0"]);
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
    assert_eq!(first.calls().len(), 2, "the old client is left alone");
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

const FAKE_ENV: &str = "MISTARR_FAKE_CLIENT";

/// Stands in for a client process when the test binary runs as one.
#[test]
#[ignore = "run only as the fake client process"]
fn fake_client_process() {
    if std::env::var_os(FAKE_ENV).is_some() {
        std::thread::sleep(Duration::from_secs(60));
    }
}

/// This test binary, linked as `rtorrent` so its `exe` link names rtorrent,
/// running [`fake_client_process`].
struct FakeRtorrent {
    child: Child,
    _dir: tempfile::TempDir,
}

impl FakeRtorrent {
    fn spawn() -> Self {
        let me = std::env::current_exe().expect("exe");
        let dir = tempfile::tempdir_in(me.parent().expect("dir")).expect("tempdir");
        let exe = dir.path().join("rtorrent");
        if std::fs::hard_link(&me, &exe).is_err() {
            std::fs::copy(&me, &exe).expect("copy");
        }
        let mut tries = 0;
        let child = loop {
            let spawned = Command::new(&exe)
                .args([
                    "--ignored",
                    "--exact",
                    "jobs::core_limits::tests::fake_client_process",
                ])
                .env(FAKE_ENV, "1")
                .stdout(std::process::Stdio::null())
                .spawn();
            match spawned {
                Ok(child) => break child,
                // A binary just written may still be busy for exec for a moment.
                Err(e) if e.raw_os_error() == Some(26) && tries < 50 => {
                    tries += 1;
                    std::thread::sleep(Duration::from_millis(20));
                }
                Err(e) => panic!("spawn: {e}"),
            }
        };
        Self { child, _dir: dir }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn state(&self) -> char {
        freeze::stat(Path::new("/proc"), self.pid())
            .expect("stat")
            .0
    }
}

impl Drop for FakeRtorrent {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
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
    let proc = FakeRtorrent::spawn();
    let mock = Mock::new(OWN);
    mock.set_pid(proc.pid());
    let (_dir, app) = start_at(&mock, local_rtorrent(), MENU, on_board, None);
    let mut live = app.events.subscribe(None).live;
    app.gate.set_corename(Some("SNES".into()));
    wait_for("the freeze", || proc.state() == 'T').await;
    assert_eq!(app.client_hold(), Some(ClientHold::Frozen));
    assert!(app.client().is_none());
    let recorded = freeze::read_file(&app.options.frozen_file).expect("read");
    assert_eq!(recorded.map(|f| f.pid), Some(proc.pid()));
    assert_eq!(
        mock.calls(),
        ["pid"],
        "no limits are sent to a frozen client"
    );
    let ev = live.recv().await.expect("status");
    assert!(ev.data.contains(r#""client_hold":"frozen""#), "{}", ev.data);

    app.defer_seed_policy(sources::SourceId(1));
    app.gate.set_corename(Some(MENU.into()));
    wait_for("the resume", || proc.state() != 'T').await;
    wait_for("the hold cleared", || app.client_hold().is_none()).await;
    assert!(app.client().is_some());
    assert_eq!(
        freeze::read_file(&app.options.frozen_file).expect("read"),
        None
    );
}

#[tokio::test]
async fn a_client_resumed_elsewhere_is_frozen_again() {
    let proc = FakeRtorrent::spawn();
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
    let proc = FakeRtorrent::spawn();
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
    let proc = FakeRtorrent::spawn();
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
