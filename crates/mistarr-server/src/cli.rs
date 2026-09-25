//! Command-line flags.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;
use crate::db::titles::SearchShape;
use crate::error::Result;

/// `mistarr [--config FILE] [--data DIR] [--listen ADDR] [serve | doctor]`.
#[derive(Debug, Parser)]
#[command(
    name = "mistarr",
    version,
    about = "Verifier and organiser for MiSTer game files"
)]
pub struct Cli {
    /// Config file; defaults to `<data>/mistarr.toml` when that exists.
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,
    /// Data directory, overriding `paths.data`.
    #[arg(long, global = true, value_name = "DIR")]
    pub data: Option<PathBuf>,
    /// Listen address, overriding `server.listen`.
    #[arg(long, global = true, value_name = "ADDR")]
    pub listen: Option<String>,
    /// What to do; `serve` when omitted.
    #[command(subcommand)]
    pub command: Option<Command>,
}

/// Subcommands.
#[derive(Debug, Clone, PartialEq, Subcommand)]
pub enum Command {
    /// Run the server.
    Serve,
    /// Print the checks a bug report should include.
    Doctor {
        /// MiB of zeros hashed for the throughput line.
        #[arg(long, default_value_t = crate::doctor::DEFAULT_HASH_MIB)]
        hash_mib: u32,
        /// Recompute the browse groups first; run it while the server is stopped.
        #[arg(long)]
        rebuild_groups: bool,
    },
    /// Print the effective listen address, for `install.sh` to wait on.
    #[command(hide = true)]
    ListenAddr,
    /// Write the synthetic benchmark catalogue into a new database file.
    #[command(hide = true)]
    BenchSeed {
        /// Database file to create; must not exist.
        #[arg(long, value_name = "PATH")]
        db: PathBuf,
        /// Catalogue size; 1 is a full set of DATs.
        #[arg(long, default_value_t = 1.0)]
        scale: f64,
    },
    /// Time browse searches on a database file, opened read-only.
    #[command(hide = true)]
    BenchSearch {
        /// Database file to read.
        #[arg(long, value_name = "PATH")]
        db: PathBuf,
        /// Platform id to browse.
        #[arg(long)]
        platform: String,
        /// Search text; empty times the unsearched page.
        #[arg(long, default_value = "")]
        term: String,
        /// Timed runs per shape.
        #[arg(long, default_value_t = 20)]
        iterations: u32,
        /// Shapes to time (like, fts, fts-platform); every shape when omitted.
        #[arg(long = "shape", value_parser = parse_shape)]
        shapes: Vec<SearchShape>,
    },
}

/// A `--shape` value.
fn parse_shape(s: &str) -> std::result::Result<SearchShape, String> {
    SearchShape::from_name(s).ok_or_else(|| {
        let names: Vec<&str> = SearchShape::ALL.iter().map(|s| s.name()).collect();
        format!("expected one of {}", names.join(", "))
    })
}

impl Cli {
    /// The config after the file and the flags are applied.
    ///
    /// # Errors
    ///
    /// [`crate::Error::Config`] when the file cannot be loaded.
    ///
    /// ```
    /// use clap::Parser;
    /// let cli = mistarr_server::cli::Cli::parse_from(["mistarr", "--data", "/nonexistent-d", "--listen", "127.0.0.1:1"]);
    /// let c = cli.config().unwrap();
    /// assert_eq!(c.server.listen, "127.0.0.1:1");
    /// ```
    pub fn config(&self) -> Result<Config> {
        let mut config = Config::load(self.config.as_deref(), self.data.as_deref())?;
        if let Some(listen) = &self.listen {
            config.server.listen.clone_from(listen);
        }
        Ok(config)
    }

