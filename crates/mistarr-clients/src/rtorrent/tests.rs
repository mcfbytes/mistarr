use std::path::Path;
use std::time::Duration;

use super::*;
use crate::fake::{FakeScgiServer, ScgiReply};
use crate::metainfo::tests::synthetic_metainfo;
use crate::PathMapping;

type Call = (String, Vec<Value>);

/// A synthetic infohash of one repeated byte, lowercase as ids carry it.
fn hash(byte: u8) -> String {
    format!("{byte:02x}").repeat(20)
}

fn id(byte: u8) -> ClientTorrentId {
    ClientTorrentId::new(hash(byte))
}

fn t(byte: u8) -> String {
    hash(byte).to_ascii_uppercase()
}

fn v(s: &str) -> Value {
    Value::from(s)
}

fn ok() -> ScgiReply {
    ScgiReply::Value(Value::Int(0))
}

fn not_found() -> ScgiReply {
    ScgiReply::fault(-501, "Could not find info-hash.")
}

fn ints(values: &[i64]) -> ScgiReply {
    ScgiReply::multicall(values.iter().copied().map(Value::Int).collect())
}

fn call(method: &str, params: Vec<Value>) -> Call {
    (method.to_owned(), params)
}

fn multicall(entries: Vec<(&str, Vec<Value>)>) -> Call {
    let list = entries
        .into_iter()
        .map(|(m, p)| {
            Value::Struct(vec![
                ("methodName".into(), v(m)),
                ("params".into(), Value::Array(p)),
            ])
        })
        .collect();
    call("system.multicall", vec![Value::Array(list)])
}

fn priorities(target: &str, from: usize, flags: &[bool]) -> Call {
    multicall(
        flags
            .iter()
            .enumerate()
            .map(|(i, &on)| {
                let file = format!("{target}:f{}", from + i);
                ("f.priority.set", vec![v(&file), Value::Int(i64::from(on))])
            })
            .collect(),
    )
}

fn size_query(target: &str) -> Call {
    multicall(vec![
        ("d.is_meta", vec![v(target)]),
        ("d.size_files", vec![v(target)]),
    ])
}

async fn setup() -> (FakeScgiServer, Rtorrent) {
    let fake = FakeScgiServer::start().await.expect("bind fake");
    let client = Rtorrent::new(&fake.addr()).expect("valid addr");
    (fake, client)
}

/// Scripts the replies to adding an existing single-file torrent.
async fn add_existing(fake: &FakeScgiServer, client: &Rtorrent, byte: u8, seed: SeedPolicy) {
    fake.push(ScgiReply::Value(v(&t(byte))));
    fake.push(ints(&[0, 1]));
    fake.push(ints(&[0]));
    fake.push(ok());
    let magnet = format!("magnet:?xt=urn:btih:{}", hash(byte));
    let got = client
        .add(TorrentSource::Magnet(magnet), Path::new("/s"), &[0], seed)
        .await
        .expect("add");
    assert_eq!(got, id(byte));
}

#[derive(Clone)]
struct Snapshot {
    state: i64,
    active: i64,
    complete: i64,
    hashing: i64,
    queued: i64,
    ratio: i64,
    message: &'static str,
    meta: i64,
    files: Vec<[i64; 4]>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Self {
            state: 1,
            active: 1,
            complete: 0,
            hashing: 0,
            queued: 0,
            ratio: 0,
            message: "",
            meta: 0,
            files: vec![[100, 4, 4, 1]],
        }
    }
}

impl Snapshot {
    fn reply(&self) -> ScgiReply {
        let files = self
            .files
            .iter()
            .map(|row| Value::Array(row.iter().copied().map(Value::Int).collect()))
            .collect();
        ScgiReply::multicall(vec![
            Value::Int(self.state),
            Value::Int(self.active),
            Value::Int(self.complete),
            Value::Int(self.hashing),
            Value::Int(self.queued),
            Value::Int(self.ratio),
            Value::Int(2048),
            Value::Int(512),
            v(self.message),
            Value::Int(self.meta),
            Value::Array(files),
        ])
    }
}

