//! The Catalog routes of `docs/API.md`, with art URLs per "Art URLs".

use std::fmt::Write as _;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::rejection::{PathRejection, QueryRejection};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::routing::{get, post};
use axum::{Json, Router};
use mistarr_mister::platforms::Kind;
use serde::{Deserialize, Serialize};

use super::{ApiError, Page, Paging};
use crate::app::AppState;
use crate::db::files::FileId;
use crate::db::titles::{
    self, Browse, GroupDetail, GroupRow, RomsetState, Sort, TitleId, Tri, WantRefused,
};
use crate::db::{downloads, platforms};
use crate::jobs::import::{self, RenameError};
use crate::jobs::transfer;

/// The libretro thumbnail server, the one external URL family the app names.
const THUMBNAILS: &str = "https://thumbnails.libretro.com";

/// Characters libretro replaces with `_` in thumbnail file names.
const SUBSTITUTED: &[char] = &['&', '*', '/', ':', '`', '<', '>', '?', '\\', '|', '"'];

pub(super) fn routes() -> Router<Arc<AppState>> {
    Router::new()
        .route("/platforms/{id}/titles", get(list))
        .route("/titles/{id}", get(detail))
        .route("/titles/{id}/want", post(want).delete(unwant))
        .route("/titles/{id}/rename", post(rename))
}

/// Box art, title screen and in-game snap URLs for one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Art {
    /// `Named_Boxarts`.
    pub boxart: String,
    /// `Named_Titles`.
    pub title: String,
    /// `Named_Snaps`.
    pub snap: String,
}

/// Art URLs for DAT name `name` on the platform with libretro playlist `playlist`.
///
/// ```
/// let art = mistarr_server::http::catalog::art("Maker - Console", "A/B: C (USA)");
/// assert_eq!(art.boxart,
///     "https://thumbnails.libretro.com/Maker%20-%20Console/Named_Boxarts/A_B_%20C%20%28USA%29.png");
/// ```
#[must_use]
pub fn art(playlist: &str, name: &str) -> Art {
    let file: String = name
        .chars()
        .map(|c| if SUBSTITUTED.contains(&c) { '_' } else { c })
        .collect();
    let (playlist, file) = (encode(playlist), encode(&file));
    let url = |kind: &str| format!("{THUMBNAILS}/{playlist}/{kind}/{file}.png");
    Art {
        boxart: url("Named_Boxarts"),
        title: url("Named_Titles"),
        snap: url("Named_Snaps"),
    }
}

/// Percent-encodes everything but RFC 3986 unreserved characters.
fn encode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'.' | b'_' | b'~') {
            out.push(char::from(b));
        } else {
            let _ = write!(out, "%{b:02X}");
        }
    }
    out
}

fn platform_art(platform: &str, name: &str) -> Option<Art> {
    mistarr_mister::platforms::by_id(platform).map(|p| art(p.libretro_playlist, name))
}

/// `GET /platforms/{id}/titles` query.
#[derive(Debug, Default, Deserialize)]
struct ListQuery {
    q: Option<String>,
    have: Option<String>,
    wanted: Option<String>,
    region: Option<String>,
    flags: Option<String>,
    hidden: Option<String>,
    sort: Option<String>,
    limit: Option<u32>,
    offset: Option<u32>,
}

fn tri(name: &str, v: Option<&str>) -> Result<Tri, ApiError> {
    match v.unwrap_or("any") {
        "any" | "" => Ok(Tri::Any),
        "yes" | "true" => Ok(Tri::Yes),
        "no" | "false" => Ok(Tri::No),
        other => Err(ApiError::bad_request(format!(
            "{name} must be yes, no or any, not {other:?}"
        ))),
    }
}

impl ListQuery {
    fn browse(&self, hide: &[String]) -> Result<Browse, ApiError> {
        let sort = match self.sort.as_deref().unwrap_or("name") {
            "name" | "" => Sort::Name,
            "have" => Sort::Have,
            "recent" => Sort::Recent,
            other => {
                return Err(ApiError::bad_request(format!(
                    "sort must be name, have or recent, not {other:?}"
                )))
            }
        };
        let flags: Vec<String> = self
            .flags
            .as_deref()
            .unwrap_or("")
            .split(',')
            .map(|f| f.trim().to_ascii_lowercase())
            .filter(|f| !f.is_empty())
            .collect();
        // `hidden` disables the hide list; it never unhides based on `flags`.
        let hidden = match self.hidden.as_deref().unwrap_or("hide") {
            "hide" | "" => hide.to_vec(),
            "show" => Vec::new(),
            other => {
                return Err(ApiError::bad_request(format!(
                    "hidden must be hide or show, not {other:?}"
                )))
            }
        };
        Ok(Browse {
            q: self.q.clone(),
            have: tri("have", self.have.as_deref())?,
            wanted: tri("wanted", self.wanted.as_deref())?,
            region: self.region.clone().filter(|r| !r.trim().is_empty()),
            flags,
            hidden,
            sort,
        })
    }
}

/// A browse row with its art.
#[derive(Debug, Serialize)]
struct GroupOut {
    #[serde(flatten)]
    row: GroupRow,
    art: Option<Art>,
}

