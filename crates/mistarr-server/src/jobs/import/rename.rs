//! `POST /titles/{id}/rename`: a misnamed file takes its canonical name in place.

use std::fs;
use std::path::{Path, PathBuf};

use mistarr_mister::{adapter_for, DatEntry, StagedFile, StagedKind, Step};
use serde_json::json;

use super::place::{self, PlaceError};
use super::support::{dat_rom, read_head, rel_string};
use super::{stat, BIOS_REFUSED};
use crate::app::AppState;
use crate::db::files::{self, FileId, FileRow, FileState};
use crate::db::imports::{self, EntryRom, ImportAction, TitleEntry};
use crate::db::titles::TitleId;
use crate::error::Error;
use crate::events::EventKind;

/// Why a rename request was not carried out, for the API to report.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum RenameError {
    /// No such file, or it is not in the title's clone group.
    #[error("no such file in this title")]
    NotFound,
    /// The file is not `misnamed`, lives in a zip, or needs more than a rename.
    #[error("{0}")]
    Refused(String),
    /// Another file already has the canonical name.
    #[error("`{0}` already exists")]
    Conflict(String),
    /// Reading or moving the file failed.
    #[error("{0}")]
    Io(String),
    /// A server failure.
    #[error(transparent)]
    Server(#[from] Error),
}

type Outcome<T> = std::result::Result<T, RenameError>;

/// Gives a `misnamed` file of title group `group` its canonical name in place,
/// as the platform's adapter computes it, and returns the file's new path.
///
/// # Errors
///
/// [`RenameError`] naming why the file was not renamed.
pub async fn rename(app: &AppState, group: TitleId, file_id: FileId) -> Outcome<String> {
    let (file, entry, rom) = subject(app, group, file_id).await?;
    let games = app.config().paths.games;
    let to = canonical_path(&file, &entry, &rom, &games).await?;
    let (from_rel, to_rel) = (file.rel_path.clone(), rel_string(&to));
    if from_rel == to_rel {
        return Err(RenameError::Refused(
            "the file already has its canonical name".into(),
        ));
    }
    let (pid, dest) = (file.platform_id.clone(), to_rel.clone());
    let stale = app
        .db
        .read(move |c| files::find_by_path(c, &pid, &dest))
        .await?;
    let on_disk = games.join(&to);
    if stale.is_some() && on_disk.exists() {
        return Err(RenameError::Conflict(to_rel));
    }
    let (g, f, t) = (games.clone(), PathBuf::from(&from_rel), to.clone());
    let moved = tokio::task::spawn_blocking(move || {
        place::rename_in_library(&g, &f, &t).and_then(|dst| {
            fs::metadata(&dst)
                .map(|m| stat(&m))
                .map_err(|source| PlaceError::Io { path: dst, source })
        })
    })
    .await
    .map_err(|e| Error::Task(e.to_string()))?;
    let (_, mtime) = match moved {
        Ok(s) => s,
        Err(PlaceError::Exists(_)) => return Err(RenameError::Conflict(to_rel)),
        Err(e @ PlaceError::Outside(_)) => return Err(RenameError::Refused(e.to_string())),
        Err(e) => return Err(RenameError::Io(e.to_string())),
    };
    let (to_db, from_db, title) = (to_rel.clone(), from_rel, entry.id);
    let stale_id = stale.map(|s| s.id);
    app.db
        .write(move |c| {
            let tx = c.transaction()?;
            let now = crate::unix_now();
            if let Some(id) = stale_id {
                files::delete(&tx, id)?;
            }
            files::move_to(&tx, file_id, &to_db, FileState::Verified, mtime, now)?;
            let detail = json!({ "from": from_db, "rel_path": to_db, "title_id": title.0 });
            imports::log(
                &tx,
                now,
                None,
                Some(file_id.0),
                ImportAction::Renamed,
                &detail,
            )?;
            crate::db::commit(tx)?;
            Ok(())
        })
        .await?;
    app.events.publish(
        EventKind::ImportDone,
        &json!({ "title_id": entry.id.0, "file_id": file_id.0, "action": ImportAction::Renamed.as_str() }),
    );
    Ok(to_rel)
}

/// The misnamed file `file_id` of group `group` with its entry and rom.
async fn subject(
    app: &AppState,
    group: TitleId,
    file_id: FileId,
) -> Outcome<(FileRow, TitleEntry, EntryRom)> {
    let found = app
        .db
        .read(move |c| {
            let Some(file) = files::get(c, file_id)? else {
                return Ok(None);
            };
            let title = match file.rom_id {
                Some(r) => imports::title_of_rom(c, r)?,
                None => None,
            };
            let Some(title) = title else {
                return Ok(Some((file, None)));
            };
            if crate::db::titles::group_of(c, title)? != Some(group) {
                return Ok(None);
            }
            Ok(Some((file, imports::title_entry(c, title)?)))
        })
        .await?;
    let Some((file, entry)) = found else {
        return Err(RenameError::NotFound);
    };
    if file.state != FileState::Misnamed {
        return Err(RenameError::Refused(format!(
            "the file is {}, not misnamed",
            file.state.as_str()
        )));
    }
    if file.rel_path.contains('#') {
        return Err(RenameError::Refused(
            "a file inside a zip cannot be renamed in place".into(),
        ));
    }
    let entry = entry.ok_or(RenameError::NotFound)?;
    if entry.is_bios() {
        return Err(RenameError::Refused(BIOS_REFUSED.into()));
    }
    let rom = entry
        .roms
        .iter()
        .find(|r| Some(r.id) == file.rom_id)
        .cloned()
        .ok_or_else(|| RenameError::Refused("the file's rom is retired from its DAT".into()))?;
    Ok((file, entry, rom))
}

/// The library path the adapter gives `file`, refusing a plan that needs
/// more than a rename.
async fn canonical_path(
    file: &FileRow,
    entry: &TitleEntry,
    rom: &EntryRom,
    games: &Path,
) -> Outcome<PathBuf> {
    let adapter = adapter_for(&file.platform_id)
        .ok_or_else(|| RenameError::Refused("the file's platform has no adapter".into()))?;
    let current = games.join(&file.rel_path);
    let path = current.clone();
    let head = tokio::task::spawn_blocking(move || read_head(&path, None))
        .await
        .map_err(|e| Error::Task(e.to_string()))?
        .map_err(|e| RenameError::Io(format!("cannot read the file: {e}")))?;
    let staged = StagedFile {
        path: current,
        size: u64::try_from(file.size).unwrap_or(0),
        kind: StagedKind::File,
        head,
        members: Vec::new(),
    };
    let dat = DatEntry {
        name: entry.name.clone(),
        roms: vec![dat_rom(rom)],
    };
    let plan = adapter
        .plan_placement(&dat, &staged)
        .map_err(|e| RenameError::Refused(format!("cannot compute the canonical name: {e}")))?;
    let mut target = None;
    for step in &plan.steps {
        match step {
            Step::Rename { to, .. } => target = Some(to.clone()),
            Step::CreateDir { .. } => {}
            _ => {
                return Err(RenameError::Refused(
                    "the file needs more than a new name to load; import it again instead".into(),
                ))
            }
        }
    }
    target.ok_or_else(|| RenameError::Refused("the adapter gave no name".into()))
}
