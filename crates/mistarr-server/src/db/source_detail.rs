//! Reads behind a source's detail view: its file page, summary, DATs and re-classify preview.

use mistarr_core::PlatformId;
use mistarr_mister::platforms::{self, Kind};
use mistarr_sources::binding;
use rusqlite::{params, Connection, OptionalExtension, Row};
use serde::Serialize;

use super::candidates::FileCandidate;
use super::sources::{self, SourceId, SourceRow, SqlDatIndex};
use crate::error::Result;

/// Download states that keep a file selected in the client.
const OPEN: &str = "('queued', 'transferring', 'checking', 'importing')";

/// A file that holds a matched rom or a candidate rom, as `matched_count` counts it.
const HAS_MATCH: &str = "(f.rom_id IS NOT NULL OR EXISTS (SELECT 1 FROM torrent_candidates c
    WHERE c.source_id = f.source_id AND c.file_index = f.file_index))";

/// A file with a download that was not cancelled.
const HAS_DOWNLOAD: &str = "EXISTS (SELECT 1 FROM downloads w
    WHERE w.source_id = f.source_id AND w.file_index = f.file_index AND w.state != 'cancelled')";

/// Which files of a source a page lists.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub enum FileFilter {
    /// Files with a matched rom or a candidate.
    Matched,
    /// Files with neither.
    Unmatched,
    /// Files with a download that was not cancelled.
    Wanted,
}

impl FileFilter {
    /// Parses the `filter` query value.
    ///
    /// ```
    /// use mistarr_server::db::source_detail::FileFilter;
    /// assert_eq!(FileFilter::parse("wanted"), Some(FileFilter::Wanted));
    /// assert_eq!(FileFilter::parse("x"), None);
    /// ```
    #[must_use]
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "matched" => Some(Self::Matched),
            "unmatched" => Some(Self::Unmatched),
            "wanted" => Some(Self::Wanted),
            _ => None,
        }
    }

    fn clause(self) -> String {
        match self {
            Self::Matched => HAS_MATCH.to_owned(),
            Self::Unmatched => format!("NOT {HAS_MATCH}"),
            Self::Wanted => HAS_DOWNLOAD.to_owned(),
        }
    }
}

/// The filter and search of a file page; the default lists every file.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileQuery {
    /// Which files.
    pub filter: Option<FileFilter>,
    /// Text the path must contain, ignoring ASCII case.
    pub q: Option<String>,
}

/// What a file in a torrent is, from its extension and the source's platform.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
#[non_exhaustive]
pub enum FileKind {
    /// A game file the platform loads.
    Rom,
    /// A zip or other archive holding game files.
    Archive,
    /// A disc image or one of its tracks or sheets.
    Disc,
    /// Text, images, checksums and other files that come with a set.
    Extra,
}

const EXTRA: [&str; 22] = [
    "txt", "nfo", "diz", "md", "pdf", "htm", "html", "url", "log", "jpg", "jpeg", "png", "gif",
    "bmp", "webp", "sfv", "md5", "sha1", "xml", "dat", "json", "ini",
];
const DISC: [&str; 10] = [
    "chd", "cue", "iso", "gdi", "cdi", "ccd", "img", "sub", "mds", "mdf",
];

/// The kind of the file at `path` in a source bound to `platform`, if any.
///
/// ```
/// use mistarr_server::db::source_detail::{file_kind, FileKind};
/// assert_eq!(file_kind("Set/readme.NFO", None), FileKind::Extra);
/// assert_eq!(file_kind("a.zip", Some("nes")), FileKind::Archive);
/// assert_eq!(file_kind("a.zip", Some("arcade")), FileKind::Rom);
/// assert_eq!(file_kind("t.bin", Some("psx")), FileKind::Disc);
/// assert_eq!(file_kind("t.bin", Some("megadrive")), FileKind::Rom);
/// ```
#[must_use]
pub fn file_kind(path: &str, platform: Option<&str>) -> FileKind {
    let leaf = path.rsplit('/').next().unwrap_or(path);
    let ext = leaf
        .rsplit_once('.')
        .map(|(_, e)| e.to_ascii_lowercase())
        .unwrap_or_default();
    let kind = platform.and_then(platforms::by_id).map(|p| p.kind);
    if EXTRA.contains(&ext.as_str()) {
        FileKind::Extra
    } else if DISC.contains(&ext.as_str()) || (ext == "bin" && kind == Some(Kind::Disc)) {
        FileKind::Disc
    } else if matches!(ext.as_str(), "zip" | "7z" | "rar")
        && !matches!(kind, Some(Kind::Arcade | Kind::Romset))
    {
        FileKind::Archive
    } else {
        FileKind::Rom
    }
}

