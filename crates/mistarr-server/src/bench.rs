//! The hidden `bench-seed` and `bench-search` commands; see `docs/TESTING.md` "Browse speed".

use std::fmt::Write as _;
use std::path::Path;
use std::time::{Duration, Instant};

use crate::config::PrefsConfig;
use crate::db::titles::{self, Browse, SearchShape};
use crate::db::{self, platforms, Db};
use crate::error::{Error, Result};
use crate::synth::{self, Seeded};

/// Page size of the benchmarked browse, the Browse screen's.
const PAGE: u32 = 60;

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
/// use mistarr_server::db::titles::SearchShape;
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
    if !db::has_table(&conn, "title_groups")? {
        return Err(Error::Bench(format!(
            "{} has no title_groups table; open it once with this version of mistarr",
            path.display()
        )));
    }
    let filter = Browse {
        q: Some(term.to_owned()).filter(|t| !t.is_empty()),
        hidden: PrefsConfig::default().hide,
        ..Browse::default()
    };
    let mut out = Vec::with_capacity(shapes.len());
    for &shape in shapes {
        let (_, total) = titles::browse_with(&conn, platform, &filter, PAGE, 0, shape)?;
        let mut runs = Vec::with_capacity(usize::try_from(iterations).unwrap_or(0));
        for _ in 0..iterations.max(1) {
            let start = Instant::now();
            titles::browse_with(&conn, platform, &filter, PAGE, 0, shape)?;
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
/// use mistarr_server::db::titles::SearchShape;
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
