//! `mistarr.toml`; the fields and defaults are in `docs/ARCHITECTURE.md` "Configuration".

use std::path::{Path, PathBuf};

use mistarr_clients::{ClientKind, PathMapping};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};

/// File name of the config inside the data directory.
pub const CONFIG_FILE: &str = "mistarr.toml";

/// Default data directory on the SD card.
pub const DEFAULT_DATA_DIR: &str = "/media/fat/mistarr";

/// The whole config file. Every field has a default, so an empty file is valid.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    /// `[server]`.
    pub server: ServerConfig,
    /// `[paths]`.
    pub paths: PathsConfig,
    /// `[client]`.
    pub client: ClientConfig,
    /// `[limits]`.
    pub limits: LimitsConfig,
    /// `[transfer]`.
    pub transfer: TransferConfig,
    /// `[prefs]`.
    pub prefs: PrefsConfig,
    /// `[sources]`.
    pub sources: SourcesConfig,
    /// `[jobs]`.
    pub jobs: JobsConfig,
    /// `[memory]`.
    pub memory: MemoryConfig,
    /// `[scan]`.
    pub scan: ScanConfig,
}

/// `[scan]`: how the library scan identifies files.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ScanConfig {
    /// Decode CHD images to hash their tracks; slow on the board, so off by default.
    pub chd_tracks: bool,
}

/// `[memory]`: the ceiling that keeps a runaway allocation from taking the board down,
/// and where and when a DAT import runs on a copy of the database in RAM.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct MemoryConfig {
    /// Soft `RLIMIT_DATA` in MiB, set at startup; 0 leaves the inherited limit.
    pub data_limit_mib: u64,
    /// The RAM-backed directory a DAT import copies the database into.
    pub import_dir: PathBuf,
    /// Memory, in MiB, a DAT import in RAM leaves available beyond the copy it needs.
    pub import_floor_mib: u64,
}

impl Default for MemoryConfig {
    /// Three times the 64 MiB peak budget; the copy beside SQLite's temporary files, and
    /// room kept for MiSTer Main and a running core.
    fn default() -> Self {
        Self {
            data_limit_mib: 192,
            import_dir: PathBuf::from(crate::db::RAM_TEMP_DIR),
            import_floor_mib: 128,
        }
    }
}

/// `[sources]`: how a dropped source is bound to a platform.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct SourcesConfig {
    /// Lowest per-platform hit rate, 0 to 1, that binds a source.
    pub bind_threshold: f32,
}

impl Default for SourcesConfig {
    fn default() -> Self {
        Self {
            bind_threshold: 0.6,
        }
    }
}

/// `[jobs]`: scheduled background work.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct JobsConfig {
    /// How often a full library scan is enqueued; 0 means manual only.
    pub scan_interval_minutes: u32,
}

impl Default for JobsConfig {
    /// A daily rescan by default, 0 disables it.
    fn default() -> Self {
        Self {
            scan_interval_minutes: 1440,
        }
    }
}

/// `[server]`: where to listen and whether to require an API key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ServerConfig {
    /// Socket address for the HTTP server.
    pub listen: String,
    /// Value required in `X-Api-Key`; empty leaves the API open on the LAN.
    pub api_key: String,
    /// Host names, beyond the built-in ones, that state-changing requests may
    /// address; `*.name` allows every subdomain. See `docs/API.md`.
    pub allowed_hosts: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            listen: "0.0.0.0:8420".to_owned(),
            api_key: String::new(),
            allowed_hosts: Vec::new(),
        }
    }
}

/// `[paths]`: the SD card root, the games tree and mistarr's own directory.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PathsConfig {
    /// SD card root holding `_Console` and friends.
    pub root: PathBuf,
    /// The `games/` tree cores read from.
    pub games: PathBuf,
    /// Database, log, `dats/`, `sources/` and `staging/`.
    pub data: PathBuf,
}

