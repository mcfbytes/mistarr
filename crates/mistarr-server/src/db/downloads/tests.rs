use mistarr_sources::binding::{Confidence, RomRef};
use mistarr_sources::torrent::TorrentFile;

use super::*;
use crate::db::sources::{self, fixtures::seed_rom, NewSource, SourceState};

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

fn title_of(c: &Connection, rom: i64) -> TitleId {
    TitleId(
        c.query_row("SELECT title_id FROM roms WHERE id = ?1", [rom], |r| {
            r.get(0)
        })
        .expect("title"),
    )
}

/// A bound source whose files are `(path, size, rom, confidence)`.
fn source(c: &Connection, byte: u8, files: &[(&str, u64, Option<i64>, Confidence)]) -> SourceId {
    let hash = format!("{byte:02x}").repeat(20);
    let id = sources::insert(
        c,
        &NewSource {
            infohash: &hash,
            display_name: "Synthetic Set",
            origin_file: "set.torrent",
            state: SourceState::Bound,
            reason: None,
            added_at: 0,
        },
    )
    .expect("insert");
    let list: Vec<TorrentFile> = files
        .iter()
        .zip(0u32..)
        .map(|((p, n, _, _), index)| TorrentFile {
            index,
            path: (*p).to_owned(),
            size: *n,
        })
        .collect();
    sources::replace_files(c, id, &list).expect("files");
    let matches: Vec<_> = files
        .iter()
        .zip(0u32..)
        .map(|((_, _, rom, conf), i)| (i, rom.map(RomRef), *conf))
        .collect();
    sources::set_matches(c, id, &matches).expect("matches");
    id
}

#[allow(clippy::unnecessary_wraps)] // Reads as the `Option` the functions return.
fn candidate(source_id: SourceId, file_index: u32) -> Option<Candidate> {
    Some(Candidate {
        source_id,
        file_index,
    })
}

#[test]
fn states_round_trip_and_follow_the_machine() {
    use DownloadState::*;
    for s in DownloadState::ALL {
        assert_eq!(DownloadState::parse(s.as_str()), Some(s));
        assert_eq!(s.to_string(), s.as_str());
    }
    assert!(Wanted.can_become(Queued) && Queued.can_become(Transferring));
    assert!(Transferring.can_become(Checking) && Checking.can_become(Importing));
    assert!(Importing.can_become(Done) && Importing.can_become(Bad));
    assert!(!Importing.can_become(Cancelled));
    assert!(!Done.can_become(Queued) && !Cancelled.can_become(Queued));
    assert!(!Wanted.can_become(Transferring));
    assert_eq!(DownloadId(4).to_string(), "4");
}

#[test]
fn best_file_prefers_size_then_name_then_load_then_id() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let wrong_size = source(
        &c,
        1,
        &[("Example Quest (USA).nes", 17, Some(rom), Confidence::Name)],
    );
    assert_eq!(best_file(&c, rom).expect("best"), candidate(wrong_size, 0));
    let by_size = source(&c, 2, &[("x.nes", 16, Some(rom), Confidence::Base)]);
    assert_eq!(best_file(&c, rom).expect("best"), candidate(by_size, 0));
    let by_name = source(
        &c,
        3,
        &[
            ("other.nes", 4, None, Confidence::Unmatched),
            ("Example Quest (USA).nes", 16, Some(rom), Confidence::Name),
        ],
    );
    assert_eq!(best_file(&c, rom).expect("best"), candidate(by_name, 1));
    let tie = source(
        &c,
        4,
        &[("Example Quest (USA).nes", 16, Some(rom), Confidence::Name)],
    );
    assert_eq!(best_file(&c, rom).expect("best"), candidate(by_name, 1));

    let title = title_of(&c, rom);
    let other = seed_rom(&c, "nes", "Second Try (USA).nes", 8, "[]").expect("rom");
    let busy = create(
        &c,
        &NewDownload {
            title_id: title_of(&c, other),
            rom_id: other,
            file: candidate(by_name, 0),
            now: 1,
        },
    )
    .expect("create");
    assert_eq!(best_file(&c, rom).expect("best"), candidate(tie, 0));
    set_state(&c, busy, DownloadState::Cancelled, 2).expect("cancel");
    sources::set_state(&c, by_name, SourceState::Disabled, None).expect("disable");
    assert_eq!(best_file(&c, rom).expect("best"), candidate(tie, 0));

    let bad = create(
        &c,
        &NewDownload {
            title_id: title,
            rom_id: rom,
            file: candidate(tie, 0),
            now: 3,
        },
    )
    .expect("create");
    c.execute("UPDATE downloads SET state = 'bad' WHERE id = ?1", [bad.0])
        .expect("bad");
    assert_eq!(best_file(&c, rom).expect("best"), candidate(by_size, 0));
    assert_eq!(best_file(&c, 999).expect("best"), None);
}

