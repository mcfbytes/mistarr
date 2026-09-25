use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

fn places(dir: &Path, ram: bool) -> Places {
    Places {
        ram: ram.then(|| dir.join("ram")),
        card: dir.join("card"),
        floor: 0,
    }
}

fn no_rest() -> Pace {
    Arc::new(|_| Duration::ZERO)
}

fn never() -> Stop {
    Arc::new(|| false)
}

/// A pace that counts its calls and never rests.
fn counted() -> (Pace, Arc<AtomicUsize>) {
    let n = Arc::new(AtomicUsize::new(0));
    let c = Arc::clone(&n);
    let pace: Pace = Arc::new(move |_| {
        c.fetch_add(1, Ordering::Relaxed);
        Duration::ZERO
    });
    (pace, n)
}

fn body(len: usize) -> Vec<u8> {
    (0..len)
        .map(|i| u8::try_from(i % 251).unwrap_or(0))
        .collect()
}

fn files_in(dir: &Path) -> Vec<String> {
    std::fs::read_dir(dir).map_or_else(
        |_| Vec::new(),
        |d| {
            d.flatten()
                .map(|e| e.file_name().to_string_lossy().into_owned())
                .collect()
        },
    )
}

fn recheck_len() -> usize {
    usize::try_from(RECHECK_BYTES).expect("fits")
}

#[tokio::test]
async fn a_spool_fills_and_moves_in_chunks() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (pace, rests) = counted();
    let mut spool = Spool::create(places(dir.path(), true), 7, Some(10), pace)
        .await
        .expect("create");
    assert!(spool.in_ram());
    let body = body(CHUNK_BYTES * 2 + 5);
    for part in body.chunks(100_000) {
        spool.push(part).await.expect("push");
    }
    assert_eq!(spool.finish().await.expect("finish"), body.len() as u64);
    assert_eq!(rests.load(Ordering::Relaxed), 0, "RAM writes do not rest");
    let from = spool.path().to_path_buf();
    let to = dir.path().join("placed.dat");
    spool.place(to.clone(), never()).await.expect("place");
    assert_eq!(std::fs::read(&to).expect("read"), body);
    assert!(!from.exists());
    assert_eq!(rests.load(Ordering::Relaxed), 3, "one per MiB written");
}

/// A spool in RAM whose floor is then raised so the next recheck moves it.
async fn squeezed(dir: &Path, pace: Pace) -> Spool {
    let mut spool = Spool::create(places(dir, true), 11, Some(10), pace)
        .await
        .expect("create");
    assert!(spool.in_ram());
    spool.places.floor = u64::MAX / 2;
    spool
}

#[tokio::test]
async fn memory_running_short_moves_the_file_to_the_card_with_paced_writes() {
    let dir = tempfile::tempdir().expect("tempdir");
    let (pace, rests) = counted();
    let mut spool = squeezed(dir.path(), pace).await;
    let data = body(recheck_len() + CHUNK_BYTES + 3);
    for part in data.chunks(CHUNK_BYTES) {
        spool.push(part).await.expect("push");
    }
    spool.finish().await.expect("finish");
    assert!(!spool.in_ram());
    assert!(spool.path().starts_with(dir.path().join("card")));
    assert!(files_in(&dir.path().join("ram")).is_empty());
    assert_eq!(std::fs::read(spool.path()).expect("read"), data);
    let rested = rests.load(Ordering::Relaxed);
    assert!(rested >= 9, "the move and later card writes rest, {rested}");
}

#[tokio::test]
async fn a_failed_move_leaves_no_partial_copy() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let mut spool = squeezed(dir.path(), no_rest()).await;
    let ram = dir.path().join("ram");
    let data = body(CHUNK_BYTES);
    let mut failed = None;
    for _ in 0..8 {
        if spool.written + CHUNK_BYTES as u64 * 2 >= RECHECK_BYTES {
            std::fs::set_permissions(&ram, std::fs::Permissions::from_mode(0o500)).expect("chmod");
        }
        if let Err(e) = spool.push(&data).await {
            failed = Some(e);
            break;
        }
    }
    std::fs::set_permissions(&ram, std::fs::Permissions::from_mode(0o700)).expect("chmod");
    assert!(failed.is_some(), "removing the RAM file failed");
    assert!(files_in(&dir.path().join("card")).is_empty());
    let path = spool.path().to_path_buf();
    assert!(path.starts_with(&ram));
    drop(spool);
    assert!(!path.exists());
}

#[tokio::test]
async fn a_stop_while_placing_leaves_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut spool = Spool::create(places(dir.path(), true), 12, Some(10), no_rest())
        .await
        .expect("create");
    spool.push(&body(CHUNK_BYTES * 3)).await.expect("push");
    spool.finish().await.expect("finish");
    let from = spool.path().to_path_buf();
    let asked = Arc::new(AtomicUsize::new(0));
    let seen = Arc::clone(&asked);
    let stop: Stop = Arc::new(move || seen.fetch_add(1, Ordering::Relaxed) >= 1);
    let to = dir.path().join("placed.dat");
    let e = spool.place(to.clone(), stop).await.expect_err("stopped");
    assert!(matches!(e, Error::Cancelled), "{e:?}");
    assert!(!to.exists() && !from.exists());
}