impl Default for PathsConfig {
    fn default() -> Self {
        Self {
            root: PathBuf::from("/media/fat"),
            games: PathBuf::from("/media/fat/games"),
            data: PathBuf::from(DEFAULT_DATA_DIR),
        }
    }
}

impl PathsConfig {
    /// The SQLite database file.
    ///
    /// ```
    /// let p = mistarr_server::config::PathsConfig::default();
    /// assert!(p.db().ends_with("mistarr.db"));
    /// ```
    #[must_use]
    pub fn db(&self) -> PathBuf {
        self.data.join("mistarr.db")
    }

    /// Where SQLite writes its temporary files when `db::RAM_TEMP_DIR` cannot be written.
    ///
    /// ```
    /// assert!(mistarr_server::config::PathsConfig::default().tmp().ends_with("tmp"));
    /// ```
    #[must_use]
    pub fn tmp(&self) -> PathBuf {
        self.data.join("tmp")
    }

    /// The log file; rotated copies sit beside it as `.1` and `.2`.
    ///
    /// ```
    /// let p = mistarr_server::config::PathsConfig::default();
    /// assert!(p.log().ends_with("mistarr.log"));
    /// ```
    #[must_use]
    pub fn log(&self) -> PathBuf {
        self.data.join("mistarr.log")
    }

    /// Watched directory for DATs.
    ///
    /// ```
    /// assert!(mistarr_server::config::PathsConfig::default().dats().ends_with("dats"));
    /// ```
    #[must_use]
    pub fn dats(&self) -> PathBuf {
        self.data.join("dats")
    }

    /// Watched directory for `.torrent` and `.magnet` files.
    ///
    /// ```
    /// assert!(mistarr_server::config::PathsConfig::default().sources().ends_with("sources"));
    /// ```
    #[must_use]
    pub fn sources(&self) -> PathBuf {
        self.data.join("sources")
    }

    /// The client's download directory.
    ///
    /// ```
    /// assert!(mistarr_server::config::PathsConfig::default().staging().ends_with("staging"));
    /// ```
    #[must_use]
    pub fn staging(&self) -> PathBuf {
        self.data.join("staging")
    }

    /// Every directory of the layout in `docs/DEPLOYMENT.md`, parents first.
    ///
    /// ```
    /// let dirs = mistarr_server::config::PathsConfig::default().layout();
    /// assert!(dirs.iter().any(|d| d.ends_with("staging/quarantine")));
    /// ```
    #[must_use]
    pub fn layout(&self) -> Vec<PathBuf> {
        let (dats, sources, staging) = (self.dats(), self.sources(), self.staging());
        vec![
            self.data.clone(),
            dats.join("loaded"),
            dats.join("rejected"),
            sources.join("loaded"),
            sources.join("rejected"),
            staging.join("quarantine"),
        ]
    }
}

/// `client.kind`: a specific client or automatic detection.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ClientChoice {
    /// Probe in the order of `docs/DOWNLOAD-CLIENTS.md`.
    #[default]
    Auto,
    /// transmission-daemon.
    Transmission,
    /// rtorrent.
    Rtorrent,
}

impl ClientChoice {
    /// The concrete kind, or `None` for automatic detection.
    ///
    /// ```
    /// use mistarr_server::config::ClientChoice;
    /// assert!(ClientChoice::Auto.kind().is_none());
    /// ```
    #[must_use]
    pub fn kind(self) -> Option<ClientKind> {
        match self {
            Self::Auto => None,
            Self::Transmission => Some(ClientKind::Transmission),
            Self::Rtorrent => Some(ClientKind::Rtorrent),
        }
    }
}

/// `[client]`: which torrent client and how to reach it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClientConfig {
    /// Which client to use.
    pub kind: ClientChoice,
    /// Transmission RPC URL or rtorrent SCGI address; empty means the defaults.
    pub url: String,
    /// Prefix pairs for a client that sees the files under other paths.
    pub remote_path_map: Vec<PathMapping>,
}