#[test]
fn want_title_queues_or_waits_and_skips_open_and_verified_roms() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let title = title_of(&c, rom);
    let wanted = want_title(&c, title, 5).expect("want");
    assert_eq!(wanted.len(), 1);
    assert_eq!(wanted[0].1, DownloadState::Wanted);
    assert!(want_title(&c, title, 6).expect("again").is_empty());
    assert!(promote_wanted(&c, 7).expect("promote").is_empty());

    let src = source(
        &c,
        1,
        &[("Example Quest (USA).nes", 16, Some(rom), Confidence::Name)],
    );
    assert_eq!(promote_wanted(&c, 8).expect("promote"), [wanted[0].0]);
    let row = get(&c, wanted[0].0).expect("get").expect("row");
    assert_eq!(row.state, DownloadState::Queued);
    assert_eq!((row.source_id, row.file_index), (Some(src), Some(0)));
    assert_eq!(row.title_name, "Example Quest (USA)");
    assert_eq!(
        (row.rom_name.as_str(), row.size),
        ("Example Quest (USA).nes", 16)
    );
    assert_eq!(row.platform_id.0, "nes");

    set_state(&c, row.id, DownloadState::Cancelled, 9).expect("cancel");
    let again = want_title(&c, title, 10).expect("want");
    assert_eq!(again.len(), 1);
    assert_eq!(again[0].1, DownloadState::Queued);
    set_state(&c, again[0].0, DownloadState::Cancelled, 11).expect("cancel");
    c.execute(
        "INSERT INTO files (platform_id, rel_path, size, mtime, rom_id, state, scanned_at)
         VALUES ('nes', 'a.nes', 16, 0, ?1, 'verified', 0)",
        [rom],
    )
    .expect("file");
    assert!(want_title(&c, title, 12).expect("have").is_empty());
}