/// Why a file has no matched rom and no candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
#[non_exhaustive]
pub enum Unmatched {
    /// The source is not bound to a platform.
    Unbound,
    /// The file is an [`FileKind::Extra`], which no DAT entry is expected to list.
    Extra,
    /// No DAT entry of the platform has its name, or its base name and size.
    NoEntry,
}

/// The newest download of a file.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileDownload {
    /// The download's id.
    pub id: i64,
    /// Its state, as `/downloads` names it.
    pub state: String,
    /// Share transferred, 0 to 1.
    pub progress: f64,
}

/// One file of a source as its detail lists it.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct FileRow {
    /// Index in the torrent.
    pub file_index: u32,
    /// Path inside the torrent.
    pub path: String,
    /// Size in bytes.
    pub size: u64,
    /// What the file is.
    pub kind: FileKind,
    /// Matched rom.
    pub rom_id: Option<i64>,
    /// The matched rom's DAT name.
    pub rom_name: Option<String>,
    /// The matched rom's title.
    pub title_id: Option<i64>,
    /// `hash`, `name` or `base`, `None` when unmatched.
    pub confidence: Option<String>,
    /// The file's candidate roms, strongest first.
    pub candidates: Vec<FileCandidate>,
    /// Why nothing matched, `None` when a rom or a candidate did.
    pub unmatched: Option<Unmatched>,
    /// The file's newest download, if any.
    pub download: Option<FileDownload>,
}

fn uint(r: &Row<'_>, i: usize) -> rusqlite::Result<u64> {
    Ok(u64::try_from(r.get::<_, i64>(i)?).unwrap_or(0))
}

fn platform_of(conn: &Connection, id: SourceId) -> Result<Option<String>> {
    Ok(conn
        .query_row(
            "SELECT platform_id FROM sources WHERE id = ?1",
            [id.0],
            |r| r.get(0),
        )
        .optional()?
        .flatten())
}

/// A page of the source's files in index order with matched roms, candidates
/// and downloads, and the total `query` selects.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn files(
    conn: &Connection,
    id: SourceId,
    query: &FileQuery,
    limit: u32,
    offset: u32,
) -> Result<(Vec<FileRow>, u64)> {
    let platform = platform_of(conn, id)?;
    let mut filter = String::new();
    if let Some(f) = query.filter {
        filter = format!(" AND {}", f.clause());
    }
    let q = query
        .q
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty())
        .map(str::to_ascii_lowercase);
    let search = "(?2 IS NULL OR instr(lower(f.path), ?2) > 0)";
    let total = conn.query_row(
        &format!(
            "SELECT COUNT(*) FROM torrent_files f WHERE f.source_id = ?1 AND {search}{filter}"
        ),
        params![id.0, q],
        |r| uint(r, 0),
    )?;
    let mut stmt = conn.prepare(&format!(
        "SELECT f.file_index, f.path, f.size, f.rom_id, r.name, r.title_id, f.confidence,
                d.id, d.state, d.progress
         FROM torrent_files f LEFT JOIN roms r ON r.id = f.rom_id
         LEFT JOIN downloads d ON d.id = (SELECT MAX(x.id) FROM downloads x
           WHERE x.source_id = f.source_id AND x.file_index = f.file_index)
         WHERE f.source_id = ?1 AND {search}{filter}
         ORDER BY f.file_index LIMIT ?3 OFFSET ?4"
    ))?;
    let mut rows = stmt
        .query_map(params![id.0, q, limit, offset], |r| {
            let path: String = r.get(1)?;
            let download = match r.get::<_, Option<i64>>(7)? {
                Some(id) => Some(FileDownload {
                    id,
                    state: r.get(8)?,
                    progress: r.get(9)?,
                }),
                None => None,
            };
            Ok(FileRow {
                file_index: r.get(0)?,
                kind: file_kind(&path, platform.as_deref()),
                path,
                size: uint(r, 2)?,
                rom_id: r.get(3)?,
                rom_name: r.get(4)?,
                title_id: r.get(5)?,
                confidence: r.get(6)?,
                candidates: Vec::new(),
                unmatched: None,
                download,
            })
        })?
        .collect::<rusqlite::Result<Vec<FileRow>>>()?;
    if let (Some(first), Some(last)) = (rows.first(), rows.last()) {
        let (from, to) = (first.file_index, last.file_index);
        for (index, found) in super::candidates::of_files(conn, id, from, to)? {
            if let Some(row) = rows.iter_mut().find(|r| r.file_index == index) {
                row.candidates.push(found);
            }
        }
    }
    for row in &mut rows {
        if row.rom_id.is_none() && row.candidates.is_empty() {
            row.unmatched = Some(if platform.is_none() {
                Unmatched::Unbound
            } else if row.kind == FileKind::Extra {
                Unmatched::Extra
            } else {
                Unmatched::NoEntry
            });
        }
    }
    Ok((rows, total))
}