/// `[limits]`: client rate limits in kbps, for the menu and while a core runs. 0 is unlimited.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct LimitsConfig {
    /// Download limit at the menu.
    pub down_kbps_menu: u32,
    /// Download limit while a core runs.
    pub down_kbps_core: u32,
    /// Upload limit at the menu.
    pub up_kbps_menu: u32,
    /// Upload limit while a core runs.
    pub up_kbps_core: u32,
}

impl Default for LimitsConfig {
    fn default() -> Self {
        Self {
            down_kbps_menu: 0,
            down_kbps_core: 512,
            up_kbps_menu: 0,
            up_kbps_core: 64,
        }
    }
}

/// `[transfer]`: what the download client may do while a core runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct TransferConfig {
    /// Stop a client on the board while a core other than the menu runs, or
    /// hold the uploads of one on another machine.
    pub pause_client_while_playing: bool,
}

impl Default for TransferConfig {
    fn default() -> Self {
        Self {
            pause_client_while_playing: true,
        }
    }
}

/// `[prefs]`: 1G1R preferences, the flags hidden by default and whether games may be launched.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct PrefsConfig {
    /// Region order, most preferred first.
    pub regions: Vec<String>,
    /// Language order, most preferred first.
    pub languages: Vec<String>,
    /// Prefer the highest revision within a region.
    pub prefer_latest_revision: bool,
    /// DAT flags hidden in the catalog.
    pub hide: Vec<String>,
    /// Whether the API may start cores and games through MiSTer Main.
    pub launch: bool,
}

impl PrefsConfig {
    /// Whether `other` picks and hides the same entries, ignoring settings that do not touch 1G1R.
    ///
    /// ```
    /// use mistarr_server::config::PrefsConfig;
    /// let a = PrefsConfig::default();
    /// assert!(a.same_selection(&PrefsConfig { launch: false, ..a.clone() }));
    /// assert!(!a.same_selection(&PrefsConfig { hide: vec![], ..a.clone() }));
    /// ```
    #[must_use]
    pub fn same_selection(&self, other: &Self) -> bool {
        self.regions == other.regions
            && self.languages == other.languages
            && self.prefer_latest_revision == other.prefer_latest_revision
            && self.hide == other.hide
    }
}

impl Default for PrefsConfig {
    fn default() -> Self {
        let owned = |xs: &[&str]| xs.iter().map(|s| (*s).to_owned()).collect();
        Self {
            regions: owned(&["USA", "World", "Europe", "Japan"]),
            languages: owned(&["En"]),
            prefer_latest_revision: true,
            hide: owned(&["bios", "beta", "proto", "demo", "sample", "program"]),
            launch: true,
        }
    }
}

/// The part of the config the API may change at runtime; stored in `settings`
/// and laid over the file on every start. Missing fields take their defaults.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct RuntimeSettings {
    /// `[client]`.
    pub client: ClientConfig,
    /// `[limits]`.
    pub limits: LimitsConfig,
    /// `[prefs]`.
    pub prefs: PrefsConfig,
    /// `[scan]`; absent in settings saved before it existed, which then keep the file's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub scan: Option<ScanConfig>,
    /// `[transfer]`; absent in settings saved before it existed, which then keep the file's.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub transfer: Option<TransferConfig>,
}

/// A partial [`RuntimeSettings`], as accepted by `PUT /system/settings`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SettingsPatch {
    /// Replaces `[client]` when present.
    pub client: Option<ClientConfig>,
    /// Replaces `[limits]` when present.
    pub limits: Option<LimitsConfig>,
    /// Replaces `[prefs]` when present.
    pub prefs: Option<PrefsConfig>,
    /// Replaces `[scan]` when present.
    pub scan: Option<ScanConfig>,
    /// Replaces `[transfer]` when present.
    pub transfer: Option<TransferConfig>,
}