fn status_call(target: &str) -> Call {
    let mut entries: Vec<(&str, Vec<Value>)> = STATUS_COMMANDS
        .iter()
        .map(|&c| (c, vec![v(target)]))
        .collect();
    entries.push((
        "f.multicall",
        [
            target,
            "",
            "f.size_bytes=",
            "f.completed_chunks=",
            "f.size_chunks=",
            "f.priority=",
        ]
        .into_iter()
        .map(v)
        .collect(),
    ));
    multicall(entries)
}

fn scratch_dir(name: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("mistarr-rt-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create scratch dir");
    dir
}

#[tokio::test]
async fn probe_reads_version_and_frames_headers() {
    let (fake, client) = setup().await;
    fake.push(ScgiReply::Value(v("0.9.8")));
    let info = client.probe().await.expect("probe");
    assert_eq!(info.kind, ClientKind::Rtorrent);
    assert_eq!(info.version, "0.9.8");
    assert_eq!(fake.calls(), vec![call("system.client_version", vec![])]);
    let req = &fake.requests()[0];
    assert_eq!(
        req.headers,
        vec![
            ("CONTENT_LENGTH".to_owned(), req.body.len().to_string()),
            ("SCGI".to_owned(), "1".to_owned()),
        ]
    );
}

#[tokio::test]
async fn add_loads_paused_then_prioritises_then_start() {
    let (fake, client) = setup().await;
    let meta = synthetic_metainfo(3);
    let h = metainfo::info_hash(&meta).expect("hash");
    let target = target(&h);
    for reply in [not_found(), ok(), ok(), ints(&[0, 0, 0]), ok(), ok()] {
        fake.push(reply);
    }
    let got = client
        .add(
            TorrentSource::Metainfo(meta.clone()),
            Path::new("/media/fat/mistarr/staging/x"),
            &[1],
            SeedPolicy::None,
        )
        .await
        .expect("add");
    assert_eq!(got.as_str(), h.to_string());
    client.start(&got).await.expect("start");
    assert_eq!(
        fake.calls(),
        vec![
            call("d.hash", vec![v(&target)]),
            call(
                "load.raw",
                vec![
                    v(""),
                    Value::Base64(meta),
                    v("d.directory.set=\"/media/fat/mistarr/staging/x\"")
                ]
            ),
            call(
                "d.directory.set",
                vec![v(&target), v("/media/fat/mistarr/staging/x")]
            ),
            priorities(&target, 0, &[false, true, false]),
            call("d.update_priorities", vec![v(&target)]),
            call("d.start", vec![v(&target)]),
        ]
    );
}

#[tokio::test]
async fn add_magnet_loads_normal_and_defers_selection() {
    let (fake, client) = setup().await;
    let magnet = format!("magnet:?xt=urn:btih:{}&dn=test", hash(0xab));
    for reply in [not_found(), ok(), ok(), ints(&[1, 1])] {
        fake.push(reply);
    }
    let got = client
        .add(
            TorrentSource::Magnet(magnet.clone()),
            Path::new("/s/x"),
            &[4],
            SeedPolicy::Client,
        )
        .await
        .expect("add");
    assert_eq!(got, id(0xab));
    assert_eq!(
        fake.calls(),
        vec![
            call("d.hash", vec![v(&t(0xab))]),
            call(
                "load.normal",
                vec![v(""), v(&magnet), v("d.directory.set=\"/s/x\"")]
            ),
            call("d.directory.set", vec![v(&t(0xab)), v("/s/x")]),
            size_query(&t(0xab)),
        ]
    );
}

