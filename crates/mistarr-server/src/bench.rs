//! The hidden `bench-seed`, `bench-search` and `bench-preview` commands; see `docs/TESTING.md`.

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use mistarr_core::PlatformId;
use mistarr_sources::binding::MatchKeys;

use crate::config::PrefsConfig;
use crate::db::ids::SourceId;
use crate::db::platforms::PlatformRef;
use crate::db::sources::{self, Tiers};
use crate::db::sql::Page;
use crate::db::titles;
use crate::db::titles::browse::{Browse, SearchShape};
use crate::db::views::source_detail;
use crate::db::{self, platforms, Db};
use crate::error::{Error, Result};
use crate::synth::{self, Seeded};

/// The benchmarked browse page, the Browse screen's first.
const PAGE: Page = Page {
    limit: 60,
    offset: 0,
};

/// Writes the synthetic catalogue at `scale` into a new database at `path`; refuses a
/// path that exists, so it never writes into an install's database.
///
/// # Errors
///
/// [`Error::Bench`] when `path` exists, [`Error::Db`] on SQLite failure.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("seed.db");
/// let seeded = mistarr_server::bench::seed_file(&path, 0.002).unwrap();
/// assert!(seeded.titles > 0);
/// assert!(mistarr_server::bench::seed_file(&path, 0.002).is_err());
/// ```
pub fn seed_file(path: &Path, scale: f64) -> Result<Seeded> {
    if path.exists() {
        return Err(Error::Bench(format!(
            "{} exists; bench-seed writes only a new file",
            path.display()
        )));
    }
    let db = Db::open(path)?;
    db.write_blocking(|c| {
        platforms::seed(c, &mistarr_mister::platforms::PLATFORMS)?;
        synth::seed(c, scale, 1)
    })
}

/// Timings of one shape over the runs of [`search`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Timing {
    /// The shape timed.
    pub shape: SearchShape,
    /// Groups the search matched.
    pub total: u64,
    /// Fastest run.
    pub min: Duration,
    /// Median run.
    pub median: Duration,
    /// 95th percentile run.
    pub p95: Duration,
}

/// Runs the first browse page and its total for `term` on `platform`, with the default
/// hide list, `iterations` times per shape after one warm-up, opening `path` read-only.
///
/// # Errors
///
/// [`Error::Bench`] for a file without the browse tables, [`Error::Db`] on SQLite failure.
///
/// ```
/// use mistarr_server::db::titles::browse::SearchShape;
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("s.db");
/// mistarr_server::bench::seed_file(&path, 0.002).unwrap();
/// let t = mistarr_server::bench::search(&path, "nes", "sta", 3, &SearchShape::ALL).unwrap();
/// assert_eq!(t.len(), 3);
/// ```
pub fn search(
    path: &Path,
    platform: &str,
    term: &str,
    iterations: u32,
    shapes: &[SearchShape],
) -> Result<Vec<Timing>> {
    let conn = db::open_read_only(path)?;
    let slug = PlatformId::new(platform.to_owned());
    let platform = &PlatformRef::resolve(&conn, &slug)?
        .ok_or_else(|| Error::Bench(format!("{slug} is not a platform in {}", path.display())))?;
    if !db::has_table(&conn, "main", "title_groups")? {
        return Err(Error::Bench(format!(
            "{} has no title_groups table; open it once with this version of mistarr",
            path.display()
        )));
    }
    let filter = Browse {
        q: Some(term.to_owned()).filter(|t| !t.is_empty()),
        hidden: PrefsConfig::default().hidden_names(),
        ..Browse::default()
    };
    let mut out = Vec::with_capacity(shapes.len());
    for &shape in shapes {
        let total = titles::browse::browse_with(&conn, platform, &filter, PAGE, shape)?.total;
        let mut runs = Vec::with_capacity(usize::try_from(iterations).unwrap_or(0));
        for _ in 0..iterations.max(1) {
            let start = Instant::now();
            titles::browse::browse_with(&conn, platform, &filter, PAGE, shape)?;
            runs.push(start.elapsed());
        }
        runs.sort();
        let at = |q: usize| runs[(runs.len() - 1) * q / 100];
        out.push(Timing {
            shape,
            total,
            min: runs[0],
            median: at(50),
            p95: at(95),
        });
    }
    Ok(out)
}

/// The timings as a table, one line per shape, in milliseconds.
///
/// ```
/// use std::time::Duration;
/// use mistarr_server::bench::{report, Timing};
/// use mistarr_server::db::titles::browse::SearchShape;
/// let ms = Duration::from_millis(2);
/// let t = Timing { shape: SearchShape::Like, total: 4, min: ms, median: ms, p95: ms };
/// assert!(report(&[t]).contains("like"));
/// ```
#[must_use]
pub fn report(timings: &[Timing]) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let mut out = String::from("shape          groups      min   median      p95  (ms)\n");
    for t in timings {
        let _ = writeln!(
            out,
            "{:<12} {:>8} {:>8.2} {:>8.2} {:>8.2}",
            t.shape.name(),
            t.total,
            ms(t.min),
            ms(t.median),
            ms(t.p95)
        );
    }
    out
}

