use std::path::Path;

use serde_json::{json, Value};

use super::*;
use crate::fake::{FakeResponse, FakeServer};
use crate::metainfo::tests::synthetic_metainfo;

/// A synthetic 40-hex-digit infohash built from one repeated byte.
fn hash(byte: u8) -> String {
    format!("{byte:02x}").repeat(20)
}

fn added(byte: u8) -> FakeResponse {
    FakeResponse::success(json!({
        "torrent-added": { "id": 1, "name": "test", "hashString": hash(byte) }
    }))
}

fn wanted_reply(flags: &[bool]) -> FakeResponse {
    FakeResponse::success(json!({ "torrents": [{ "wanted": flags }] }))
}

fn exists() -> FakeResponse {
    FakeResponse::success(json!({ "torrents": [{ "id": 1 }] }))
}

fn rpc(method: &str, arguments: Value) -> Value {
    let mut body = serde_json::Map::new();
    body.insert("method".into(), json!(method));
    body.insert("arguments".into(), arguments);
    Value::Object(body)
}

async fn setup() -> (FakeServer, Transmission) {
    let fake = FakeServer::start().await.expect("bind fake");
    let client = Transmission::new(&fake.url()).expect("valid url");
    (fake, client)
}

fn id(byte: u8) -> ClientTorrentId {
    ClientTorrentId::new(hash(byte))
}

#[tokio::test]
async fn probe_reads_version() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(
        json!({ "version": "4.0.5 (synthetic)" }),
    ));
    let info = client.probe().await.expect("probe");
    assert_eq!(info.kind, ClientKind::Transmission);
    assert_eq!(info.version, "4.0.5 (synthetic)");
    assert_eq!(
        fake.bodies(),
        vec![rpc("session-get", json!({ "fields": ["version"] }))]
    );
}

#[tokio::test]
async fn handshake_retries_once_with_session_id() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::session_conflict("sid-1"));
    fake.push(FakeResponse::success(json!({ "version": "4" })));
    client.probe().await.expect("probe");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 2);
    assert_eq!(reqs[0].header("x-transmission-session-id"), None);
    assert_eq!(reqs[1].header("x-transmission-session-id"), Some("sid-1"));
    assert_eq!(reqs[0].body, reqs[1].body);
}

#[tokio::test]
async fn session_id_is_reused() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::session_conflict("sid-1"));
    fake.push(FakeResponse::success(json!({ "version": "4" })));
    fake.push(FakeResponse::success(json!({})));
    client.probe().await.expect("probe");
    client.set_rate_limits(None, None).await.expect("limits");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[2].header("x-transmission-session-id"), Some("sid-1"));
}

#[tokio::test]
async fn second_conflict_is_a_protocol_error() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::session_conflict("a"));
    fake.push(FakeResponse::session_conflict("b"));
    let err = client.probe().await.expect_err("rejected");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test]
async fn conflict_without_header_is_a_protocol_error() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::new(409));
    let err = client.probe().await.expect_err("rejected");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn unauthorised_maps_to_auth() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::new(401));
    let err = client.probe().await.expect_err("rejected");
    assert!(matches!(err, ClientError::Auth), "{err:?}");
}

#[tokio::test]
async fn unexpected_status_maps_to_protocol() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::new(500));
    let err = client.probe().await.expect_err("rejected");
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m.contains("500")),
        "{err:?}"
    );
}

#[tokio::test]
async fn failure_result_maps_to_protocol() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::failure("invalid or corrupt torrent file"));
    let err = client.probe().await.expect_err("rejected");
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m == "invalid or corrupt torrent file"),
        "{err:?}"
    );
}

#[tokio::test]
async fn connection_refused_maps_to_unreachable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr");
    drop(listener);
    let client = Transmission::new(&format!("http://{addr}/transmission/rpc")).expect("url");
    let err = client.probe().await.expect_err("refused");
    assert!(matches!(err, ClientError::Unreachable(_)), "{err:?}");
}

#[tokio::test]
async fn credentials_are_sent_as_basic_auth() {
    let fake = FakeServer::start().await.expect("bind");
    let client = Transmission::new(&fake.url())
        .expect("url")
        .with_credentials("user", "pass");
    fake.push(FakeResponse::success(json!({ "version": "4" })));
    client.probe().await.expect("probe");
    assert_eq!(
        fake.requests()[0].header("authorization"),
        Some("Basic dXNlcjpwYXNz")
    );
}