/// How a source's files classify; the first four add up to its file count.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    /// Files with a matched rom.
    pub matched: u64,
    /// Files with only candidate roms.
    pub candidates: u64,
    /// Other files that are not [`FileKind::Extra`].
    pub unmatched: u64,
    /// Unmatched [`FileKind::Extra`] files.
    pub extra: u64,
    /// Files with a download that was not cancelled.
    pub wanted: u64,
}

/// A DAT the source's matched files come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct DatShare {
    /// The `dat_versions` row.
    pub dat_version_id: i64,
    /// Its name.
    pub dat_name: String,
    /// Its version string.
    pub version: String,
    /// Files matched to its roms.
    pub matched: u64,
}

/// The source's open downloads, which the client holds selected.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
pub struct Transfer {
    /// Files with a queued, transferring, checking or importing download.
    pub files: u64,
    /// Their size in bytes.
    pub size: u64,
    /// Bytes of them transferred.
    pub done: u64,
}

/// `GET /sources/{id}`: the list item with how its files classify.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct SourceDetail {
    /// The source as `/sources` lists it.
    #[serde(flatten)]
    pub source: SourceRow,
    /// How its files classify.
    pub summary: Summary,
    /// The DATs its matched files come from, most files first.
    pub dats: Vec<DatShare>,
    /// Its open downloads.
    pub transfer: Transfer,
}

/// The detail of one source, `None` when there is no such source.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn detail(conn: &Connection, id: SourceId) -> Result<Option<SourceDetail>> {
    let Some(source) = sources::get(conn, id)? else {
        return Ok(None);
    };
    let platform = source.platform_id.as_ref().map(|p| p.0.as_str());
    let mut summary = Summary::default();
    let mut stmt = conn.prepare(&format!(
        "SELECT f.path, f.rom_id IS NOT NULL, {HAS_MATCH} FROM torrent_files f WHERE f.source_id = ?1"
    ))?;
    let mut rows = stmt.query([id.0])?;
    while let Some(r) = rows.next()? {
        let (path, rom, any): (String, bool, bool) = (r.get(0)?, r.get(1)?, r.get(2)?);
        let slot = if rom {
            &mut summary.matched
        } else if any {
            &mut summary.candidates
        } else if file_kind(&path, platform) == FileKind::Extra {
            &mut summary.extra
        } else {
            &mut summary.unmatched
        };
        *slot += 1;
    }
    summary.wanted = conn.query_row(
        "SELECT COUNT(DISTINCT file_index) FROM downloads
         WHERE source_id = ?1 AND file_index IS NOT NULL AND state != 'cancelled'",
        [id.0],
        |r| uint(r, 0),
    )?;
    let dats = conn
        .prepare(
            "SELECT d.id, d.dat_name, d.version, COUNT(*) AS n
             FROM torrent_files f JOIN roms r ON r.id = f.rom_id
             JOIN titles t ON t.id = r.title_id JOIN dat_versions d ON d.id = t.dat_version_id
             WHERE f.source_id = ?1 GROUP BY d.id ORDER BY n DESC, d.id",
        )?
        .query_map([id.0], |r| {
            Ok(DatShare {
                dat_version_id: r.get(0)?,
                dat_name: r.get(1)?,
                version: r.get(2)?,
                matched: uint(r, 3)?,
            })
        })?
        .collect::<rusqlite::Result<_>>()?;
    let transfer = conn.query_row(
        &format!(
            "SELECT COUNT(DISTINCT d.file_index), COALESCE(SUM(f.size), 0),
                    COALESCE(SUM(CAST(d.progress * f.size AS INTEGER)), 0)
             FROM downloads d JOIN torrent_files f
               ON f.source_id = d.source_id AND f.file_index = d.file_index
             WHERE d.source_id = ?1 AND d.state IN {OPEN}"
        ),
        [id.0],
        |r| {
            Ok(Transfer {
                files: uint(r, 0)?,
                size: uint(r, 1)?,
                done: uint(r, 2)?,
            })
        },
    )?;
    Ok(Some(SourceDetail {
        source,
        summary,
        dats,
        transfer,
    }))
}