#[test]
fn transitions_progress_and_lookups() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let src = source(
        &c,
        1,
        &[("Example Quest (USA).nes", 16, Some(rom), Confidence::Name)],
    );
    let id = create(
        &c,
        &NewDownload {
            title_id: title_of(&c, rom),
            rom_id: rom,
            file: candidate(src, 0),
            now: 1,
        },
    )
    .expect("create");
    assert_eq!(queued_sources(&c).expect("queued"), [src]);
    assert_eq!(selected_indices(&c, src).expect("selected"), [0]);
    assert!(!set_state(&c, id, DownloadState::Importing, 2).expect("skip"));
    assert!(!set_state(&c, DownloadId(99), DownloadState::Queued, 2).expect("missing"));
    assert_eq!(
        move_all(&c, &[id], DownloadState::Transferring, None, 2).expect("start"),
        [id]
    );
    assert!(queued_sources(&c).expect("queued").is_empty());
    assert_eq!(
        count_in(&c, &[DownloadState::Transferring]).expect("count"),
        1
    );

    let half = Observed {
        state: DownloadState::Transferring,
        progress: 0.5,
        staged_path: None,
        error: None,
    };
    assert!(observe(&c, id, &half, 3).expect("observe"));
    assert!(!observe(&c, id, &half, 4).expect("unchanged"));
    let wrong = Observed {
        state: DownloadState::Done,
        ..half
    };
    assert!(!observe(&c, id, &wrong, 4).expect("not allowed"));
    let polled_rows = polled(&c).expect("polled");
    assert_eq!(polled_rows.len(), 1);
    let p = &polled_rows[0];
    assert_eq!((p.id, p.file_index, p.progress), (id, 0, 0.5));
    assert_eq!(p.infohash, "01".repeat(20));
    assert!(!p.single_file);
    assert_eq!(p.seed_policy, "none");

    let done = Observed {
        state: DownloadState::Importing,
        progress: 1.0,
        staged_path: Some("/s/x.nes"),
        error: None,
    };
    assert!(observe(&c, id, &done, 5).expect("importing"));
    assert!(polled(&c).expect("polled").is_empty());
    let found = find_by_file(&c, src, 0).expect("find").expect("row");
    assert_eq!(found.state, DownloadState::Importing);
    assert_eq!(found.staged_path.as_deref(), Some("/s/x.nes"));
    assert_eq!(
        of_source(&c, src, &[DownloadState::Importing])
            .expect("of")
            .len(),
        1
    );
    assert!(find_by_file(&c, src, 1).expect("find").is_none());

    let (page, total) = list(&c, &[], 10, 0).expect("list");
    assert_eq!((page.len(), total), (1, 1));
    let (page, total) = list(&c, &[DownloadState::Failed], 10, 0).expect("list");
    assert!(page.is_empty() && total == 0);
}

#[test]
fn retry_and_cancel() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let title = title_of(&c, rom);
    let src = source(
        &c,
        1,
        &[("Example Quest (USA).nes", 16, Some(rom), Confidence::Name)],
    );
    let new = NewDownload {
        title_id: title,
        rom_id: rom,
        file: candidate(src, 0),
        now: 1,
    };
    let id = create(&c, &new).expect("create");
    assert_eq!(
        retry(&c, id, 2).expect("retry"),
        RetryOutcome::NotFailed(DownloadState::Queued)
    );
    assert_eq!(
        retry(&c, DownloadId(99), 2).expect("retry"),
        RetryOutcome::Missing
    );
    move_all(&c, &[id], DownloadState::Failed, Some("disk full"), 2).expect("fail");
    let row = get(&c, id).expect("get").expect("row");
    assert_eq!(
        (row.state, row.error.as_deref()),
        (DownloadState::Failed, Some("disk full"))
    );
    let other = create(&c, &new).expect("create");
    assert_eq!(retry(&c, id, 3).expect("retry"), RetryOutcome::Busy);
    assert!(matches!(
        cancel(&c, other, 3).expect("cancel"),
        CancelOutcome::Cancelled(Cancelled { started: false, .. })
    ));
    assert_eq!(retry(&c, id, 4).expect("retry"), RetryOutcome::Queued);
    let row = get(&c, id).expect("get").expect("row");
    assert_eq!((row.state, row.error), (DownloadState::Queued, None));

    move_all(&c, &[id], DownloadState::Transferring, None, 5).expect("start");
    match cancel(&c, id, 6).expect("cancel") {
        CancelOutcome::Cancelled(x) => {
            assert!(x.started);
            assert_eq!(x.source_id, Some(src));
        }
        other => panic!("{other:?}"),
    }
    assert_eq!(
        cancel(&c, id, 7).expect("cancel"),
        CancelOutcome::Final(DownloadState::Cancelled)
    );
    assert_eq!(
        cancel(&c, DownloadId(99), 7).expect("cancel"),
        CancelOutcome::Missing
    );
}