#[tokio::test]
async fn add_deselects_unwanted_files_and_makes_none_unlimited() {
    let (fake, client) = setup().await;
    let meta = synthetic_metainfo(3);
    fake.push(added(0xa1));
    fake.push(FakeResponse::success(json!({})));
    let got = client
        .add(
            TorrentSource::Metainfo(meta.clone()),
            Path::new("/staging/x"),
            &[1],
            SeedPolicy::None,
        )
        .await
        .expect("add");
    assert_eq!(got, id(0xa1));
    assert_eq!(
        fake.bodies(),
        vec![
            rpc(
                "torrent-add",
                json!({
                    "download-dir": "/staging/x",
                    "paused": true,
                    "metainfo": BASE64.encode(&meta),
                    "files-unwanted": [0, 2],
                })
            ),
            rpc(
                "torrent-set",
                json!({ "ids": [hash(0xa1)], "seedRatioMode": 2 })
            ),
        ]
    );
}

#[tokio::test]
async fn add_with_client_policy_and_all_wanted_sends_one_call() {
    let (fake, client) = setup().await;
    let meta = synthetic_metainfo(0);
    fake.push(added(0xa2));
    client
        .add(
            TorrentSource::Metainfo(meta.clone()),
            Path::new("/staging/y"),
            &[0],
            SeedPolicy::Client,
        )
        .await
        .expect("add");
    assert_eq!(
        fake.bodies(),
        vec![rpc(
            "torrent-add",
            json!({ "download-dir": "/staging/y", "paused": true, "metainfo": BASE64.encode(&meta) })
        )]
    );
}

#[tokio::test]
async fn add_large_torrent_uses_shorthand_and_verifies() {
    let (fake, client) = setup().await;
    let meta = synthetic_metainfo(2001);
    let mut flags = vec![false; 2001];
    flags[5] = true;
    flags[2000] = true;
    fake.push(added(0xa3));
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    fake.push(wanted_reply(&flags));
    fake.push(FakeResponse::success(json!({})));
    client
        .add(
            TorrentSource::Metainfo(meta.clone()),
            Path::new("/staging/z"),
            &[2000, 5],
            SeedPolicy::Ratio { ratio: 1.5 },
        )
        .await
        .expect("add");
    let h = hash(0xa3);
    assert_eq!(
        fake.bodies(),
        vec![
            rpc(
                "torrent-add",
                json!({ "download-dir": "/staging/z", "paused": true, "metainfo": BASE64.encode(&meta) })
            ),
            rpc("torrent-set", json!({ "ids": [h], "files-unwanted": [] })),
            rpc(
                "torrent-set",
                json!({ "ids": [h], "files-wanted": [5, 2000] })
            ),
            rpc("torrent-get", json!({ "ids": [h], "fields": ["wanted"] })),
            rpc(
                "torrent-set",
                json!({ "ids": [h], "seedRatioMode": 1, "seedRatioLimit": 1.5 })
            ),
        ]
    );
}

#[tokio::test]
async fn add_large_torrent_fails_when_selection_did_not_apply() {
    let (fake, client) = setup().await;
    fake.push(added(0xa4));
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    fake.push(wanted_reply(&[true; 2001]));
    let err = client
        .add(
            TorrentSource::Metainfo(synthetic_metainfo(2001)),
            Path::new("/staging/z"),
            &[0],
            SeedPolicy::Client,
        )
        .await
        .expect_err("not applied");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
}

fn duplicate(byte: u8) -> FakeResponse {
    FakeResponse::success(json!({
        "torrent-duplicate": { "id": 7, "name": "test", "hashString": hash(byte).to_uppercase() }
    }))
}

#[tokio::test]
async fn add_duplicate_reapplies_selection_and_seed() {
    let (fake, client) = setup().await;
    let meta = synthetic_metainfo(3);
    fake.push(duplicate(0xa5));
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    let got = client
        .add(
            TorrentSource::Metainfo(meta.clone()),
            Path::new("/staging/d"),
            &[0],
            SeedPolicy::Ratio { ratio: 2.0 },
        )
        .await
        .expect("add");
    assert_eq!(got, id(0xa5));
    let h = hash(0xa5);
    assert_eq!(
        fake.bodies(),
        vec![
            rpc(
                "torrent-add",
                json!({
                    "download-dir": "/staging/d",
                    "paused": true,
                    "metainfo": BASE64.encode(&meta),
                    "files-unwanted": [1, 2],
                })
            ),
            rpc(
                "torrent-set",
                json!({ "ids": [h], "files-wanted": [0], "files-unwanted": [1, 2] })
            ),
            rpc(
                "torrent-set",
                json!({ "ids": [h], "seedRatioMode": 1, "seedRatioLimit": 2.0 })
            ),
        ]
    );
}