#[tokio::test]
async fn a_dropped_spool_leaves_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut spool = Spool::create(places(dir.path(), false), 8, None, no_rest())
        .await
        .expect("create");
    assert!(!spool.in_ram());
    spool.push(b"abc").await.expect("push");
    let path = spool.path().to_path_buf();
    assert!(path.starts_with(dir.path().join("card")));
    drop(spool);
    assert!(!path.exists());
}

#[tokio::test]
async fn a_file_on_the_card_is_renamed_and_an_adopted_one_placed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut spool = Spool::create(places(dir.path(), false), 9, Some(3), no_rest())
        .await
        .expect("create");
    spool.push(b"xyz").await.expect("push");
    spool.finish().await.expect("finish");
    let to = dir.path().join("x.torrent");
    let old = spool.path().to_path_buf();
    let target = spool.target(100);
    assert!(!target.ram && target.name.ends_with("-out.part"));
    let mut spill = Spill::create(target, 3).expect("spill");
    spill.write_all(b"new").expect("write");
    let (path, in_ram) = spill.finish().expect("finish");
    spool.adopt(path, in_ram);
    assert!(!old.exists());
    spool.place(to.clone(), never()).await.expect("place");
    assert_eq!(std::fs::read(&to).expect("read"), b"new");
}

fn target(dir: &Path, ram: bool, limit: u64) -> Target {
    Target {
        places: places(dir, ram),
        name: "fetch-1-2-out.part".into(),
        ram,
        pace: no_rest(),
        limit,
    }
}

#[test]
fn a_spill_moves_to_the_card_mid_write_and_keeps_its_place() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(dir.path().join("ram")).expect("mkdir");
    let mut spill = Spill::create(target(dir.path(), true, u64::MAX), 10).expect("spill");
    assert!(spill.in_ram);
    spill.target.places.floor = u64::MAX / 2;
    let data = body(recheck_len() + 5);
    spill.write_all(&data).expect("write");
    spill.write_all(b"tail").expect("write");
    spill.seek(SeekFrom::Start(0)).expect("seek");
    spill.write_all(b"HEAD").expect("write");
    let (path, in_ram) = spill.finish().expect("finish");
    assert!(!in_ram && path.starts_with(dir.path().join("card")));
    assert!(files_in(&dir.path().join("ram")).is_empty());
    let mut expected = data;
    expected.extend_from_slice(b"tail");
    expected[..4].copy_from_slice(b"HEAD");
    assert_eq!(std::fs::read(&path).expect("read"), expected);
}

#[test]
fn a_spill_past_its_limit_fails_and_a_dropped_one_leaves_nothing() {
    let dir = tempfile::tempdir().expect("tempdir");
    let mut spill = Spill::create(target(dir.path(), false, 4), 1).expect("spill");
    spill.write_all(b"abcd").expect("within");
    let e = spill.write_all(b"e").expect_err("over");
    assert_eq!(e.kind(), std::io::ErrorKind::FileTooLarge);
    let path = spill.path.clone();
    assert!(path.exists());
    drop(spill);
    assert!(!path.exists());
}

#[test]
fn the_card_keeps_a_spare_beyond_each_write() {
    assert!(fits(None, u64::MAX));
    assert!(fits(Some(CARD_SPARE + 10), 10));
    assert!(!fits(Some(CARD_SPARE + 9), 10));
    assert_eq!(no_room().kind(), std::io::ErrorKind::StorageFull);
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(card_allows(dir.path(), 1));
    assert!(!card_allows(dir.path(), u64::MAX / 2));
    let e = Spill::create(target(dir.path(), false, u64::MAX), u64::MAX / 2)
        .err()
        .expect("no room");
    assert_eq!(e.kind(), std::io::ErrorKind::StorageFull);
}

#[test]
fn stale_parts_are_removed() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join("fetch-1-2.part"), b"x").expect("write");
    std::fs::write(dir.path().join("etilqs_1"), b"x").expect("write");
    std::fs::write(dir.path().join(".upload-1-2.part"), b"x").expect("write");
    clean_stale(dir.path());
    assert!(!dir.path().join("fetch-1-2.part").exists());
    assert!(dir.path().join("etilqs_1").exists());
    clean_parts(dir.path(), ".upload-");
    assert!(!dir.path().join(".upload-1-2.part").exists());
    clean_stale(&dir.path().join("none"));
}

#[test]
fn memory_short_of_the_floor_keeps_a_file_off_ram() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(ram_allows(dir.path(), 1, 0));
    assert!(!ram_allows(dir.path(), 1, u64::MAX / 2));
}