#[tokio::test]
async fn add_existing_torrent_reapplies_selection_without_loading() {
    let (fake, client) = setup().await;
    fake.push(ScgiReply::Value(v(&t(1))));
    fake.push(ints(&[0, 2]));
    fake.push(ints(&[0, 0]));
    fake.push(ok());
    let magnet = format!("magnet:?xt=urn:btih:{}", hash(1));
    client
        .add(
            TorrentSource::Magnet(magnet),
            Path::new("/s"),
            &[0],
            SeedPolicy::None,
        )
        .await
        .expect("add");
    assert_eq!(
        fake.calls(),
        vec![
            call("d.hash", vec![v(&t(1))]),
            size_query(&t(1)),
            priorities(&t(1), 0, &[true, false]),
            call("d.update_priorities", vec![v(&t(1))]),
        ]
    );
}

#[tokio::test]
async fn add_rejected_by_rtorrent_is_a_protocol_error() {
    let (fake, client) = setup().await;
    for reply in [not_found(), ok(), not_found()] {
        fake.push(reply);
    }
    let err = client
        .add(
            TorrentSource::Metainfo(synthetic_metainfo(1)),
            Path::new("/s"),
            &[],
            SeedPolicy::None,
        )
        .await
        .expect_err("not loaded");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn add_checks_indices_and_sources_before_any_call() {
    let (fake, client) = setup().await;
    let err = client
        .add(
            TorrentSource::Metainfo(synthetic_metainfo(3)),
            Path::new("/s"),
            &[3],
            SeedPolicy::None,
        )
        .await
        .expect_err("out of range");
    assert!(
        matches!(
            err,
            ClientError::FileIndex {
                index: 3,
                file_count: 3
            }
        ),
        "{err:?}"
    );
    for src in [
        TorrentSource::Metainfo(b"not bencode".to_vec()),
        TorrentSource::Magnet("magnet:?dn=nohash".into()),
    ] {
        let err = client
            .add(src, Path::new("/s"), &[], SeedPolicy::None)
            .await
            .expect_err("rejected");
        assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    }
    assert!(fake.requests().is_empty());
}

#[tokio::test]
async fn priorities_are_chunked_at_500() {
    let (fake, client) = setup().await;
    let meta = synthetic_metainfo(1201);
    let target = target(&metainfo::info_hash(&meta).expect("hash"));
    for reply in [not_found(), ok(), ok()] {
        fake.push(reply);
    }
    for n in [500, 500, 201] {
        fake.push(ints(&vec![0; n]));
    }
    fake.push(ok());
    client
        .add(
            TorrentSource::Metainfo(meta),
            Path::new("/s"),
            &[0, 1200],
            SeedPolicy::None,
        )
        .await
        .expect("add");
    let calls = fake.calls();
    let methods: Vec<&str> = calls.iter().map(|(m, _)| m.as_str()).collect();
    assert_eq!(
        methods,
        [
            "d.hash",
            "load.raw",
            "d.directory.set",
            "system.multicall",
            "system.multicall",
            "system.multicall",
            "d.update_priorities"
        ]
    );
    let mut flags = vec![false; 1201];
    flags[0] = true;
    flags[1200] = true;
    assert_eq!(calls[3], priorities(&target, 0, &flags[..500]));
    assert_eq!(calls[4], priorities(&target, 500, &flags[500..1000]));
    assert_eq!(calls[5], priorities(&target, 1000, &flags[1000..]));
}

#[tokio::test]
async fn set_wanted_sets_every_priority_then_updates() {
    let (fake, client) = setup().await;
    fake.push(ints(&[0, 3]));
    fake.push(ints(&[0, 0, 0]));
    fake.push(ok());
    client.set_wanted(&id(2), &[0, 2]).await.expect("set");
    assert_eq!(
        fake.calls(),
        vec![
            size_query(&t(2)),
            priorities(&t(2), 0, &[true, false, true]),
            call("d.update_priorities", vec![v(&t(2))]),
        ]
    );
}

#[tokio::test]
async fn set_wanted_errors() {
    let (fake, client) = setup().await;
    fake.push(ints(&[1, 1]));
    let err = client.set_wanted(&id(2), &[0]).await.expect_err("pending");
    assert!(matches!(err, ClientError::MetadataPending), "{err:?}");
    fake.push(ints(&[0, 2]));
    let err = client.set_wanted(&id(2), &[2]).await.expect_err("range");
    assert!(matches!(err, ClientError::FileIndex { .. }), "{err:?}");
    assert_eq!(fake.requests().len(), 2);
}

#[tokio::test]
async fn start_and_stop_send_one_command() {
    let (fake, client) = setup().await;
    fake.push(ok());
    fake.push(ok());
    client.start(&id(3)).await.expect("start");
    client.stop(&id(3)).await.expect("stop");
    assert_eq!(
        fake.calls(),
        vec![
            call("d.start", vec![v(&t(3))]),
            call("d.stop", vec![v(&t(3))]),
        ]
    );
}

#[tokio::test]
async fn unknown_torrent_is_not_found() {
    let (fake, client) = setup().await;
    fake.push(not_found());
    let err = client.stop(&id(4)).await.expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
    let bad = ClientTorrentId::new("not-a-hash");
    let err = client.start(&bad).await.expect_err("malformed");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
    assert_eq!(fake.requests().len(), 1);
}

#[tokio::test]
async fn status_reports_per_file_progress() {
    let (fake, client) = setup().await;
    let snap = Snapshot {
        ratio: 250,
        files: vec![[100, 4, 4, 1], [100, 1, 4, 2], [50, 0, 2, 0], [0, 0, 0, 1]],
        ..Snapshot::default()
    };
    fake.push(snap.reply());
    let st = client.status(&id(5)).await.expect("status");
    assert_eq!(fake.calls(), vec![status_call(&t(5))]);
    assert_eq!(st.infohash, InfoHash::from_bytes([5; 20]));
    assert_eq!(st.state, TorrentState::Downloading);
    assert!((st.ratio - 0.25).abs() < f32::EPSILON);
    assert_eq!((st.down_rate, st.up_rate), (2048, 512));
    assert!(!st.is_finished);
    let files: Vec<(u32, u64, u64, bool)> = st
        .files
        .iter()
        .map(|f| (f.index, f.bytes_done, f.size, f.wanted))
        .collect();
    assert_eq!(
        files,
        vec![
            (0, 100, 100, true),
            (1, 25, 100, true),
            (2, 0, 50, false),
            (3, 0, 0, true)
        ]
    );
    assert!(st.file_done(0) && !st.file_done(1));
}

#[tokio::test]
async fn status_maps_states() {
    let (fake, client) = setup().await;
    let cases = [
        (
            Snapshot {
                hashing: 1,
                ..Snapshot::default()
            },
            TorrentState::Checking,
        ),
        (
            Snapshot {
                active: 0,
                queued: 1,
                message: "Queued for hashing",
                ..Snapshot::default()
            },
            TorrentState::Checking,
        ),
        (
            Snapshot {
                state: 0,
                active: 0,
                ..Snapshot::default()
            },
            TorrentState::Stopped,
        ),
        (
            Snapshot {
                active: 0,
                message: "Storage error: disk full",
                ..Snapshot::default()
            },
            TorrentState::Error("Storage error: disk full".into()),
        ),
        (
            Snapshot {
                message: "Tracker: [Timeout was reached]",
                files: vec![[10, 0, 1, 1]],
                ..Snapshot::default()
            },
            TorrentState::Downloading,
        ),
        (
            Snapshot {
                files: vec![[10, 1, 1, 1], [10, 0, 1, 0]],
                ..Snapshot::default()
            },
            TorrentState::Seeding,
        ),
        (
            Snapshot {
                complete: 1,
                meta: 1,
                ..Snapshot::default()
            },
            TorrentState::Seeding,
        ),
    ];
    for (snap, want) in cases {
        fake.push(snap.reply());
        let st = client.status(&id(6)).await.expect("status");
        assert_eq!(st.state, want);
        if snap.meta == 1 {
            assert!(st.files.is_empty());
        }
    }
}

#[tokio::test]
async fn status_fault_in_multicall_entry_is_not_found() {
    let (fake, client) = setup().await;
    let fault = Fault {
        code: -501,
        message: "Could not find info-hash.".into(),
    }
    .to_value();
    fake.push(ScgiReply::Value(Value::Array(vec![fault; 11])));
    let err = client.status(&id(7)).await.expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
}

#[tokio::test]
async fn ratio_policy_stops_torrent_on_poll() {
    let (fake, client) = setup().await;
    add_existing(&fake, &client, 8, SeedPolicy::Ratio { ratio: 1.0 }).await;
    let seeding = |ratio| Snapshot {
        ratio,
        ..Snapshot::default()
    };
    fake.push(seeding(999).reply());
    let st = client.status(&id(8)).await.expect("status");
    assert_eq!(st.state, TorrentState::Seeding);
    assert!(!st.is_finished);

    fake.push(seeding(1000).reply());
    fake.push(ok());
    let st = client.status(&id(8)).await.expect("status");
    assert_eq!(st.state, TorrentState::Stopped);
    assert!(st.is_finished);

    fake.push(
        Snapshot {
            active: 0,
            ratio: 1000,
            ..Snapshot::default()
        }
        .reply(),
    );
    let st = client.status(&id(8)).await.expect("status");
    assert!(st.is_finished);

    let calls = fake.calls();
    let tail: Vec<Call> = calls[4..].to_vec();
    assert_eq!(
        tail,
        vec![
            status_call(&t(8)),
            status_call(&t(8)),
            call("d.stop", vec![v(&t(8))]),
            status_call(&t(8)),
        ]
    );

    fake.push(ok());
    client.start(&id(8)).await.expect("start");
    fake.push(seeding(1500).reply());
    fake.push(ok());
    let st = client.status(&id(8)).await.expect("status");
    assert!(st.is_finished);
    assert_eq!(
        fake.methods().last().map(String::as_str),
        Some("d.stop"),
        "a restarted torrent past its ratio is stopped again"
    );
}

#[tokio::test]
async fn finished_is_derived_from_state_and_current_policy() {
    let (fake, client) = setup().await;
    add_existing(&fake, &client, 19, SeedPolicy::Ratio { ratio: 1.0 }).await;
    let stopped = Snapshot {
        state: 0,
        active: 0,
        ratio: 1000,
        ..Snapshot::default()
    };
    fake.push(stopped.reply());
    assert!(client.status(&id(19)).await.expect("status").is_finished);

    fake.push(ScgiReply::Value(v(&t(19))));
    client
        .set_seed_policy(&id(19), SeedPolicy::Ratio { ratio: 2.0 })
        .await
        .expect("raise");
    fake.push(stopped.reply());
    let st = client.status(&id(19)).await.expect("status");
    assert_eq!(st.state, TorrentState::Stopped);
    assert!(!st.is_finished, "a raised policy is no longer met");

    fake.push(
        Snapshot {
            ratio: 1000,
            ..Snapshot::default()
        }
        .reply(),
    );
    let st = client.status(&id(19)).await.expect("status");
    assert_eq!(st.state, TorrentState::Seeding, "restarted outside mistarr");
    assert!(!st.is_finished);

    fake.push(
        Snapshot {
            state: 0,
            active: 0,
            ratio: 5000,
            files: vec![[100, 1, 4, 1]],
            ..Snapshot::default()
        }
        .reply(),
    );
    let st = client.status(&id(19)).await.expect("status");
    assert!(
        !st.is_finished,
        "a paused incomplete torrent is not finished"
    );
    assert!(!fake.methods().iter().any(|m| m == "d.stop"));
}

#[tokio::test]
async fn none_policy_stops_once_seeding_and_client_policy_never_does() {
    let (fake, client) = setup().await;
    add_existing(&fake, &client, 9, SeedPolicy::None).await;
    add_existing(&fake, &client, 10, SeedPolicy::Client).await;
    fake.push(Snapshot::default().reply());
    fake.push(ok());
    assert!(client.status(&id(9)).await.expect("status").is_finished);
    fake.push(
        Snapshot {
            ratio: 50_000,
            ..Snapshot::default()
        }
        .reply(),
    );
    let st = client.status(&id(10)).await.expect("status");
    assert_eq!(st.state, TorrentState::Seeding);
    let methods = fake.methods();
    assert_eq!(methods.iter().filter(|m| *m == "d.stop").count(), 1);
    assert_eq!(methods.last().map(String::as_str), Some("system.multicall"));
}

#[tokio::test]
async fn set_seed_policy_replaces_the_policy_evaluated_by_status() {
    let (fake, client) = setup().await;
    add_existing(&fake, &client, 11, SeedPolicy::Client).await;
    let seeding = Snapshot {
        ratio: 3000,
        ..Snapshot::default()
    };
    fake.push(seeding.reply());
    assert!(!client.status(&id(11)).await.expect("status").is_finished);

    fake.push(ScgiReply::Value(v(&t(11))));
    client
        .set_seed_policy(&id(11), SeedPolicy::Ratio { ratio: 2.0 })
        .await
        .expect("policy");
    fake.push(seeding.reply());
    fake.push(ok());
    assert!(client.status(&id(11)).await.expect("status").is_finished);
    let calls = fake.calls();
    assert_eq!(calls[5], call("d.hash", vec![v(&t(11))]));
    assert_eq!(calls[7], call("d.stop", vec![v(&t(11))]));

    fake.push(not_found());
    let err = client
        .set_seed_policy(&id(12), SeedPolicy::None)
        .await
        .expect_err("missing");
    assert!(matches!(err, ClientError::NotFound), "{err:?}");
}

#[tokio::test]
async fn set_seed_policy_adopts_an_untracked_torrent() {
    let (fake, client) = setup().await;
    fake.push(ScgiReply::Value(v(&t(13))));
    client
        .set_seed_policy(&id(13), SeedPolicy::None)
        .await
        .expect("policy");
    fake.push(Snapshot::default().reply());
    fake.push(ok());
    assert!(client.status(&id(13)).await.expect("status").is_finished);
}

#[tokio::test]
async fn remove_erases_without_touching_data() {
    let (fake, client) = setup().await;
    fake.push(ok());
    client.remove(&id(14), false).await.expect("remove");
    assert_eq!(fake.calls(), vec![call("d.erase", vec![v(&t(14))])]);
}

fn layout_reply(dir: &str, multi: i64, paths: &[&str]) -> ScgiReply {
    let rows = paths.iter().map(|p| Value::Array(vec![v(p)])).collect();
    ScgiReply::multicall(vec![v(dir), Value::Int(multi), Value::Array(rows)])
}

#[tokio::test]
async fn remove_deletes_data_through_the_path_map() {
    let local = scratch_dir("remove");
    let set = local.join("set");
    std::fs::create_dir_all(set.join("sub/deeper")).expect("mkdir");
    std::fs::write(set.join("a.bin"), b"a").expect("write");
    std::fs::write(set.join("sub/deeper/b.bin"), b"b").expect("write");
    std::fs::write(local.join("other.bin"), b"o").expect("write");

    let fake = FakeScgiServer::start().await.expect("bind");
    let client = Rtorrent::new(&fake.addr())
        .expect("addr")
        .with_path_map(RemotePathMap::new(vec![PathMapping::new(
            "/remote/dl",
            &local,
        )]));
    fake.push(layout_reply(
        "/remote/dl/set",
        1,
        &["a.bin", "sub/deeper/b.bin", "missing.bin"],
    ));
    fake.push(ok());
    client.remove(&id(15), true).await.expect("remove");
    assert_eq!(
        fake.calls(),
        vec![
            multicall(vec![
                ("d.directory", vec![v(&t(15))]),
                ("d.is_multi_file", vec![v(&t(15))]),
                ("f.multicall", vec![v(&t(15)), v(""), v("f.path=")]),
            ]),
            call("d.erase", vec![v(&t(15))]),
        ]
    );
    assert!(!set.exists());
    assert!(local.join("other.bin").exists());
    let _ = std::fs::remove_dir_all(&local);
}

#[tokio::test]
async fn remove_single_file_keeps_the_directory() {
    let local = scratch_dir("single");
    std::fs::write(local.join("one.bin"), b"1").expect("write");
    std::fs::write(local.join("other.bin"), b"o").expect("write");
    let (fake, client) = setup().await;
    fake.push(layout_reply(&local.display().to_string(), 0, &["one.bin"]));
    fake.push(ok());
    client.remove(&id(16), true).await.expect("remove");
    assert!(!local.join("one.bin").exists());
    assert!(local.join("other.bin").exists());
    let _ = std::fs::remove_dir_all(&local);
}

#[tokio::test]
async fn remove_deletes_what_it_can_then_erases_and_reports_the_failure() {
    let local = scratch_dir("partial");
    std::fs::create_dir_all(local.join("stuck.bin/inner")).expect("mkdir");
    std::fs::write(local.join("gone.bin"), b"g").expect("write");
    let (fake, client) = setup().await;
    let dir = local.display().to_string();
    fake.push(layout_reply(&dir, 0, &["stuck.bin", "gone.bin"]));
    fake.push(ok());
    let err = client.remove(&id(20), true).await.expect_err("partial");
    assert!(matches!(err, ClientError::Io(_)), "{err:?}");
    assert!(!local.join("gone.bin").exists());
    assert!(local.join("stuck.bin").exists());
    assert_eq!(
        fake.methods(),
        vec!["system.multicall".to_owned(), "d.erase".to_owned()]
    );
    let _ = std::fs::remove_dir_all(&local);
}

#[tokio::test]
async fn remove_refuses_unsafe_paths_before_erasing() {
    let (fake, client) = setup().await;
    fake.push(layout_reply("/srv/x", 1, &["../escape.bin"]));
    let err = client.remove(&id(17), true).await.expect_err("unsafe");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    fake.push(layout_reply("relative", 1, &["a.bin"]));
    let err = client.remove(&id(17), true).await.expect_err("relative");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    assert!(!fake.methods().iter().any(|m| m == "d.erase"));
}

#[tokio::test]
async fn rate_limits_use_global_throttles() {
    let (fake, client) = setup().await;
    for _ in 0..4 {
        fake.push(ok());
    }
    client
        .set_rate_limits(Some(100), None)
        .await
        .expect("limits");
    client
        .set_rate_limits(Some(0), Some(25))
        .await
        .expect("limits");
    let down = "throttle.global_down.max_rate.set_kb";
    let up = "throttle.global_up.max_rate.set_kb";
    assert_eq!(
        fake.calls(),
        vec![
            call(down, vec![v(""), Value::Int(100)]),
            call(up, vec![v(""), Value::Int(0)]),
            call(down, vec![v(""), Value::Int(0)]),
            call(up, vec![v(""), Value::Int(25)]),
        ]
    );
}

#[tokio::test]
async fn fault_and_garbage_map_to_protocol() {
    let (fake, client) = setup().await;
    fake.push(ScgiReply::fault(-506, "Method 'x' not defined"));
    let err = client.probe().await.expect_err("fault");
    assert!(
        matches!(err, ClientError::Protocol(ref m) if m.contains("-506")),
        "{err:?}"
    );
    fake.push(ScgiReply::Raw(b"Status: 200 OK\r\n\r\n<html/>".to_vec()));
    let err = client.probe().await.expect_err("garbage");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
    fake.push(ScgiReply::Value(Value::Int(1)));
    let err = client.probe().await.expect_err("not a string");
    assert!(matches!(err, ClientError::Protocol(_)), "{err:?}");
}

#[tokio::test]
async fn connection_refused_is_unreachable() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind");
    let addr = listener.local_addr().expect("addr").to_string();
    drop(listener);
    let client = Rtorrent::new(&addr).expect("addr");
    let err = client.probe().await.expect_err("refused");
    assert!(matches!(err, ClientError::Unreachable(_)), "{err:?}");
}