impl Config {
    /// Parses TOML text; absent fields take their defaults.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when the text is not valid TOML or a value has the wrong type.
    ///
    /// ```
    /// let c = mistarr_server::config::Config::parse("[server]\nlisten = \"127.0.0.1:1\"").unwrap();
    /// assert_eq!(c.server.listen, "127.0.0.1:1");
    /// assert_eq!(c.limits.down_kbps_core, 512);
    /// ```
    pub fn parse(text: &str) -> Result<Self> {
        toml::from_str(text).map_err(|e| Error::Config(e.to_string()))
    }

    /// Loads `explicit` if given, which must exist, else `<data>/mistarr.toml`
    /// if present, else defaults. `data` then overrides `paths.data`.
    ///
    /// # Errors
    ///
    /// [`Error::Config`] when a file exists but cannot be read or parsed, or
    /// `explicit` does not exist.
    ///
    /// ```
    /// use std::path::Path;
    /// let dir = std::env::temp_dir().join("mistarr-doc-config-load");
    /// let c = mistarr_server::config::Config::load(None, Some(&dir)).unwrap();
    /// assert_eq!(c.paths.data, dir);
    /// ```
    pub fn load(explicit: Option<&Path>, data: Option<&Path>) -> Result<Self> {
        let default_data = PathBuf::from(DEFAULT_DATA_DIR);
        let file = match explicit {
            Some(p) => Some(p.to_path_buf()),
            None => Some(data.unwrap_or(&default_data).join(CONFIG_FILE)).filter(|p| p.exists()),
        };
        let mut config = match file {
            Some(path) => {
                let text = std::fs::read_to_string(&path)
                    .map_err(|e| Error::Config(format!("{}: {e}", path.display())))?;
                Self::parse(&text).map_err(|e| Error::Config(format!("{}: {e}", path.display())))?
            }
            None => Self::default(),
        };
        if let Some(d) = data {
            config.paths.data = d.to_path_buf();
        }
        Ok(config)
    }

    /// The runtime-editable subset.
    ///
    /// ```
    /// let c = mistarr_server::config::Config::default();
    /// assert_eq!(c.runtime().limits, c.limits);
    /// ```
    #[must_use]
    pub fn runtime(&self) -> RuntimeSettings {
        RuntimeSettings {
            client: self.client.clone(),
            limits: self.limits,
            prefs: self.prefs.clone(),
            scan: Some(self.scan),
            transfer: Some(self.transfer),
        }
    }

    /// Lays saved runtime settings over this config; `[scan]` and `[transfer]` only
    /// when they carry them.
    ///
    /// ```
    /// use mistarr_server::config::{Config, RuntimeSettings, ScanConfig};
    /// let mut c = Config { scan: ScanConfig { chd_tracks: true }, ..Config::default() };
    /// c.overlay(RuntimeSettings::default());
    /// assert!(c.scan.chd_tracks);
    /// ```
    pub fn overlay(&mut self, runtime: RuntimeSettings) {
        self.client = runtime.client;
        self.limits = runtime.limits;
        self.prefs = runtime.prefs;
        if let Some(scan) = runtime.scan {
            self.scan = scan;
        }
        if let Some(transfer) = runtime.transfer {
            self.transfer = transfer;
        }
    }

