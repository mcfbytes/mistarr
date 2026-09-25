//! Source import, binding and the Sources API against a booted server.

mod common;

use std::path::PathBuf;
use std::time::Duration;

use common::{
    boot, boot_with_options, config_in, eventually, get, options_in, request, request_bytes,
    Booted, Sse,
};
use mistarr_clients::fake::{FakeResponse, FakeScgiServer, FakeServer, ScgiReply};
use mistarr_clients::xmlrpc::Value as Xml;
use mistarr_server::config::ClientChoice;
use mistarr_server::db::sources::fixtures::seed_rom;
use serde_json::{json, Value};

/// Transmission's answer to the existence check before start and stop.
fn exists() -> FakeResponse {
    FakeResponse::success(json!({ "torrents": [{ "id": 1 }] }))
}

/// A synthetic infohash of one repeated byte.
fn hash(byte: u8) -> String {
    format!("{byte:02x}").repeat(20)
}

fn bstr(s: &str) -> String {
    format!("{}:{s}", s.len())
}

/// A synthetic multi-file `.torrent` whose files sit under a subdirectory.
fn torrent(name: &str, files: &[(&str, u64)]) -> Vec<u8> {
    let mut list = String::from("l");
    for (file, len) in files {
        list.push_str(&format!(
            "d6:lengthi{len}e4:pathl{}{}ee",
            bstr("NES"),
            bstr(file)
        ));
    }
    list.push('e');
    let info = format!(
        "d5:files{list}4:name{}12:piece lengthi16384e6:pieces0:e",
        bstr(name)
    );
    let announce = bstr("http://tracker.invalid/announce");
    format!("d8:announce{announce}4:info{info}e").into_bytes()
}

/// Three roms on `nes` that the synthetic torrents refer to.
fn seed_catalog(b: &Booted) {
    b.running
        .app
        .db
        .write_blocking(|c| {
            seed_rom(c, "nes", "Example Quest (USA).nes", 40_976, &[])?;
            seed_rom(c, "nes", "Second Try (Japan).nes", 24_592, &[])?;
            seed_rom(c, "nes", "Third Tale (Europe).nes", 65_552, &[])?;
            Ok(())
        })
        .expect("seed");
}

fn sources_dir(b: &Booted) -> PathBuf {
    b.running.app.config().paths.sources()
}

/// Three of four files match `nes`: two by name, one by base name and size.
fn matching_set(name: &str) -> Vec<u8> {
    torrent(
        name,
        &[
            ("Example Quest (USA).nes", 40_976),
            ("Second Try (Europe).nes", 24_592),
            ("Third Tale (Europe).nes", 65_552),
            ("readme.txt", 120),
        ],
    )
}

/// One of four files matches `nes`.
fn sparse_set(name: &str) -> Vec<u8> {
    torrent(
        name,
        &[
            ("Example Quest (USA).nes", 40_976),
            ("Unlisted One (USA).nes", 1_000),
            ("Unlisted Two (USA).nes", 2_000),
            ("notes.txt", 10),
        ],
    )
}

async fn sources(b: &Booted) -> Vec<Value> {
    let r = get(b.addr(), "/api/v1/sources").await;
    assert_eq!(r.status, 200, "{}", r.body);
    r.json()["items"].as_array().cloned().unwrap_or_default()
}

async fn only_source(b: &Booted) -> Value {
    let items = sources(b).await;
    assert_eq!(items.len(), 1, "{items:?}");
    items[0].clone()
}

async fn put(b: &Booted, id: &Value, body: &Value) -> common::Response {
    let path = format!("/api/v1/sources/{id}");
    request(b.addr(), "PUT", &path, &[], Some(&body.to_string())).await
}