#[tokio::test]
async fn add_duplicate_magnet_reads_count_and_restores_client_policy() {
    let (fake, client) = setup().await;
    let magnet = format!("magnet:?xt=urn:btih:{}", hash(0xa8));
    fake.push(duplicate(0xa8));
    fake.push(wanted_reply(&[true, true]));
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    client
        .add(
            TorrentSource::Magnet(magnet),
            Path::new("/staging/m"),
            &[1],
            SeedPolicy::Client,
        )
        .await
        .expect("add");
    let h = hash(0xa8);
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 4);
    assert_eq!(
        bodies[1],
        rpc("torrent-get", json!({ "ids": [h], "fields": ["wanted"] }))
    );
    assert_eq!(
        bodies[2],
        rpc(
            "torrent-set",
            json!({ "ids": [h], "files-wanted": [1], "files-unwanted": [0] })
        )
    );
    assert_eq!(
        bodies[3],
        rpc("torrent-set", json!({ "ids": [h], "seedRatioMode": 0 }))
    );
}

#[tokio::test]
async fn set_seed_policy_none_checks_existence_then_seeds_unlimited() {
    let (fake, client) = setup().await;
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    client
        .set_seed_policy(&id(0xe1), SeedPolicy::None)
        .await
        .expect("seed");
    let h = hash(0xe1);
    assert_eq!(
        fake.bodies(),
        vec![
            rpc("torrent-get", json!({ "ids": [h], "fields": ["id"] })),
            rpc("torrent-set", json!({ "ids": [h], "seedRatioMode": 2 })),
        ]
    );
}

#[tokio::test]
async fn set_seed_policy_client_restores_session_default() {
    let (fake, client) = setup().await;
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    client
        .set_seed_policy(&id(0xe2), SeedPolicy::Client)
        .await
        .expect("seed");
    assert_eq!(
        fake.bodies()[1],
        rpc(
            "torrent-set",
            json!({ "ids": [hash(0xe2)], "seedRatioMode": 0 })
        )
    );
}