async fn list(
    State(app): State<Arc<AppState>>,
    id: Result<Path<String>, PathRejection>,
    query: Result<Query<ListQuery>, QueryRejection>,
) -> Result<Json<Page<GroupOut>>, ApiError> {
    let Path(id) = id.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let Query(query) = query.map_err(|e| ApiError::bad_request(e.body_text()))?;
    let filter = query.browse(&app.config().prefs.hide)?;
    let (limit, offset) = Paging {
        limit: query.limit,
        offset: query.offset,
    }
    .resolve();
    let (items, total) = app
        .db
        .read(move |c| {
            if platforms::get(c, &id)?.is_none() {
                return Ok(None);
            }
            titles::browse(c, &id, &filter, limit, offset).map(Some)
        })
        .await?
        .ok_or_else(|| ApiError::not_found("no such platform"))?;
    let items = items
        .into_iter()
        .map(|row| {
            let name = row.pick_name.as_deref().unwrap_or(&row.name);
            let art = platform_art(&row.platform_id.0, name);
            GroupOut { row, art }
        })
        .collect();
    Ok(Json(Page { items, total }))
}

/// A clone group with the art of its pick, or of its parent when nothing is selectable.
#[derive(Debug, Serialize)]
struct DetailOut {
    #[serde(flatten)]
    detail: GroupDetail,
    art: Option<Art>,
    /// BIOS files the Neo Geo core's `romsets.xml` names, for a Neo Geo group.
    #[serde(skip_serializing_if = "Option::is_none")]
    bios: Option<Vec<BiosFile>>,
}

/// A BIOS file a core names, reported as present or missing and never handled.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
struct BiosFile {
    name: String,
    present: bool,
}

/// Fills each Neo Geo variant's romset state from `games/NeoGeo` and its `romsets.xml`,
/// and returns the BIOS files that file names.
fn neogeo_romsets(dir: &std::path::Path, detail: &mut GroupDetail) -> Option<Vec<BiosFile>> {
    use mistarr_mister::adapter::neogeo::{read_romsets, romset_on_disk};
    let romsets = read_romsets(dir).unwrap_or_else(|e| {
        tracing::warn!(error = %e, "cannot read romsets.xml");
        None
    });
    for v in &mut detail.variants {
        v.romset = Some(RomsetState {
            listed: romsets.as_ref().map(|r| r.sets.contains(&v.name)),
            present: romset_on_disk(dir, &v.name),
        });
    }
    romsets.map(|r| {
        r.bios
            .into_iter()
            .map(|name| BiosFile {
                present: dir.join(&name).is_file(),
                name,
            })
            .collect()
    })
}

fn title_id(id: Result<Path<i64>, PathRejection>) -> Result<TitleId, ApiError> {
    id.map(|Path(id)| TitleId(id))
        .map_err(|e| ApiError::bad_request(e.body_text()))
}

async fn load_detail(app: &AppState, id: TitleId) -> Result<DetailOut, ApiError> {
    let detail = app
        .db
        .read(move |c| titles::group_detail(c, id))
        .await?
        .ok_or_else(|| ApiError::not_found("no such title"))?;
    let (detail, bios) = match mistarr_mister::platforms::by_id(&detail.platform_id.0) {
        Some(p) if p.kind == Kind::Romset => {
            let dir = app.config().paths.games.join(p.core_dir);
            let mut detail = detail;
            tokio::task::spawn_blocking(move || {
                let bios = neogeo_romsets(&dir, &mut detail);
                (detail, bios)
            })
            .await
            .map_err(|e| crate::Error::Task(e.to_string()))?
        }
        _ => (detail, None),
    };
    let shown = detail.pick_variant_id.unwrap_or(detail.parent_id);
    let art = detail
        .variants
        .iter()
        .find(|v| v.id == shown)
        .and_then(|v| platform_art(&detail.platform_id.0, &v.name));
    Ok(DetailOut { detail, art, bios })
}

async fn detail(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<DetailOut>, ApiError> {
    Ok(Json(load_detail(&app, title_id(id)?).await?))
}

/// `POST /titles/{id}/want` body.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct WantBody {
    variant_id: Option<i64>,
}

async fn want(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
    body: Bytes,
) -> Result<Json<DetailOut>, ApiError> {
    let id = title_id(id)?;
    let body: WantBody = if body.iter().all(u8::is_ascii_whitespace) {
        WantBody::default()
    } else {
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?
    };
    let current = load_detail(&app, id).await?;
    let target = match body.variant_id.map(TitleId) {
        Some(v) if current.detail.variants.iter().any(|x| x.id == v) => v,
        Some(_) => return Err(ApiError::bad_request("variant_id is not in this group")),
        None => current.detail.pick_variant_id.ok_or_else(|| {
            ApiError::bad_request("no variant is selectable under the current preferences")
        })?,
    };
    let result = app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            let wanted = titles::want(&tx, target)?;
            let created = match wanted {
                Ok(()) => downloads::want_title(&tx, target, crate::unix_now())?,
                Err(_) => Vec::new(),
            };
            crate::db::commit(tx)?;
            Ok(wanted.map(|()| created))
        })
        .await?;
    match result {
        Ok(created) => {
            for (id, state) in created {
                transfer::publish(&app, id, state, 0.0);
            }
            transfer::kick(&app).await;
        }
        Err(WantRefused::Missing) => return Err(ApiError::not_found("no such title")),
        Err(WantRefused::Retired) => {
            return Err(ApiError::bad_request("the entry is retired from its DAT"))
        }
        Err(WantRefused::Bios) => {
            return Err(ApiError::bad_request("BIOS entries cannot be wanted"))
        }
    }
    Ok(Json(load_detail(&app, id).await?))
}