/// How many of a source's files one platform's DAT entries match by name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PlatformMatch {
    /// The platform.
    pub platform_id: PlatformId,
    /// Files its roms match by name, or by base name and size.
    pub matched: u64,
}

/// `GET /sources/{id}/preview`: what binding to each platform would match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Preview {
    /// Files in the source.
    pub total: u64,
    /// Every platform with a loaded DAT, most matched first, then by id.
    pub platforms: Vec<PlatformMatch>,
}

/// Platforms with live titles from a DAT file, by id.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn platforms_with_dat(conn: &Connection) -> Result<Vec<PlatformId>> {
    let ids = conn
        .prepare(
            "SELECT p.id FROM platforms p WHERE EXISTS (SELECT 1 FROM titles t
               WHERE t.platform_id = p.id AND t.source = 'dat' AND t.retired = 0)
             ORDER BY p.id",
        )?
        .query_map([], |r| r.get(0).map(PlatformId))?
        .collect::<rusqlite::Result<_>>()?;
    Ok(ids)
}

/// A dry run of binding's name tiers over the source's files against every
/// platform at once, as the automatic classifier scores them. Reads only.
///
/// # Errors
///
/// [`crate::Error::Db`] on SQLite failure.
pub fn preview(conn: &Connection, id: SourceId) -> Result<Preview> {
    let files = sources::torrent_files(conn, id)?;
    let total = u64::try_from(files.len()).unwrap_or(u64::MAX);
    let scores = binding::score_platforms(&files, &SqlDatIndex::new(conn));
    drop(files);
    let mut platforms: Vec<PlatformMatch> = platforms_with_dat(conn)?
        .into_iter()
        .map(|platform_id| {
            let rate = scores
                .iter()
                .find(|(p, _)| *p == platform_id)
                .map_or(0.0, |(_, r)| f64::from(*r));
            // Rates are hits over the file count, far below 2^24, so this restores the count.
            #[allow(
                clippy::cast_possible_truncation,
                clippy::cast_sign_loss,
                clippy::cast_precision_loss
            )]
            let matched = (rate * total as f64).round() as u64;
            PlatformMatch {
                platform_id,
                matched,
            }
        })
        .collect();
    platforms.sort_by(|a, b| {
        b.matched
            .cmp(&a.matched)
            .then_with(|| a.platform_id.0.cmp(&b.platform_id.0))
    });
    Ok(Preview { total, platforms })
}

#[cfg(test)]
mod tests {
    use mistarr_sources::binding::{Confidence, RomRef};
    use mistarr_sources::torrent::TorrentFile;

    use super::*;
    use crate::db::sources::fixtures::seed_rom;
    use crate::db::sources::{NewSource, SourceState};

    fn conn() -> Connection {
        let mut c = Connection::open_in_memory().expect("open");
        crate::db::migrate::apply(&mut c).expect("migrate");
        crate::db::platforms::seed(&mut c, &platforms::PLATFORMS).expect("seed");
        c
    }

    fn file(index: u32, path: &str, size: u64) -> TorrentFile {
        TorrentFile {
            index,
            path: path.to_owned(),
            size,
        }
    }

    /// A source on `nes` with a matched rom, a candidate, an unmatched rom and a readme.
    fn source(c: &Connection) -> (SourceId, i64) {
        let a = seed_rom(c, "nes", "Example Quest (USA).nes", 16, &[]).expect("rom");
        let b = seed_rom(c, "nes", "Second Try (Japan).nes", 24, &[]).expect("rom");
        let id = sources::insert(
            c,
            &NewSource {
                infohash: &"0a".repeat(20),
                display_name: "Synthetic Set",
                origin_file: "set.torrent",
                state: SourceState::Bound,
                reason: None,
                added_at: 1,
            },
        )
        .expect("insert");
        let list = [
            file(0, "Set/Example Quest (USA).nes", 16),
            file(1, "Set/Second Guess (USA).nes", 24),
            file(2, "Set/Unlisted (USA).nes", 8),
            file(3, "Set/readme.txt", 4),
        ];
        sources::replace_files(c, id, &list).expect("files");
        sources::set_binding(c, id, Some(&PlatformId("nes".into())), Some(0.25)).expect("bind");
        sources::set_matches(c, id, &[(0, Some(RomRef(a)), Confidence::Name)]).expect("m");
        c.execute(
            "INSERT INTO torrent_candidates (source_id, file_index, rom_id, confidence)
             VALUES (?1, 1, ?2, 'fuzzy')",
            params![id.0, b],
        )
        .expect("candidate");
        (id, a)
    }