    /// The subcommand, defaulting to `serve`.
    ///
    /// ```
    /// use clap::Parser;
    /// use mistarr_server::cli::{Cli, Command};
    /// assert_eq!(Cli::parse_from(["mistarr"]).command(), Command::Serve);
    /// ```
    #[must_use]
    pub fn command(&self) -> Command {
        self.command.clone().unwrap_or(Command::Serve)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_parse_before_and_after_the_subcommand() {
        let cli = Cli::try_parse_from(["mistarr", "doctor", "--hash-mib", "2", "--data", "/d"])
            .expect("parse");
        assert_eq!(
            cli.command(),
            Command::Doctor {
                hash_mib: 2,
                rebuild_groups: false
            }
        );
        assert_eq!(cli.data.as_deref(), Some(std::path::Path::new("/d")));
        let cli = Cli::try_parse_from(["mistarr", "doctor", "--rebuild-groups"]).expect("parse");
        assert!(matches!(
            cli.command(),
            Command::Doctor {
                rebuild_groups: true,
                ..
            }
        ));
        let cli =
            Cli::try_parse_from(["mistarr", "--listen", "0.0.0.0:1", "serve"]).expect("parse");
        assert_eq!(cli.command(), Command::Serve);
        assert!(Cli::try_parse_from(["mistarr", "bogus"]).is_err());
    }

    #[test]
    fn bench_commands_parse_shapes_and_defaults() {
        let cli = Cli::try_parse_from([
            "mistarr",
            "bench-search",
            "--db",
            "/tmp/b.db",
            "--platform",
            "nes",
            "--term",
            "sta",
            "--shape",
            "like",
            "--shape",
            "fts-platform",
        ])
        .expect("parse");
        assert_eq!(
            cli.command(),
            Command::BenchSearch {
                db: "/tmp/b.db".into(),
                platform: "nes".into(),
                term: "sta".into(),
                iterations: 20,
                shapes: vec![SearchShape::Like, SearchShape::FtsPlatform],
            }
        );
        assert!(Cli::try_parse_from([
            "mistarr",
            "bench-search",
            "--db",
            "b.db",
            "--platform",
            "nes",
            "--shape",
            "grep"
        ])
        .is_err());
        let cli = Cli::try_parse_from(["mistarr", "bench-seed", "--db", "s.db"]).expect("parse");
        assert_eq!(
            cli.command(),
            Command::BenchSeed {
                db: "s.db".into(),
                scale: 1.0
            }
        );
    }

    #[test]
    fn listen_addr_reads_any_valid_toml_form() {
        let dir = tempfile::tempdir().expect("tempdir");
        let data = dir.path().to_string_lossy().into_owned();
        let cli = Cli::try_parse_from(["mistarr", "--data", &data, "listen-addr"]).expect("parse");
        assert_eq!(cli.command(), Command::ListenAddr);
        assert_eq!(cli.config().expect("config").server.listen, "0.0.0.0:8420");
        for text in [
            "[server]\nlisten = '0.0.0.0:9000'\n",
            "server.listen = \"0.0.0.0:9000\"\n",
            "[ server ]\nlisten = \"\"\"0.0.0.0:9000\"\"\"\n",
            "[jobs]\n[server] # the web UI\n  listen=\"0.0.0.0:9000\"\n",
        ] {
            std::fs::write(dir.path().join("mistarr.toml"), text).expect("write");
            assert_eq!(
                cli.config().expect("config").server.listen,
                "0.0.0.0:9000",
                "{text}"
            );
        }
    }

    #[test]
    fn listen_flag_overrides_the_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            dir.path().join("mistarr.toml"),
            "[server]\nlisten = \"1.2.3.4:5\"\n",
        )
        .expect("write");
        let data = dir.path().to_string_lossy().into_owned();
        let cli = Cli::try_parse_from(["mistarr", "--data", &data]).expect("parse");
        assert_eq!(cli.config().expect("config").server.listen, "1.2.3.4:5");
        let cli = Cli::try_parse_from(["mistarr", "--data", &data, "--listen", "127.0.0.1:9"])
            .expect("parse");
        assert_eq!(cli.config().expect("config").server.listen, "127.0.0.1:9");
    }
}