#[tokio::test]
async fn set_seed_policy_unknown_torrent_is_not_found() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(json!({ "torrents": [] })));
    let err = client
        .set_seed_policy(&id(0xe3), SeedPolicy::Client)
        .await
        .expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn add_rejects_out_of_range_index_before_calling() {
    let (fake, client) = setup().await;
    let err = client
        .add(
            TorrentSource::Metainfo(synthetic_metainfo(2)),
            Path::new("/staging/d"),
            &[2],
            SeedPolicy::None,
        )
        .await
        .expect_err("out of range");
    assert!(
        matches!(
            err,
            ClientError::FileIndex {
                index: 2,
                file_count: 2
            }
        ),
        "{err:?}"
    );
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn add_magnet_without_metadata_skips_selection() {
    let (fake, client) = setup().await;
    let magnet = format!("magnet:?xt=urn:btih:{}", hash(0xa6));
    fake.push(added(0xa6));
    fake.push(wanted_reply(&[]));
    client
        .add(
            TorrentSource::Magnet(magnet.clone()),
            Path::new("/staging/m"),
            &[0],
            SeedPolicy::Client,
        )
        .await
        .expect("add");
    let h = hash(0xa6);
    assert_eq!(
        fake.bodies(),
        vec![
            rpc(
                "torrent-add",
                json!({ "download-dir": "/staging/m", "paused": true, "filename": magnet })
            ),
            rpc("torrent-get", json!({ "ids": [h], "fields": ["wanted"] })),
        ]
    );
}

#[tokio::test]
async fn add_magnet_with_metadata_applies_selection() {
    let (fake, client) = setup().await;
    let magnet = format!("magnet:?xt=urn:btih:{}", hash(0xa7));
    fake.push(added(0xa7));
    fake.push(wanted_reply(&[true, true, true]));
    fake.push(FakeResponse::success(json!({})));
    client
        .add(
            TorrentSource::Magnet(magnet),
            Path::new("/staging/m"),
            &[2],
            SeedPolicy::Client,
        )
        .await
        .expect("add");
    let bodies = fake.bodies();
    assert_eq!(bodies.len(), 3);
    assert_eq!(
        bodies[2],
        rpc(
            "torrent-set",
            json!({ "ids": [hash(0xa7)], "files-wanted": [2], "files-unwanted": [0, 1] })
        )
    );
}

#[tokio::test]
async fn set_wanted_replaces_selection() {
    let (fake, client) = setup().await;
    fake.push(wanted_reply(&[false, true, true]));
    fake.push(FakeResponse::success(json!({})));
    client.set_wanted(&id(0xb1), &[0]).await.expect("set");
    let h = hash(0xb1);
    assert_eq!(
        fake.bodies(),
        vec![
            rpc("torrent-get", json!({ "ids": [h], "fields": ["wanted"] })),
            rpc(
                "torrent-set",
                json!({ "ids": [h], "files-wanted": [0], "files-unwanted": [1, 2] })
            ),
        ]
    );
}

#[tokio::test]
async fn set_wanted_empty_never_sends_an_empty_list() {
    let (fake, client) = setup().await;
    fake.push(wanted_reply(&[true, true]));
    fake.push(FakeResponse::success(json!({})));
    client.set_wanted(&id(0xb2), &[]).await.expect("set");
    assert_eq!(
        fake.bodies()[1],
        rpc(
            "torrent-set",
            json!({ "ids": [hash(0xb2)], "files-unwanted": [0, 1] })
        )
    );
}

#[tokio::test]
async fn set_wanted_without_metadata_is_pending() {
    let (fake, client) = setup().await;
    fake.push(wanted_reply(&[]));
    let err = client
        .set_wanted(&id(0xb3), &[0])
        .await
        .expect_err("pending");
    assert!(matches!(err, ClientError::MetadataPending), "{err:?}");
}

#[tokio::test]
async fn set_wanted_unknown_torrent_is_not_found() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(json!({ "torrents": [] })));
    let err = client
        .set_wanted(&id(0xb4), &[0])
        .await
        .expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
}

#[tokio::test]
async fn start_checks_existence_then_starts() {
    let (fake, client) = setup().await;
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    client.start(&id(0xc1)).await.expect("start");
    let h = hash(0xc1);
    assert_eq!(
        fake.bodies(),
        vec![
            rpc("torrent-get", json!({ "ids": [h], "fields": ["id"] })),
            rpc("torrent-start", json!({ "ids": [h] })),
        ]
    );
}

#[tokio::test]
async fn stop_checks_existence_then_stops() {
    let (fake, client) = setup().await;
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    client.stop(&id(0xc2)).await.expect("stop");
    assert_eq!(
        fake.bodies()[1],
        rpc("torrent-stop", json!({ "ids": [hash(0xc2)] }))
    );
}

#[tokio::test]
async fn start_unknown_torrent_is_not_found() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(json!({ "torrents": [] })));
    let err = client.start(&id(0xc3)).await.expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn remove_passes_delete_flag() {
    let (fake, client) = setup().await;
    fake.push(exists());
    fake.push(FakeResponse::success(json!({})));
    client.remove(&id(0xc4), true).await.expect("remove");
    assert_eq!(
        fake.bodies()[1],
        rpc(
            "torrent-remove",
            json!({ "ids": [hash(0xc4)], "delete-local-data": true })
        )
    );
}

fn status_torrent(status: i64, error: i64) -> Value {
    json!({
        "id": 1,
        "hashString": hash(0xd1),
        "status": status,
        "leftUntilDone": 50,
        "error": error,
        "errorString": if error == 0 { "" } else { "No space left" },
        "fileStats": [
            { "bytesCompleted": 100, "wanted": true, "priority": 0 },
            { "bytesCompleted": 0, "wanted": 0, "priority": 0 },
        ],
        "rateDownload": 2048,
        "rateUpload": 512,
        "uploadRatio": 0.25,
        "isFinished": false,
    })
}