    #[test]
    fn detail_counts_each_kind_of_file_and_the_dat() {
        let c = conn();
        let (id, rom) = source(&c);
        c.execute(
            "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, progress,
               created_at, updated_at)
             SELECT title_id, id, ?1, 0, 'transferring', 0.5, 0, 0 FROM roms WHERE id = ?2",
            params![id.0, rom],
        )
        .expect("download");
        let d = detail(&c, id).expect("detail").expect("some");
        assert_eq!(
            d.summary,
            Summary {
                matched: 1,
                candidates: 1,
                unmatched: 1,
                extra: 1,
                wanted: 1
            }
        );
        assert_eq!(d.dats.len(), 1);
        assert_eq!(
            (d.dats[0].dat_name.as_str(), d.dats[0].matched),
            ("nes test", 1)
        );
        assert_eq!(
            d.transfer,
            Transfer {
                files: 1,
                size: 16,
                done: 8
            }
        );
        assert!(detail(&c, SourceId(99)).expect("detail").is_none());
    }

    #[test]
    fn file_pages_filter_search_and_explain() {
        let c = conn();
        let (id, rom) = source(&c);
        let all = files(&c, id, &FileQuery::default(), 10, 0).expect("files");
        assert_eq!(all.1, 4);
        let reasons: Vec<_> = all.0.iter().map(|f| f.unmatched).collect();
        assert_eq!(
            reasons,
            [None, None, Some(Unmatched::NoEntry), Some(Unmatched::Extra)]
        );
        assert_eq!(all.0[3].kind, FileKind::Extra);
        let page = |filter, q: Option<&str>, limit, offset| {
            let query = FileQuery {
                filter,
                q: q.map(str::to_owned),
            };
            let (rows, total) = files(&c, id, &query, limit, offset).expect("files");
            (rows.iter().map(|f| f.file_index).collect::<Vec<_>>(), total)
        };
        assert_eq!(
            page(Some(FileFilter::Matched), None, 10, 0),
            (vec![0, 1], 2)
        );
        assert_eq!(page(Some(FileFilter::Unmatched), None, 1, 1), (vec![3], 2));
        assert_eq!(page(None, Some("  README "), 10, 0), (vec![3], 1));
        assert_eq!(page(Some(FileFilter::Wanted), None, 10, 0), (vec![], 0));
        c.execute(
            "INSERT INTO downloads (title_id, rom_id, source_id, file_index, state, created_at, updated_at)
             SELECT title_id, id, ?1, 0, 'queued', 0, 0 FROM roms WHERE id = ?2",
            params![id.0, rom],
        )
        .expect("download");
        assert_eq!(page(Some(FileFilter::Wanted), None, 10, 0), (vec![0], 1));
        let first = files(&c, id, &FileQuery::default(), 1, 0).expect("files").0;
        assert_eq!(
            first[0].download.as_ref().map(|d| d.state.as_str()),
            Some("queued")
        );
        sources::set_binding(&c, id, None, None).expect("unbind");
        let rows = files(&c, id, &FileQuery::default(), 10, 0)
            .expect("files")
            .0;
        assert_eq!(rows[2].unmatched, Some(Unmatched::Unbound));
    }

    #[test]
    fn preview_scores_every_platform_with_a_dat() {
        let c = conn();
        let (id, _) = source(&c);
        seed_rom(&c, "snes", "Unlisted (USA).sfc", 8, &[]).expect("rom");
        sources::refresh_match_keys(&c).expect("keys");
        let p = preview(&c, id).expect("preview");
        assert_eq!(p.total, 4);
        let got: Vec<_> = p
            .platforms
            .iter()
            .map(|m| (m.platform_id.0.as_str(), m.matched))
            .collect();
        assert_eq!(got, [("nes", 1), ("snes", 1)]);
        assert_eq!(
            platforms_with_dat(&c).expect("dat"),
            [PlatformId("nes".into()), PlatformId("snes".into())]
        );
    }
}