#[tokio::test]
async fn silence_times_out_as_unreachable() {
    let fake = FakeScgiServer::start().await.expect("bind");
    fake.push(ScgiReply::Silent);
    let client = Rtorrent::new(&fake.addr())
        .expect("addr")
        .with_timeout(Duration::from_millis(100));
    let err = client.probe().await.expect_err("timeout");
    assert!(
        matches!(err, ClientError::Unreachable(ref m) if m.contains("timed out")),
        "{err:?}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn unix_socket_transport() {
    let path = std::env::temp_dir().join(format!("mistarr-rt-{}.sock", std::process::id()));
    let fake = FakeScgiServer::start_unix(&path).expect("bind");
    fake.push(ScgiReply::Value(v("0.9.8")));
    fake.push(ok());
    let client = Rtorrent::new(&format!("scgi://{}", path.display())).expect("addr");
    assert_eq!(client.probe().await.expect("probe").version, "0.9.8");
    client.stop(&id(18)).await.expect("stop");
    assert_eq!(
        fake.methods(),
        vec!["system.client_version".to_owned(), "d.stop".to_owned()]
    );
}

#[test]
fn recommended_rc_is_the_documented_rc() {
    let doc = include_str!("../../../../docs/DOWNLOAD-CLIENTS.md");
    let rc = recommended_rc();
    assert!(
        doc.contains(&format!("```\n{rc}```")),
        "rc differs from docs"
    );
    assert!(rc.contains("network.xmlrpc.size_limit.set = "));
}

#[test]
fn magnet_hashes_parse() {
    let hex = format!(
        "magnet:?dn=x&xt=urn:btih:{}",
        hash(0xcd).to_ascii_uppercase()
    );
    assert_eq!(magnet_hash(&hex), Some(InfoHash::from_bytes([0xcd; 20])));
    let zeros = format!("magnet:?xt=urn:btih:{}", "a".repeat(32));
    assert_eq!(magnet_hash(&zeros), Some(InfoHash::from_bytes([0; 20])));
    let ones = format!("magnet:?xt=URN:BTIH:{}", "7".repeat(32));
    assert_eq!(magnet_hash(&ones), Some(InfoHash::from_bytes([0xff; 20])));
    let encoded = format!("magnet:?dn=a%20b&xt=urn%3Abtih%3A{}", hash(0x5e));
    assert_eq!(
        magnet_hash(&encoded),
        Some(InfoHash::from_bytes([0x5e; 20]))
    );
    let mixed = "magnet:?xt=urn:btih:AEAQCAIBAEAQCAIBAEAQCAIBAEAQCAIB";
    assert_eq!(magnet_hash(mixed), Some(InfoHash::from_bytes([1; 20])));
    for bad in [
        "magnet:?xt=urn:btih:abc",
        "magnet:?xt=urn:sha1:",
        "magnet:?xt=urn%3Abtih%3",
        "magnet:?xt=urn%ZZbtih",
        "http://x/?xt=urn:btih:",
        &format!("magnet:?xt=urn:btih:{}", "1".repeat(32)),
    ] {
        assert_eq!(magnet_hash(bad), None, "{bad}");
    }
}

#[test]
fn directory_command_quotes_the_path() {
    assert_eq!(directory_command("/s/a b"), "d.directory.set=\"/s/a b\"");
    assert_eq!(
        directory_command(r#"/s/q"x\y"#),
        r#"d.directory.set="/s/q\"x\\y""#
    );
}

#[test]
fn new_rejects_non_scgi_addresses() {
    assert!(Rtorrent::new("localhost").is_err());
    assert!(Rtorrent::new("127.0.0.1:5000").is_ok());
}