#[tokio::test]
async fn status_reads_per_file_progress() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(
        json!({ "torrents": [status_torrent(4, 0)] }),
    ));
    let st = client.status(&id(0xd1)).await.expect("status");
    assert_eq!(
        fake.bodies(),
        vec![rpc(
            "torrent-get",
            json!({ "ids": [hash(0xd1)], "fields": STATUS_FIELDS })
        )]
    );
    assert_eq!(st.infohash, InfoHash::from_bytes([0xd1; 20]));
    assert_eq!(st.state, TorrentState::Downloading);
    assert_eq!(
        st.files,
        vec![
            FileProgress {
                index: 0,
                bytes_done: 100,
                size: None,
                wanted: true
            },
            FileProgress {
                index: 1,
                bytes_done: 0,
                size: None,
                wanted: false
            },
        ]
    );
    assert!((st.ratio - 0.25).abs() < f32::EPSILON);
    assert_eq!(
        (st.down_rate, st.up_rate, st.is_finished),
        (2048, 512, false)
    );
    assert!(st.file_done(0, 100));
    assert!(!st.file_done(1, 50));
}

#[test]
fn a_finished_selection_reports_its_sizes() {
    let mut t = status_torrent(6, 0);
    t["leftUntilDone"] = json!(0);
    let st = convert(t).expect("valid");
    assert_eq!(st.files[0].size, Some(100));
    assert!(st.files[0].is_complete());
    assert_eq!(st.files[1].size, None);
}

#[tokio::test]
async fn status_unknown_torrent_is_not_found() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(json!({ "torrents": [] })));
    let err = client.status(&id(0xd2)).await.expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
}

fn convert(torrent: Value) -> Result<TorrentStatus> {
    serde_json::from_value::<RawTorrent>(torrent)
        .map_err(protocol)
        .and_then(RawTorrent::into_status)
}

#[test]
fn status_codes_map_to_states() {
    let table = [
        (0, TorrentState::Stopped),
        (1, TorrentState::Checking),
        (2, TorrentState::Checking),
        (3, TorrentState::Queued),
        (4, TorrentState::Downloading),
        (5, TorrentState::Queued),
        (6, TorrentState::Seeding),
    ];
    for (code, want) in table {
        assert_eq!(convert(status_torrent(code, 0)).expect("valid").state, want);
    }
    assert!(convert(status_torrent(9, 0)).is_err());
}

#[test]
fn only_local_errors_become_error_state() {
    let st = convert(status_torrent(0, 3)).expect("valid");
    assert_eq!(st.state, TorrentState::Error("No space left".into()));
    let st = convert(status_torrent(4, 2)).expect("valid");
    assert_eq!(st.state, TorrentState::Downloading);
}

#[test]
fn special_ratios_are_mapped() {
    let mut t = status_torrent(6, 0);
    t["uploadRatio"] = json!(-2);
    assert!(convert(t.clone()).expect("valid").ratio.is_infinite());
    t["uploadRatio"] = json!(-1);
    assert!(convert(t).expect("valid").ratio.abs() < f32::EPSILON);
}

#[test]
fn bad_hashes_are_rejected() {
    let mut t = status_torrent(4, 0);
    t["hashString"] = json!("short");
    assert!(matches!(convert(t), Err(ClientError::Protocol(_))));
}

#[tokio::test]
async fn rate_limits_enable_or_lift_each_direction() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(json!({})));
    fake.push(FakeResponse::success(json!({})));
    client
        .set_rate_limits(Some(512), None)
        .await
        .expect("limits");
    client
        .set_rate_limits(Some(0), Some(64))
        .await
        .expect("limits");
    assert_eq!(
        fake.bodies(),
        vec![
            rpc(
                "session-set",
                json!({
                    "speed-limit-down": 512,
                    "speed-limit-down-enabled": true,
                    "speed-limit-up-enabled": false,
                })
            ),
            rpc(
                "session-set",
                json!({
                    "speed-limit-down-enabled": false,
                    "speed-limit-up": 64,
                    "speed-limit-up-enabled": true,
                })
            ),
        ]
    );
}

#[tokio::test]
async fn concurrent_calls_are_serialised() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::session_conflict("sid-1"));
    fake.push(FakeResponse::success(json!({ "version": "4" })));
    fake.push(FakeResponse::success(json!({ "version": "4" })));
    let (a, b) = tokio::join!(client.probe(), client.probe());
    a.expect("first");
    b.expect("second");
    let reqs = fake.requests();
    assert_eq!(reqs.len(), 3);
    assert_eq!(reqs[2].header("x-transmission-session-id"), Some("sid-1"));
}