#[tokio::test]
async fn dropped_torrent_binds_and_records_matches() {
    let b = boot().await;
    seed_catalog(&b);
    let mut sse = Sse::open(b.addr(), "/api/v1/events", &[]).await;
    let dir = sources_dir(&b);
    std::fs::write(dir.join("set.torrent"), matching_set("Synthetic Set")).expect("write");
    sse.until("event: source.changed").await;
    assert!(sse.text.contains(r#""state":"bound""#), "{}", sse.text);
    assert!(sse.text.contains(r#""platform_id":"nes""#), "{}", sse.text);

    let s = only_source(&b).await;
    assert_eq!(s["state"], "bound");
    assert_eq!(s["platform_id"], "nes");
    assert_eq!(s["display_name"], "Synthetic Set");
    assert_eq!(s["origin_file"], "set.torrent");
    assert_eq!(
        (s["file_count"].clone(), s["matched_count"].clone()),
        (json!(4), json!(3))
    );
    assert_eq!(s["bind_score"], 0.75);
    assert_eq!(s["seed_policy"], "none");
    assert_eq!(s["reason"], Value::Null);
    assert!(dir.join("loaded/set.torrent").is_file());
    assert!(!dir.join("set.torrent").exists());

    let files = get(b.addr(), &format!("/api/v1/sources/{}/files", s["id"])).await;
    assert_eq!(files.status, 200);
    let files = files.json();
    assert_eq!(files["total"], 4);
    let items = files["items"].as_array().expect("items");
    let got: Vec<(Value, Value, Value)> = items
        .iter()
        .map(|f| {
            (
                f["path"].clone(),
                f["rom_name"].clone(),
                f["confidence"].clone(),
            )
        })
        .collect();
    assert_eq!(
        got,
        [
            (
                json!("NES/Example Quest (USA).nes"),
                json!("Example Quest (USA).nes"),
                json!("name")
            ),
            (
                json!("NES/Second Try (Europe).nes"),
                json!("Second Try (Japan).nes"),
                json!("base")
            ),
            (
                json!("NES/Third Tale (Europe).nes"),
                json!("Third Tale (Europe).nes"),
                json!("name")
            ),
            (json!("NES/readme.txt"), Value::Null, Value::Null),
        ]
    );
    let missing = get(b.addr(), "/api/v1/sources/999/files").await;
    assert_eq!(missing.status, 404);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn below_threshold_stays_unbound_until_bound_by_hand() {
    let b = boot().await;
    seed_catalog(&b);
    let dir = sources_dir(&b);
    std::fs::write(dir.join("sparse.torrent"), sparse_set("Sparse Set")).expect("write");
    eventually("an unbound source", || async {
        sources(&b).await.len() == 1
    })
    .await;
    let s = only_source(&b).await;
    assert_eq!(s["state"], "unbound");
    assert_eq!(s["platform_id"], Value::Null);
    assert_eq!(s["matched_count"], 0);
    let reason = s["reason"].as_str().expect("reason");
    assert!(reason.contains("60%"), "{reason}");

    let r = put(&b, &s["id"], &json!({ "platform_id": "no-such" })).await;
    assert_eq!(r.status, 400);
    let r = put(&b, &s["id"], &json!({ "platform_id": "nes" })).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert!(r.json()["job_id"].is_i64(), "{}", r.body);
    assert_eq!(r.json()["user_binding"], true);
    assert_eq!(
        r.json()["pending_binding"],
        json!({ "automatic": false, "platform_id": "nes" })
    );
    eventually("the source bound by hand", || async {
        only_source(&b).await["state"] == "bound"
    })
    .await;
    let s = only_source(&b).await;
    assert_eq!(s["platform_id"], "nes");
    assert_eq!(
        (s["matched_count"].clone(), s["bind_score"].clone()),
        (json!(1), json!(0.25))
    );
    let r = put(&b, &s["id"], &json!({ "platform_id": null })).await;
    assert_eq!(r.status, 202, "{}", r.body);
    eventually("the source set aside", || async {
        only_source(&b).await["state"] == "unbound"
    })
    .await;
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn detail_files_preview_and_reset_to_automatic() {
    let b = boot().await;
    seed_catalog(&b);
    let dir = sources_dir(&b);
    std::fs::write(dir.join("set.torrent"), matching_set("Detail Set")).expect("write");
    eventually("a bound source", || async {
        sources(&b)
            .await
            .first()
            .is_some_and(|s| s["state"] == "bound")
    })
    .await;
    let id = only_source(&b).await["id"].clone();
    assert_eq!(only_source(&b).await["user_binding"], false);

    let d = get(b.addr(), &format!("/api/v1/sources/{id}")).await;
    assert_eq!(d.status, 200, "{}", d.body);
    let d = d.json();
    assert_eq!(d["display_name"], "Detail Set");
    assert_eq!(
        d["summary"],
        json!({ "matched": 3, "candidates": 0, "unmatched": 0, "extra": 1, "wanted": 0 })
    );
    assert_eq!(d["dats"][0]["matched"], 3);
    assert_eq!(d["transfer"]["files"], 0);
    assert_eq!(get(b.addr(), "/api/v1/sources/999").await.status, 404);

    let files = |query: &'static str| {
        let path = format!("/api/v1/sources/{id}/files{query}");
        let addr = b.addr();
        async move { get(addr, &path).await }
    };
    let r = files("?filter=unmatched").await.json();
    assert_eq!(r["total"], 1);
    assert_eq!(r["items"][0]["kind"], "extra");
    assert_eq!(r["items"][0]["unmatched"], "extra");
    let r = files("?filter=matched&limit=2&offset=2").await.json();
    assert_eq!(
        (r["total"].clone(), r["items"].as_array().map(Vec::len)),
        (json!(3), Some(1))
    );
    assert_eq!(files("?q=second").await.json()["total"], 1);
    assert_eq!(files("?filter=wanted").await.json()["total"], 0);
    assert_eq!(files("?filter=other").await.status, 400);

    let p = get(b.addr(), &format!("/api/v1/sources/{id}/preview")).await;
    assert_eq!(p.status, 200, "{}", p.body);
    assert_eq!(
        p.json(),
        json!({ "total": 4, "sampled": 4, "platforms": [{ "platform_id": "nes", "matched": 3 }] })
    );

    let bad = json!({ "platform_id": "nes", "binding": "automatic" });
    assert_eq!(put(&b, &id, &bad).await.status, 400);
    assert_eq!(put(&b, &id, &json!({ "binding": "x" })).await.status, 400);
    let r = put(&b, &id, &json!({ "platform_id": null })).await;
    assert_eq!(r.status, 202, "{}", r.body);
    eventually("the source set aside", || async {
        only_source(&b).await["state"] == "unbound"
    })
    .await;
    assert_eq!(only_source(&b).await["user_binding"], true);
    let r = put(&b, &id, &json!({ "binding": "automatic" })).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["user_binding"], false);
    eventually("the source bound again", || async {
        only_source(&b).await["state"] == "bound"
    })
    .await;
    let s = only_source(&b).await;
    assert_eq!(
        (s["platform_id"].clone(), s["matched_count"].clone()),
        (json!("nes"), json!(3))
    );
    let r = put(&b, &id, &json!({ "seed_policy": "client" })).await;
    assert_eq!((r.status, r.json()["job_id"].clone()), (200, Value::Null));
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn a_seed_policy_no_client_took_is_kept_for_one() {
    use mistarr_server::db::{deferred, sources::SourceId};
    let b = boot().await;
    seed_catalog(&b);
    std::fs::write(sources_dir(&b).join("s.torrent"), matching_set("Seed Set")).expect("write");
    eventually("a source", || async { sources(&b).await.len() == 1 }).await;
    let id = only_source(&b).await["id"].clone();
    let source = SourceId(id.as_i64().expect("id"));
    let db = b.running.app.db.clone();
    db.write(move |c| mistarr_server::db::sources::set_client_id(c, source, Some("t")))
        .await
        .expect("client id");
    let r = put(&b, &id, &json!({ "seed_policy": "ratio:1.5" })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    let kept = db.read(deferred::get).await.expect("kept");
    assert_eq!(
        kept.iter().map(|e| e.op).collect::<Vec<_>>(),
        [deferred::Op::Seed],
        "with no client the policy waits for one"
    );
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn seed_policy_disable_and_delete() {
    let b = boot().await;
    seed_catalog(&b);
    std::fs::write(sources_dir(&b).join("s.torrent"), matching_set("Seed Set")).expect("write");
    eventually("a source", || async { sources(&b).await.len() == 1 }).await;
    let id = only_source(&b).await["id"].clone();

    let r = put(&b, &id, &json!({ "seed_policy": "ratio:1.5" })).await;
    assert_eq!(r.status, 200, "{}", r.body);
    assert_eq!(r.json()["seed_policy"], "ratio:1.5");
    for bad in [
        json!({ "seed_policy": "ratio:0" }),
        json!({ "state": "gone" }),
        json!({ "x": 1 }),
    ] {
        assert_eq!(put(&b, &id, &bad).await.status, 400, "{bad}");
    }
    let r = put(&b, &id, &json!({ "state": "disabled" })).await;
    assert_eq!(r.json()["state"], "disabled");
    let r = put(&b, &id, &json!({ "state": "enabled" })).await;
    assert_eq!(r.json()["state"], "bound");
    assert_eq!(put(&b, &json!(999), &json!({})).await.status, 404);

    let path = format!("/api/v1/sources/{id}");
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 204, "{}", r.body);
    assert!(sources(&b).await.is_empty());
    let r = request(b.addr(), "DELETE", &path, &[], None).await;
    assert_eq!(r.status, 404);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn upload_takes_a_torrent_or_a_magnet() {
    let b = boot().await;
    seed_catalog(&b);
    let data = matching_set("Uploaded Set");
    let mut body =
        b"--bnd\r\nContent-Disposition: form-data; name=\"file\"; filename=\"up.torrent\"\r\n\r\n"
            .to_vec();
    body.extend_from_slice(&data);
    body.extend_from_slice(b"\r\n--bnd--\r\n");
    let multipart = "multipart/form-data; boundary=bnd";
    let r = request_bytes(b.addr(), "POST", "/api/v1/sources/upload", multipart, &body).await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["file"], "up.torrent");
    eventually("the uploaded torrent bound", || async {
        sources(&b)
            .await
            .first()
            .is_some_and(|s| s["state"] == "bound")
    })
    .await;
    let again = request_bytes(b.addr(), "POST", "/api/v1/sources/upload", multipart, &body).await;
    assert_eq!(again.status, 400);

    let uri = format!(
        "magnet:?xt=urn:btih:{}&dn=Magnet%20Set&tr=http%3A%2F%2Ftracker.invalid%2Fannounce",
        hash(0x5a)
    );
    let json_body = json!({ "magnet": uri }).to_string();
    let r = request(
        b.addr(),
        "POST",
        "/api/v1/sources/upload",
        &[],
        Some(&json_body),
    )
    .await;
    assert_eq!(r.status, 202, "{}", r.body);
    assert_eq!(r.json()["file"], "Magnet Set.magnet");
    eventually("the magnet source", || async {
        sources(&b).await.len() == 2
    })
    .await;
    let m = sources(&b).await[1].clone();
    assert_eq!(m["state"], "resolving");
    assert_eq!(m["infohash"], hash(0x5a));
    assert_eq!(m["display_name"], "Magnet Set");
    eventually("a reason on the resolving source", || async {
        sources(&b).await[1]["reason"].is_string()
    })
    .await;
    let r = request(
        b.addr(),
        "POST",
        "/api/v1/sources/upload",
        &[],
        Some(&json_body),
    )
    .await;
    assert_eq!(r.status, 400, "a second copy of a resolving magnet");

    let bad = json!({ "magnet": "magnet:?dn=nothing" }).to_string();
    let r = request(b.addr(), "POST", "/api/v1/sources/upload", &[], Some(&bad)).await;
    assert_eq!(r.status, 400);
    let junk = b"--bnd\r\nContent-Disposition: form-data; name=\"file\"; filename=\"x.torrent\"\r\n\r\nnot bencode\r\n--bnd--\r\n";
    let r = request_bytes(b.addr(), "POST", "/api/v1/sources/upload", multipart, junk).await;
    assert_eq!(r.status, 400);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn malformed_file_is_rejected_with_a_reason() {
    let b = boot().await;
    let dir = sources_dir(&b);
    std::fs::write(dir.join("broken.torrent"), b"d4:infoi1ee").expect("write");
    let reason = dir.join("rejected/broken.torrent.reason.txt");
    eventually("the rejected file", || async { reason.is_file() }).await;
    assert!(dir.join("rejected/broken.torrent").is_file());
    let text = std::fs::read_to_string(&reason).expect("reason");
    assert!(text.starts_with("Not a valid .torrent file"), "{text}");
    assert!(sources(&b).await.is_empty());
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn magnet_resolves_through_the_client() {
    let fake = FakeServer::start().await.expect("fake");
    let h = hash(0xab);
    fake.push(FakeResponse::success(json!({ "version": "4.0.5" })));
    fake.push(FakeResponse::success(json!({
        "torrent-added": { "id": 1, "name": "Magnet Set", "hashString": h }
    })));
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "wanted": [] }] }),
    ));
    fake.push(FakeResponse::success(json!({})));
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "name": "Magnet Set", "files": [] }] }),
    ));
    let listed: Vec<Value> = [
        ("Example Quest (USA).nes", 40_976),
        ("Second Try (Europe).nes", 24_592),
        ("extra.txt", 3),
    ]
    .iter()
    .map(
        |(f, n)| json!({ "name": format!("Magnet Set/NES/{f}"), "length": n, "bytesCompleted": 0 }),
    )
    .collect();
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "name": "Magnet Set", "files": listed }] }),
    ));
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "wanted": [true, true, true] }] }),
    ));
    fake.push(FakeResponse::success(json!({})));

    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.client.url = fake.url();
    // Only the started-magnet cadence can pick up the listing in time.
    let mut options = options_in(dir.path());
    options.magnet_poll = Duration::from_secs(3600);
    let b = boot_with_options(dir, config, options).await;
    assert!(b.running.app.client().is_some());
    seed_catalog(&b);
    let mut sse = Sse::open(b.addr(), "/api/v1/events", &[]).await;
    let uri = format!(
        "magnet:?xt=urn:btih:{h}&dn=Magnet%20Set&tr=http%3A%2F%2Ftracker.invalid%2Fannounce"
    );
    std::fs::write(sources_dir(&b).join("m.magnet"), format!("{uri}\n")).expect("write");
    sse.until(r#""state":"resolving""#).await;
    sse.until(r#""state":"bound""#).await;

    let s = only_source(&b).await;
    assert_eq!(s["state"], "bound");
    assert_eq!(s["platform_id"], "nes");
    assert_eq!(s["client_id"], h.as_str());
    assert_eq!(
        (s["file_count"].clone(), s["matched_count"].clone()),
        (json!(3), json!(2))
    );
    assert_eq!(s["reason"], Value::Null);
    let files = get(b.addr(), &format!("/api/v1/sources/{}/files", s["id"]))
        .await
        .json();
    assert_eq!(files["items"][0]["path"], "NES/Example Quest (USA).nes");
    assert_eq!(files["items"][1]["confidence"], "base");

    let bodies = fake.bodies();
    let methods: Vec<&str> = bodies
        .iter()
        .map(|b| b["method"].as_str().unwrap_or(""))
        .collect();
    // The scripted listing only lines up if the torrent was started first.
    assert_eq!(
        methods,
        [
            "session-get",
            "torrent-add",
            "torrent-get",
            "torrent-set",
            "torrent-get",
            "torrent-start",
            "torrent-get",
            "torrent-get",
            "torrent-get",
            "torrent-stop",
            "torrent-get",
            "torrent-set",
        ]
    );
    assert_eq!(bodies[6]["arguments"]["fields"], json!(["name", "files"]));
    let add = &bodies[1];
    assert_eq!(add["method"], "torrent-add");
    assert_eq!(add["arguments"]["paused"], true);
    assert_eq!(add["arguments"]["filename"], uri.as_str());
    let staging = b.running.app.config().paths.staging().join(&h);
    assert_eq!(
        add["arguments"]["download-dir"],
        staging.to_string_lossy().as_ref()
    );
    assert_eq!(bodies[11]["arguments"]["files-unwanted"], json!([0, 1, 2]));
    b.running.shutdown().await.expect("shutdown");
}

fn xml_ok() -> ScgiReply {
    ScgiReply::Value(Xml::Int(0))
}

fn xml_multicall(values: Vec<Xml>) -> ScgiReply {
    ScgiReply::multicall(values)
}

#[tokio::test]
async fn magnet_resolves_through_rtorrent() {
    let fake = FakeScgiServer::start().await.expect("fake");
    let h = hash(0xcd);
    fake.push(ScgiReply::fault(-501, "Could not find info-hash."));
    fake.push(xml_ok());
    fake.push(xml_ok());
    fake.push(xml_multicall(vec![Xml::Int(1), Xml::Int(0)]));
    fake.push(xml_ok());
    fake.push(xml_multicall(vec![Xml::Int(1), Xml::Array(vec![])]));
    let row = |p: &str, n: i64| Xml::Array(vec![Xml::from(p), Xml::Int(n)]);
    fake.push(xml_multicall(vec![
        Xml::Int(0),
        Xml::Array(vec![
            row("NES/Example Quest (USA).nes", 40_976),
            row("NES/Third Tale (Europe).nes", 65_552),
        ]),
    ]));
    fake.push(xml_ok());
    fake.push(xml_multicall(vec![Xml::Int(0), Xml::Int(2)]));
    fake.push(xml_multicall(vec![Xml::Int(0), Xml::Int(0)]));
    fake.push(xml_ok());

    let dir = tempfile::tempdir().expect("tempdir");
    let mut config = config_in(dir.path());
    config.client.kind = ClientChoice::Rtorrent;
    config.client.url = fake.addr();
    let mut options = options_in(dir.path());
    options.magnet_poll = Duration::from_secs(3600);
    let b = boot_with_options(dir, config, options).await;
    seed_catalog(&b);
    let mut sse = Sse::open(b.addr(), "/api/v1/events", &[]).await;
    let uri = format!("magnet:?xt=urn:btih:{h}&dn=Rt%20Set");
    std::fs::write(sources_dir(&b).join("rt.magnet"), format!("{uri}\n")).expect("write");
    sse.until(r#""state":"bound""#).await;

    let s = only_source(&b).await;
    assert_eq!(s["platform_id"], "nes");
    assert_eq!(s["matched_count"], 2);
    let calls = fake.calls();
    let names: Vec<String> = calls
        .iter()
        .map(|(m, p)| match (m.as_str(), p.first()) {
            ("system.multicall", Some(Xml::Array(list))) => {
                let first = list.first().and_then(|c| match c {
                    Xml::Struct(fields) => fields.iter().find(|(k, _)| k == "methodName"),
                    _ => None,
                });
                match first {
                    Some((_, Xml::String(inner))) => format!("multi:{inner}"),
                    _ => "multi:?".to_owned(),
                }
            }
            _ => m.clone(),
        })
        .collect();
    // The scripted listing only lines up if the torrent was started first.
    assert_eq!(
        names,
        [
            "d.hash",
            "load.normal",
            "d.directory.set",
            "multi:d.is_meta",
            "d.start",
            "multi:d.is_meta",
            "multi:d.is_meta",
            "d.stop",
            "multi:d.is_meta",
            "multi:f.priority.set",
            "d.update_priorities",
        ]
    );
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn concurrent_uploads_of_one_name_keep_both() {
    let b = boot().await;
    seed_catalog(&b);
    let body = |set: &str| {
        let mut body = b"--bnd\r\nContent-Disposition: form-data; name=\"file\"; filename=\"same.torrent\"\r\n\r\n".to_vec();
        body.extend_from_slice(&matching_set(set));
        body.extend_from_slice(b"\r\n--bnd--\r\n");
        body
    };
    let (one, two) = (body("First Set"), body("Second Set"));
    let multipart = "multipart/form-data; boundary=bnd";
    let path = "/api/v1/sources/upload";
    let (a, c) = tokio::join!(
        request_bytes(b.addr(), "POST", path, multipart, &one),
        request_bytes(b.addr(), "POST", path, multipart, &two),
    );
    assert_eq!((a.status, c.status), (202, 202), "{} {}", a.body, c.body);
    let mut files = [a.json()["file"].clone(), c.json()["file"].clone()];
    files.sort_by_key(ToString::to_string);
    assert_eq!(files, [json!("same (1).torrent"), json!("same.torrent")]);
    eventually("both sources", || async { sources(&b).await.len() == 2 }).await;
    let mut names: Vec<String> = sources(&b)
        .await
        .iter()
        .map(|s| s["display_name"].as_str().unwrap_or("").to_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["First Set", "Second Set"]);
    b.running.shutdown().await.expect("shutdown");
}

#[tokio::test]
async fn unstarted_magnets_keep_the_slow_cadence() {
    let dir = tempfile::tempdir().expect("tempdir");
    let config = config_in(dir.path());
    let mut options = options_in(dir.path());
    options.magnet_poll = Duration::from_secs(3600);
    let b = boot_with_options(dir, config, options).await;
    let uri = format!("magnet:?xt=urn:btih:{}&dn=Slow%20Set", hash(0x3c));
    std::fs::write(sources_dir(&b).join("slow.magnet"), format!("{uri}\n")).expect("write");
    eventually("a reason from the unreachable client", || async {
        sources(&b)
            .await
            .first()
            .is_some_and(|s| s["reason"].is_string())
    })
    .await;
    tokio::time::sleep(Duration::from_millis(400)).await;
    let app = &b.running.app;
    let runs = app
        .db
        .read(|c| mistarr_server::db::jobs::count_kind(c, "resolve_magnet"))
        .await
        .expect("count");
    // The import's own run, plus at most the first slow tick.
    assert!(
        runs <= 2,
        "{runs} resolve runs for a magnet the client never took"
    );
    assert_eq!(only_source(&b).await["client_id"], Value::Null);
    b.running.shutdown().await.expect("shutdown");
}

/// Holds the writer in one open transaction for `hold`, as a DAT apply does, and
/// returns once it is held.
fn hold_writer(b: &Booted, hold: Duration) -> std::thread::JoinHandle<()> {
    let db = b.running.app.db.clone();
    let (held, is_held) = std::sync::mpsc::channel();
    let holder = std::thread::spawn(move || {
        db.write_blocking(|c| {
            let tx = c.transaction()?;
            mistarr_server::db::settings::set(&tx, "test.held", "1")?;
            let _ = held.send(());
            std::thread::sleep(hold);
            mistarr_server::db::commit(tx)
        })
        .expect("held write");
    });
    is_held.recv().expect("writer held");
    holder
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn uploads_answer_promptly_while_the_writer_is_held() {
    let b = boot().await;
    seed_catalog(&b);
    let multipart = "multipart/form-data; boundary=bnd";
    let part = |name: &str, data: &[u8]| {
        let mut body = format!(
            "--bnd\r\nContent-Disposition: form-data; name=\"file\"; filename=\"{name}\"\r\n\r\n"
        )
        .into_bytes();
        body.extend_from_slice(data);
        body.extend_from_slice(b"\r\n--bnd--\r\n");
        body
    };
    let torrent_body = part("held.torrent", &matching_set("Held Set"));
    let dat_body = part(
        "held.dat",
        b"<datafile><header><name>Held</name></header></datafile>",
    );
    let uri = format!("magnet:?xt=urn:btih:{}&dn=Held%20Magnet", hash(0x6b));
    let magnet_body = json!({ "magnet": uri }).to_string();
    let upload = "/api/v1/sources/upload";

    let holder = hold_writer(&b, Duration::from_secs(4));
    let started = std::time::Instant::now();
    let t = request_bytes(b.addr(), "POST", upload, multipart, &torrent_body).await;
    let torrent_took = started.elapsed();
    let started = std::time::Instant::now();
    let m = request(b.addr(), "POST", upload, &[], Some(&magnet_body)).await;
    let magnet_took = started.elapsed();
    let started = std::time::Instant::now();
    let d = request_bytes(
        b.addr(),
        "POST",
        "/api/v1/dats/upload",
        multipart,
        &dat_body,
    )
    .await;
    let dat_took = started.elapsed();
    eprintln!("while held: torrent {torrent_took:?}, magnet {magnet_took:?}, dat {dat_took:?}");
    for (r, took) in [(&t, torrent_took), (&m, magnet_took), (&d, dat_took)] {
        assert_eq!(r.status, 202, "{}", r.body);
        assert!(took < Duration::from_secs(1), "answered in {took:?}");
    }
    holder.join().expect("holder");
    eventually("both uploaded sources", || async {
        sources(&b).await.len() == 2
    })
    .await;
    b.running.shutdown().await.expect("shutdown");
}