#[test]
fn cancel_group_reports_started_downloads() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let title = title_of(&c, rom);
    let src = source(
        &c,
        1,
        &[("Example Quest (USA).nes", 16, Some(rom), Confidence::Name)],
    );
    let new = NewDownload {
        title_id: title,
        rom_id: rom,
        file: candidate(src, 0),
        now: 1,
    };
    let a = create(&c, &new).expect("create");
    let b = create(&c, &new).expect("create");
    let imp = create(&c, &new).expect("create");
    move_all(&c, &[b, imp], DownloadState::Transferring, None, 2).expect("start");
    move_all(&c, &[imp], DownloadState::Importing, None, 2).expect("import");
    let got = cancel_group(&c, title, 3).expect("cancel");
    assert_eq!(
        got,
        [
            Cancelled {
                id: a,
                source_id: Some(src),
                started: false
            },
            Cancelled {
                id: b,
                source_id: Some(src),
                started: true
            },
        ]
    );
    assert_eq!(selected_indices(&c, src).expect("selected"), [0]);
    let row = get(&c, imp).expect("get").expect("row");
    assert_eq!(row.state, DownloadState::Importing);
}

#[test]
fn deleting_a_source_keeps_finished_downloads_without_it() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let src = source(
        &c,
        1,
        &[("Example Quest (USA).nes", 16, Some(rom), Confidence::Name)],
    );
    let new = NewDownload {
        title_id: title_of(&c, rom),
        rom_id: rom,
        file: candidate(src, 0),
        now: 1,
    };
    let done = create(&c, &new).expect("create");
    for to in [
        DownloadState::Transferring,
        DownloadState::Importing,
        DownloadState::Done,
    ] {
        assert!(set_state(&c, done, to, 2).expect("move"));
    }
    let cancelled = create(&c, &new).expect("create");
    let open = create(&c, &new).expect("create");
    set_state(&c, cancelled, DownloadState::Cancelled, 3).expect("cancel");
    assert_eq!(sources::open_download_count(&c, src).expect("count"), 1);
    set_state(&c, open, DownloadState::Cancelled, 4).expect("cancel");
    assert_eq!(sources::open_download_count(&c, src).expect("count"), 0);
    assert!(sources::delete(&c, src).expect("delete"));
    for id in [done, cancelled, open] {
        assert_eq!(get(&c, id).expect("get").expect("kept").source_id, None);
    }
    let row = get(&c, done).expect("get").expect("row");
    assert_eq!(row.state, DownloadState::Done);
}

#[test]
fn works_on_the_file_database() {
    let (_dir, db) = crate::db::testutil::db();
    let n = db
        .read_blocking(|c| count_in(c, &DownloadState::ALL))
        .expect("count");
    assert_eq!(n, 0);
}

#[test]
fn name_tier_files_beat_candidates_and_a_header_on_top_counts_as_the_size() {
    let c = conn();
    let rom = seed_rom(&c, "nes", "Example Quest (USA).nes", 16, "[]").expect("rom");
    let guessed = source(&c, 1, &[("example.nes", 16, None, Confidence::Unmatched)]);
    let change = crate::db::candidates::Change {
        add: vec![(0, rom, "fuzzy")],
        ..Default::default()
    };
    crate::db::candidates::apply(&c, guessed, &change).expect("candidate");
    assert_eq!(best_file(&c, rom).expect("best"), candidate(guessed, 0));
    let odd = source(
        &c,
        2,
        &[("Example Quest (USA).nes", 17, Some(rom), Confidence::Name)],
    );
    assert_eq!(
        best_file(&c, rom).expect("best"),
        candidate(odd, 0),
        "tier first"
    );
    let headered = source(
        &c,
        3,
        &[("Example Quest.nes", 32, Some(rom), Confidence::Base)],
    );
    assert_eq!(
        best_file(&c, rom).expect("best"),
        candidate(headered, 0),
        "a size with the iNES header on top matches"
    );
}
