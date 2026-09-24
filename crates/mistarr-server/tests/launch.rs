//! The launch routes over HTTP, with MiSTer Main replaced by a recording sink.

mod common;

use std::sync::Arc;

use common::{boot, request, request_plain};
use mistarr_core::PlatformId;
use mistarr_mister::launch::{CommandSink, RecordingSink};
use mistarr_server::db::files::{self, FileState, Hashed};

fn touch(path: &std::path::Path) {
    std::fs::create_dir_all(path.parent().expect("parent")).expect("mkdir");
    std::fs::write(path, b"").expect("write");
}

#[tokio::test]
async fn launch_routes_answer_with_documented_statuses() {
    let b = boot().await;
    let addr = b.addr();
    let post = |path: String| async move { request(addr, "POST", &path, &[], None).await };

    let r = post("/api/v1/platforms/snes/launch-core".into()).await;
    assert_eq!(r.status, 503, "{}", r.body);
    assert_eq!(r.json()["error"]["code"], "unavailable");
    let status = request(addr, "GET", "/api/v1/system/status", &[], None).await;
    assert_eq!(status.json()["launch"], "unavailable");

    let sink = Arc::new(RecordingSink::new());
    b.running
        .app
        .set_command_sink(Arc::clone(&sink) as Arc<dyn CommandSink>);
    let status = request(addr, "GET", "/api/v1/system/status", &[], None).await;
    assert_eq!(status.json()["launch"], "ready");

    assert_eq!(
        post("/api/v1/platforms/snes/launch-core".into())
            .await
            .status,
        409
    );
    assert_eq!(
        post("/api/v1/platforms/none/launch-core".into())
            .await
            .status,
        404
    );
    assert_eq!(post("/api/v1/titles/999/launch".into()).await.status, 404);

    touch(&b.dir.path().join("_Console/NES_20240101.rbf"));
    let r = post("/api/v1/platforms/nes/launch-core".into()).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["core"], "_Console/NES_20240101.rbf");

    let pid = PlatformId("nes".into());
    let id = b
        .running
        .app
        .db
        .write(move |c| {
            let t = files::seed_title_fixture(c, &pid, "Example Quest (USA)")?;
            let hashes = mistarr_core::HashSet {
                size: 4,
                crc32: "00000001".into(),
                md5: "0".repeat(32),
                sha1: "1".repeat(40),
            };
            let rom = files::seed_rom_for_title_fixture(c, t, "a.nes", &hashes, "good")?;
            let hashed = Hashed::default();
            files::upsert(
                c,
                &pid,
                "NES/a.nes",
                4,
                0,
                &hashed,
                Some(rom),
                FileState::Pending,
                0,
            )?;
            Ok(t)
        })
        .await
        .expect("seed");
    assert_eq!(
        post(format!("/api/v1/titles/{id}/launch")).await.status,
        409
    );
    b.running
        .app
        .db
        .write(|c| {
            c.execute("UPDATE files SET state = 'verified'", [])?;
            Ok(())
        })
        .await
        .expect("verify");
    let r = post(format!("/api/v1/titles/{id}/launch")).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["file"], "NES/a.nes");
    assert_eq!(sink.lines().len(), 2);

    let put = request(
        addr,
        "PUT",
        "/api/v1/system/settings",
        &[],
        Some(r#"{"prefs":{"launch":false}}"#),
    )
    .await;
    assert_eq!(put.status, 200, "{}", put.body);
    assert_eq!(put.json()["prefs"]["launch"], false);
    let status = request(addr, "GET", "/api/v1/system/status", &[], None).await;
    assert_eq!(status.json()["launch"], "disabled");
    assert_eq!(
        post(format!("/api/v1/titles/{id}/launch")).await.status,
        409
    );
    assert_eq!(sink.lines().len(), 2);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn writes_from_another_site_are_refused() {
    let b = boot().await;
    let addr = b.addr();
    let path = "/api/v1/system/pause";
    let r = request_plain(addr, "POST", path, &[], None).await;
    assert_eq!(r.status, 403, "{}", r.body);
    assert_eq!(r.json()["error"]["code"], "forbidden");
    let cross = [("X-Mistarr", "1"), ("Sec-Fetch-Site", "cross-site")];
    assert_eq!(
        request_plain(addr, "POST", path, &cross, None).await.status,
        403
    );
    let origin = [("X-Mistarr", "1"), ("Origin", "http://other.example")];
    assert_eq!(
        request_plain(addr, "POST", path, &origin, None)
            .await
            .status,
        403
    );
    let own = format!("http://{addr}");
    let same = [("X-Mistarr", "1"), ("Origin", own.as_str())];
    assert_eq!(
        request_plain(addr, "POST", path, &same, None).await.status,
        200
    );
    assert_eq!(
        request_plain(addr, "GET", "/api/v1/system/status", &[], None)
            .await
            .status,
        200
    );
    b.running.shutdown().await.expect("shutdown");
}