async fn unwant(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
) -> Result<Json<DetailOut>, ApiError> {
    let id = title_id(id)?;
    let current = load_detail(&app, id).await?;
    let group = current.detail.parent_id;
    let cancelled = app
        .db
        .write(move |c| {
            let tx = c.transaction()?;
            let now = crate::unix_now();
            let cancelled = downloads::cancel_group(&tx, group, now)?;
            titles::unwant_group(&tx, group, now)?;
            crate::db::commit(tx)?;
            Ok(cancelled)
        })
        .await?;
    super::downloads::after_cancel(&app, &cancelled).await;
    Ok(Json(load_detail(&app, id).await?))
}

/// `POST /titles/{id}/rename` body.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RenameBody {
    file_id: i64,
}

async fn rename(
    State(app): State<Arc<AppState>>,
    id: Result<Path<i64>, PathRejection>,
    body: Bytes,
) -> Result<Json<DetailOut>, ApiError> {
    let id = title_id(id)?;
    let body: RenameBody =
        serde_json::from_slice(&body).map_err(|e| ApiError::bad_request(e.to_string()))?;
    let group = load_detail(&app, id).await?.detail.parent_id;
    match import::rename(&app, group, FileId(body.file_id)).await {
        Ok(_) => Ok(Json(load_detail(&app, id).await?)),
        Err(RenameError::NotFound) => Err(ApiError::not_found("no such file in this title")),
        Err(RenameError::Conflict(path)) => Err(ApiError::new(
            StatusCode::CONFLICT,
            "conflict",
            format!("{path} already exists"),
        )),
        Err(RenameError::Server(e)) => Err(e.into()),
        Err(RenameError::Io(message)) => Err(ApiError::new(
            StatusCode::INTERNAL_SERVER_ERROR,
            "internal",
            message,
        )),
        Err(e) => Err(ApiError::bad_request(e.to_string())),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn art_substitutes_and_encodes() {
        let a = art("Maker - Console", r#"A&B*C/D:E`F<G>H?I\J|K"L (Japan)"#);
        assert_eq!(
            a.title,
            "https://thumbnails.libretro.com/Maker%20-%20Console/Named_Titles/A_B_C_D_E_F_G_H_I_J_K_L%20%28Japan%29.png"
        );
        assert!(a.snap.contains("/Named_Snaps/"));
        assert_eq!(encode("é~"), "%C3%A9~");
    }

    #[test]
    fn platform_art_uses_the_playlist_name() {
        let a = platform_art("gb", "Example Quest (USA)").expect("art");
        assert!(a.boxart.contains("/Nintendo%20-%20Game%20Boy/"));
        assert!(platform_art("nope", "x").is_none());
    }

    #[test]
    fn list_query_parses_filters() {
        let q = ListQuery {
            have: Some("yes".into()),
            wanted: Some("false".into()),
            flags: Some("Beta, proto".into()),
            sort: Some("recent".into()),
            region: Some(" ".into()),
            ..ListQuery::default()
        };
        let hide = ["bios".to_owned(), "beta".to_owned()];
        let b = q.browse(&hide).expect("browse");
        assert_eq!(
            (b.have, b.wanted, b.sort),
            (Tri::Yes, Tri::No, Sort::Recent)
        );
        assert_eq!(b.flags, ["beta", "proto"]);
        assert_eq!(b.hidden, hide, "flags never unhide");
        assert_eq!(b.region, None);
        let bad = ListQuery {
            sort: Some("size".into()),
            ..ListQuery::default()
        };
        assert!(bad.browse(&hide).is_err());
        assert!(tri("have", Some("maybe")).is_err());
    }

    #[test]
    fn hidden_show_disables_the_hide_list() {
        let hide = ["bios".to_owned()];
        let show = ListQuery {
            hidden: Some("show".into()),
            ..ListQuery::default()
        };
        assert!(show.browse(&hide).expect("browse").hidden.is_empty());
        let hide_q = ListQuery {
            hidden: Some("hide".into()),
            ..ListQuery::default()
        };
        assert_eq!(hide_q.browse(&hide).expect("browse").hidden, hide);
        let default_q = ListQuery::default();
        assert_eq!(default_q.browse(&hide).expect("browse").hidden, hide);
        let bad = ListQuery {
            hidden: Some("maybe".into()),
            ..ListQuery::default()
        };
        assert!(bad.browse(&hide).is_err());
    }
}