/// Median times of one source's preview over the runs of [`preview`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PreviewTiming {
    /// The source previewed.
    pub source: SourceId,
    /// Files in its sample.
    pub sampled: u64,
    /// Reading the sample's chunks.
    pub read: Duration,
    /// Deriving the sample's match keys.
    pub keys: Duration,
    /// The scoring statements, both tiers, as the preview runs them.
    pub scored: Duration,
    /// The same statements with the name tier alone.
    pub name: Duration,
    /// The same statements with the base-name and size tier alone.
    pub base: Duration,
}

impl PreviewTiming {
    /// The preview as `GET /sources/{id}/preview` reads it: sample, keys and scoring.
    #[must_use]
    pub fn total(&self) -> Duration {
        self.read + self.keys + self.scored
    }
}

/// Times the preview of `source`, or of every source with files, `iterations` times
/// after one warm-up, opening `path` read-only: the whole preview, and each name tier
/// alone over the same chunks.
///
/// # Errors
///
/// [`Error::Bench`] for a file without the source tables, [`Error::Db`] on SQLite failure.
///
/// ```
/// let dir = tempfile::tempdir().unwrap();
/// let path = dir.path().join("p.db");
/// mistarr_server::bench::seed_file(&path, 0.002).unwrap();
/// assert!(mistarr_server::bench::preview(&path, None, 2).unwrap().is_empty());
/// ```
pub fn preview(
    path: &Path,
    source: Option<SourceId>,
    iterations: u32,
) -> Result<Vec<PreviewTiming>> {
    let conn = db::open_read_only(path)?;
    if !db::has_table(&conn, "main", "torrent_files")? {
        return Err(Error::Bench(format!(
            "{} has no torrent_files table; open it once with this version of mistarr",
            path.display()
        )));
    }
    let every = Page {
        limit: u32::MAX,
        offset: 0,
    };
    let mut out = Vec::new();
    for row in sources::list(&conn, every)?.items {
        if row.file_count == 0 || source.is_some_and(|s| s != row.id) {
            continue;
        }
        let step = source_detail::sample_step(row.file_count, source_detail::PREVIEW_SAMPLE);
        let mut runs: Vec<[Duration; 5]> = Vec::new();
        let mut sampled = 0;
        for run in 0..=iterations.max(1) {
            let mut t = [Duration::ZERO; 5];
            let mut after = None;
            sampled = 0;
            loop {
                let start = Instant::now();
                let files = source_detail::sample_files(
                    &conn,
                    row.id,
                    after,
                    step,
                    source_detail::PREVIEW_CHUNK,
                )?;
                t[0] += start.elapsed();
                let start = Instant::now();
                let keys: Vec<MatchKeys> = files.iter().map(MatchKeys::of).collect();
                t[1] += start.elapsed();
                for (slot, tiers) in [(2, Tiers::All), (3, Tiers::Name), (4, Tiers::Base)] {
                    let start = Instant::now();
                    sources::platform_hits(&conn, &keys, tiers)?;
                    t[slot] += start.elapsed();
                }
                sampled += files.len() as u64;
                match files.last() {
                    Some(f) if files.len() == source_detail::PREVIEW_CHUNK as usize => {
                        after = Some(f.index);
                    }
                    _ => break,
                }
            }
            // The first run warms the page cache and the statement cache.
            if run > 0 {
                runs.push(t);
            }
        }
        let median = |slot: usize| {
            let mut v: Vec<Duration> = runs.iter().map(|t| t[slot]).collect();
            v.sort();
            v[(v.len() - 1) / 2]
        };
        out.push(PreviewTiming {
            source: row.id,
            sampled,
            read: median(0),
            keys: median(1),
            scored: median(2),
            name: median(3),
            base: median(4),
        });
    }
    Ok(out)
}

/// The preview timings as a table, one line per source, in milliseconds, with the base
/// tier's share of the whole preview and of its scoring.
///
/// ```
/// use std::time::Duration;
/// use mistarr_server::bench::{preview_report, PreviewTiming};
/// use mistarr_server::db::ids::SourceId;
/// let ms = Duration::from_millis(2);
/// let t = PreviewTiming {
///     source: SourceId::new(1), sampled: 10,
///     read: ms, keys: ms, scored: ms, name: ms, base: ms,
/// };
/// assert!(preview_report(&[t]).contains("33.3"));
/// ```
#[must_use]
pub fn preview_report(timings: &[PreviewTiming]) -> String {
    let ms = |d: Duration| d.as_secs_f64() * 1000.0;
    let share = |part: Duration, of: Duration| {
        if of.is_zero() {
            0.0
        } else {
            100.0 * part.as_secs_f64() / of.as_secs_f64()
        }
    };
    let mut out = String::from(
        "source  sampled     read     keys   scored     name     base    total  base%total  base%scored  (ms)\n",
    );
    for t in timings {
        let _ = writeln!(
            out,
            "{:<6} {:>8} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>8.2} {:>11.1} {:>12.1}",
            t.source.to_string(),
            t.sampled,
            ms(t.read),
            ms(t.keys),
            ms(t.scored),
            ms(t.name),
            ms(t.base),
            ms(t.total()),
            share(t.base, t.total()),
            share(t.base, t.scored),
        );
    }
    out
}