#[test]
fn complement_and_index_checks() {
    let wanted: BTreeSet<u32> = [1, 3].into_iter().collect();
    assert_eq!(complement(&wanted, 5), vec![0, 2, 4]);
    assert!(check_indices(&wanted, 4).is_ok());
    assert!(check_indices(&wanted, 3).is_err());
    assert!(check_indices(&BTreeSet::new(), 0).is_ok());
}

#[test]
fn debug_output_hides_credentials() {
    let t = Transmission::new(Transmission::DEFAULT_URL)
        .expect("url")
        .with_credentials("user", "pass");
    let shown = format!("{t:?}");
    assert!(shown.contains("<redacted>"));
    assert!(!shown.contains("dXNlcjpwYXNz"));
}

#[tokio::test]
async fn files_strip_the_torrent_name_and_wait_for_metadata() {
    let (fake, client) = setup().await;
    fake.push(FakeResponse::success(
        json!({ "torrents": [{ "name": "Set", "files": [] }] }),
    ));
    fake.push(FakeResponse::success(json!({ "torrents": [{
        "name": "Set",
        "files": [
            { "name": "Set/Sub/a.bin", "length": 4, "bytesCompleted": 0 },
            { "name": "Set/b.bin", "length": 8, "bytesCompleted": 0 }
        ]
    }] })));
    fake.push(FakeResponse::success(json!({ "torrents": [{
        "name": "one.bin",
        "files": [{ "name": "one.bin", "length": 2, "bytesCompleted": 0 }]
    }] })));
    fake.push(FakeResponse::success(json!({ "torrents": [] })));
    assert!(matches!(
        client.files(&id(1)).await,
        Err(ClientError::MetadataPending)
    ));
    let files = client.files(&id(1)).await.expect("files");
    let listed: Vec<(u32, &str, u64)> = files
        .iter()
        .map(|f| (f.index, f.path.as_str(), f.size))
        .collect();
    assert_eq!(listed, vec![(0, "Sub/a.bin", 4), (1, "b.bin", 8)]);
    let single = client.files(&id(2)).await.expect("files");
    assert_eq!(single[0].path, "one.bin");
    assert!(matches!(
        client.files(&id(3)).await,
        Err(ClientError::NotFound)
    ));
    assert_eq!(
        fake.bodies()[0],
        rpc(
            "torrent-get",
            json!({ "ids": [hash(1)], "fields": ["name", "files"] })
        )
    );
}

#[test]
fn replies_parse_into_types_and_report_failures() {
    let ok: Torrents<WantedOnly> =
        parse_reply(br#"{"result":"success","arguments":{"torrents":[{"wanted":[1,true]}]}}"#)
            .expect("parse");
    let flags: Vec<bool> = ok
        .torrents
        .expect("torrents")
        .remove(0)
        .wanted
        .into_iter()
        .map(Flag::into_bool)
        .collect();
    assert_eq!(flags, [true, true]);
    let missing: Torrents<Value> = parse_reply(br#"{"result":"success"}"#).expect("parse");
    assert!(missing.torrents.is_none());
    let failed = parse_reply::<Torrents<RawTorrent>>(
        br#"{"result":"no such method","arguments":{"torrents":7}}"#,
    );
    assert!(matches!(failed, Err(ClientError::Protocol(m)) if m == "no such method"));
    let garbled = parse_reply::<Torrents<RawTorrent>>(br#"{"result":"success","arguments":[]}"#);
    assert!(matches!(garbled, Err(ClientError::Protocol(_))));
}

#[tokio::test]
async fn status_of_a_large_torrent_reads_every_file() {
    let (fake, client) = setup().await;
    let n = 20_000u32;
    let stats: Vec<Value> = (0..n)
        .map(|i| json!({ "bytesCompleted": i % 5, "wanted": i % 2 == 0, "priority": 0 }))
        .collect();
    fake.push(FakeResponse::success(json!({ "torrents": [{
        "id": 1, "hashString": hash(4), "status": 4, "leftUntilDone": 9, "error": 0,
        "errorString": "", "fileStats": stats, "rateDownload": 0,
        "rateUpload": 0, "uploadRatio": 0.0, "isFinished": false
    }] })));
    let st = client.status(&id(4)).await.expect("status");
    assert_eq!(st.files.len(), 20_000);
    assert!(st
        .file(19_999)
        .is_some_and(|f| f.bytes_done == 4 && !f.wanted));
    assert!(st.file_done(4, 4) && !st.file_done(3, 4));
}