    /// Replaces each section the patch carries.
    ///
    /// ```
    /// use mistarr_server::config::{Config, LimitsConfig, SettingsPatch};
    /// let mut c = Config::default();
    /// let limits = LimitsConfig { up_kbps_core: 1, ..LimitsConfig::default() };
    /// c.apply(&SettingsPatch { limits: Some(limits), ..SettingsPatch::default() });
    /// assert_eq!(c.limits.up_kbps_core, 1);
    /// ```
    pub fn apply(&mut self, patch: &SettingsPatch) {
        if let Some(client) = &patch.client {
            self.client.clone_from(client);
        }
        if let Some(limits) = patch.limits {
            self.limits = limits;
        }
        if let Some(prefs) = &patch.prefs {
            self.prefs.clone_from(prefs);
        }
        if let Some(scan) = patch.scan {
            self.scan = scan;
        }
        if let Some(transfer) = patch.transfer {
            self.transfer = transfer;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_file_is_all_defaults() {
        assert_eq!(Config::parse("").expect("parse"), Config::default());
        let c = Config::default();
        assert_eq!(c.server.listen, "0.0.0.0:8420");
        assert!(c.server.api_key.is_empty());
        assert_eq!(c.paths.games, Path::new("/media/fat/games"));
        assert_eq!(c.client.kind, ClientChoice::Auto);
        assert_eq!(c.prefs.regions[0], "USA");
        assert!(c.prefs.prefer_latest_revision);
        assert!(c.prefs.launch);
        assert_eq!(c.jobs.scan_interval_minutes, 1440);
    }

    #[test]
    fn memory_limit_defaults_to_192_mib_and_is_configurable() {
        assert_eq!(Config::default().memory.data_limit_mib, 192);
        let c = Config::parse("[memory]\ndata_limit_mib = 0").expect("parse");
        assert_eq!(
            c.memory,
            MemoryConfig {
                data_limit_mib: 0,
                ..MemoryConfig::default()
            }
        );
    }

    #[test]
    fn the_import_in_ram_settings_default_beside_the_temp_dir() {
        let m = Config::default().memory;
        assert_eq!(m.import_dir, Path::new(crate::db::RAM_TEMP_DIR));
        assert_eq!(m.import_floor_mib, 128);
        let c = Config::parse("[memory]\nimport_dir = \"/run/x\"\nimport_floor_mib = 9")
            .expect("parse");
        assert_eq!(c.memory.import_dir, Path::new("/run/x"));
        assert_eq!(c.memory.import_floor_mib, 9);
    }

    #[test]
    fn jobs_interval_is_configurable_and_zero_disables_it() {
        let c = Config::parse("[jobs]\nscan_interval_minutes = 0").expect("parse");
        assert_eq!(c.jobs.scan_interval_minutes, 0);
        let c = Config::parse("[jobs]\nscan_interval_minutes = 60").expect("parse");
        assert_eq!(c.jobs.scan_interval_minutes, 60);
    }

    #[test]
    fn full_example_parses() {
        let text = r#"
            [server]
            listen = "127.0.0.1:9000"
            api_key = "k"
            [paths]
            root = "/r"
            games = "/r/games"
            data = "/r/m"
            [client]
            kind = "rtorrent"
            url = "127.0.0.1:5000"
            remote_path_map = [{ remote = "/downloads", local = "/r/m/staging" }]
            [limits]
            down_kbps_core = 1
            [prefs]
            regions = ["Europe"]
            launch = false
            [sources]
            bind_threshold = 0.8
        "#;
        let c = Config::parse(text).expect("parse");
        assert_eq!(c.server.api_key, "k");
        assert_eq!(c.client.kind.kind(), Some(ClientKind::Rtorrent));
        assert_eq!(c.client.remote_path_map.len(), 1);
        assert_eq!(c.limits.down_kbps_core, 1);
        assert_eq!(c.limits.up_kbps_core, 64);
        assert_eq!(c.prefs.regions, ["Europe"]);
        assert_eq!(c.prefs.languages, ["En"]);
        assert!(!c.prefs.launch);
        assert!((c.sources.bind_threshold - 0.8).abs() < f32::EPSILON);
        assert!((Config::default().sources.bind_threshold - 0.6).abs() < f32::EPSILON);
        assert_eq!(c.paths.db(), Path::new("/r/m/mistarr.db"));
    }

    #[test]
    fn bad_values_are_config_errors() {
        assert!(matches!(
            Config::parse("[client]\nkind = \"other\""),
            Err(Error::Config(_))
        ));
        assert!(matches!(Config::parse("[[["), Err(Error::Config(_))));
    }

    #[test]
    fn load_prefers_explicit_then_data_dir() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = dir.path().join("data");
        std::fs::create_dir_all(&data).expect("mkdir");
        std::fs::write(data.join(CONFIG_FILE), "[limits]\nup_kbps_menu = 7\n").expect("write");
        let explicit = dir.path().join("other.toml");
        std::fs::write(&explicit, "[limits]\nup_kbps_menu = 9\n").expect("write");

        let c = Config::load(None, Some(&data)).expect("load");
        assert_eq!(c.limits.up_kbps_menu, 7);
        assert_eq!(c.paths.data, data);
        let c = Config::load(Some(&explicit), Some(&data)).expect("load");
        assert_eq!(c.limits.up_kbps_menu, 9);
        assert!(Config::load(Some(&dir.path().join("missing.toml")), None).is_err());
        let c = Config::load(None, Some(&dir.path().join("empty"))).expect("load");
        assert_eq!(c.limits, LimitsConfig::default());
    }

    #[test]
    fn runtime_and_patch_round_trip() {
        let mut c = Config::default();
        let patch: SettingsPatch =
            serde_json::from_str(r#"{"prefs":{"regions":["Japan"]}}"#).expect("json");
        c.apply(&patch);
        assert_eq!(c.runtime().prefs.regions, ["Japan"]);
        assert_eq!(c.runtime().prefs.languages, ["En"]);
        assert!(serde_json::from_str::<SettingsPatch>(r#"{"server":{}}"#).is_err());
        let partial: RuntimeSettings =
            serde_json::from_str(r#"{"limits":{"up_kbps_core":2}}"#).expect("partial");
        assert_eq!(partial.limits.up_kbps_core, 2);
        assert_eq!(partial.prefs, PrefsConfig::default());
    }

    #[test]
    fn scan_settings_overlay_only_when_saved() {
        let on = ScanConfig { chd_tracks: true };
        let off = ScanConfig::default();
        let file_on = Config::parse("[scan]\nchd_tracks = true").expect("parse");
        assert_eq!(file_on.scan, on);
        assert_eq!(Config::default().scan, off);
        let old: RuntimeSettings = serde_json::from_str(r#"{"limits":{}}"#).expect("json");
        for (file, saved, want) in [
            (on, None, on),
            (off, None, off),
            (on, Some(off), off),
            (off, Some(on), on),
        ] {
            let mut c = Config {
                scan: file,
                ..Config::default()
            };
            c.overlay(RuntimeSettings {
                scan: saved,
                ..old.clone()
            });
            assert_eq!(c.scan, want, "file {file:?}, saved {saved:?}");
        }
        let mut c = Config::default();
        c.apply(&serde_json::from_str(r#"{"scan":{"chd_tracks":true}}"#).expect("patch"));
        assert_eq!(c.runtime().scan, Some(on));
    }

    #[test]
    fn the_client_pauses_while_playing_unless_turned_off() {
        assert!(Config::default().transfer.pause_client_while_playing);
        let file_off =
            Config::parse("[transfer]\npause_client_while_playing = false").expect("parse");
        assert!(!file_off.transfer.pause_client_while_playing);
        let mut c = file_off.clone();
        c.overlay(serde_json::from_str(r#"{"limits":{}}"#).expect("old settings"));
        assert!(!c.transfer.pause_client_while_playing);
        c.apply(&serde_json::from_str(r#"{"transfer":{}}"#).expect("patch"));
        assert_eq!(c.runtime().transfer, Some(TransferConfig::default()));
    }

    #[test]
    fn layout_lists_every_directory() {
        let p = PathsConfig {
            data: PathBuf::from("/d"),
            ..PathsConfig::default()
        };
        let dirs = p.layout();
        for want in [
            "/d",
            "/d/dats/loaded",
            "/d/dats/rejected",
            "/d/sources/loaded",
            "/d/sources/rejected",
            "/d/staging/quarantine",
        ] {
            assert!(dirs.contains(&PathBuf::from(want)), "{want}");
        }
        assert_eq!(p.log(), Path::new("/d/mistarr.log"));
        assert_eq!(p.sources(), Path::new("/d/sources"));
    }
}
