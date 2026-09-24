use super::*;
use crate::db::groups;

fn conn() -> Connection {
    let mut c = Connection::open_in_memory().expect("open");
    crate::db::migrate::apply(&mut c).expect("migrate");
    crate::db::platforms::seed(&mut c, &mistarr_mister::platforms::PLATFORMS).expect("seed");
    c
}

fn count(c: &Connection, sql: &str) -> i64 {
    c.query_row(sql, [], |r| r.get(0)).expect(sql)
}

#[test]
fn every_console_is_a_known_platform() {
    for console in &CONSOLES {
        assert!(
            mistarr_mister::platforms::PLATFORMS
                .iter()
                .any(|p| p.id == console.id),
            "{}",
            console.id
        );
    }
    let total: usize = CONSOLES.iter().map(|c| c.titles).sum();
    assert!((40_000..60_000).contains(&total), "{total}");
}

#[test]
fn a_small_seed_is_consistent_and_repeatable() {
    let mut a = conn();
    let seeded = seed(&mut a, 0.01, 7).expect("seed");
    let titles = count(&a, "SELECT COUNT(*) FROM titles");
    assert_eq!(u64::try_from(titles).ok(), Some(seeded.titles));
    assert_eq!(
        u64::try_from(count(&a, "SELECT COUNT(*) FROM roms")).ok(),
        Some(seeded.roms)
    );
    assert!(seeded.roms > seeded.titles && seeded.files > 0);
    assert!(groups::check(&a).expect("check").is_consistent());
    let groups = count(&a, "SELECT COUNT(*) FROM title_groups");
    assert!(
        titles > groups * 2 && titles < groups * 4,
        "{titles} in {groups}"
    );
    let mut b = conn();
    assert_eq!(seed(&mut b, 0.01, 7).expect("seed"), seeded);
    let names = |c: &Connection| -> Vec<String> {
        c.prepare("SELECT name FROM titles ORDER BY id")
            .expect("prepare")
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<rusqlite::Result<_>>()
            .expect("rows")
    };
    assert_eq!(names(&a), names(&b));
}

#[test]
fn common_trigrams_span_every_platform_and_placed_words_do_not() {
    let mut c = conn();
    seed(&mut c, 0.2, 3).expect("seed");
    for term in ["the", "sta", "man"] {
        let platforms = count(
            &c,
            &format!(
                "SELECT COUNT(DISTINCT platform_id) FROM titles WHERE base_name LIKE '%{term}%'"
            ),
        );
        assert!(platforms >= 25, "{term} on {platforms} platforms");
    }
    for p in BROWSED {
        let here = count(
            &c,
            &format!(
                "SELECT COUNT(*) FROM title_groups WHERE platform_id = '{p}' AND base_name LIKE '%{ELSEWHERE}%'"
            ),
        );
        assert_eq!(here, 1, "{p}");
    }
    let elsewhere = count(
        &c,
        &format!("SELECT COUNT(*) FROM title_groups WHERE base_name LIKE '%{ELSEWHERE}%'"),
    );
    assert!(elsewhere > 50, "{elsewhere}");
    let rare = count(
        &c,
        &format!("SELECT COUNT(*) FROM title_groups WHERE base_name LIKE '%{RARE}%'"),
    );
    assert!((1..=60).contains(&rare), "{rare}");
    let arcade = count(
        &c,
        "SELECT COUNT(*) FROM titles WHERE platform_id = 'arcade' AND source <> 'mra'",
    );
    assert_eq!(arcade, 0);
}

#[test]
fn game_names_are_distinct_dat_names_in_groups() {
    let names = game_names(2_000, 5);
    assert_eq!(names.len(), 2_000);
    let unique: HashSet<&String> = names.iter().collect();
    assert_eq!(unique.len(), names.len());
    let regions = names.iter().filter(|n| n.contains("(Europe)")).count();
    assert!(regions > 200 && regions < 1_600, "{regions}");
    assert_eq!(game_names(2_000, 5), names);
}
