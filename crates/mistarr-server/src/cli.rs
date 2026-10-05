//! Parses the command-line flags and runs the chosen subcommand.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::app;
use crate::config::Config;
use crate::db::titles::browse::SearchShape;
use crate::error::{Error, Result};

/// `mistarr [--config FILE] [--data DIR] [--listen ADDR] [serve | doctor]`.
#[derive(Debug, Parser)]
#[command(
    name = "mistarr",
    version = crate::version::version(),
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

/// Wraps a failure with what was being done.
trait Context<T> {
    fn context(self, what: &str) -> Result<T>;
    fn with_context(self, what: impl FnOnce() -> String) -> Result<T>;
}

impl<T, E: std::error::Error + Send + Sync + 'static> Context<T> for std::result::Result<T, E> {
    fn context(self, what: &str) -> Result<T> {
        self.with_context(|| what.to_owned())
    }

    fn with_context(self, what: impl FnOnce() -> String) -> Result<T> {
        self.map_err(|e| Error::Command {
            what: what(),
            source: Box::new(e),
        })
    }
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
    /// [`crate::Error::ConfigRead`] or [`crate::Error::Config`] when the file cannot be loaded.
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

/// Runs the chosen subcommand to completion; `serve` returns after SIGINT or SIGTERM.
///
/// # Errors
///
/// Whatever the subcommand reports, with context naming what failed.
pub fn run(cli: &Cli, out: &mut impl std::io::Write) -> Result<()> {
    let config = cli.config()?;
    if cli.command() == Command::ListenAddr {
        writeln!(out, "{}", config.server.listen)?;
        return Ok(());
    }
    // Set before any thread starts, so every stack and heap counts against it.
    let data_limit = crate::memory::limit_data(config.memory.data_limit_mib)
        .context("cannot set the memory limit")?;
    let mut temp_refused = None;
    let ram = std::env::var_os(crate::db::tempdir::TEMP_DIR_ENV).map_or_else(
        || std::path::PathBuf::from(crate::db::tempdir::RAM_TEMP_DIR),
        Into::into,
    );
    let options = app::Options::for_board(&ram);
    if matches!(cli.command(), Command::Serve) {
        let disk = config.paths.tmp();
        let tmp = crate::db::tempdir::choose_temp_dir(&ram, &disk)
            .with_context(|| format!("cannot create {}", disk.display()))?;
        // Set before any thread starts, as the environment is shared.
        std::env::set_var(crate::db::tempdir::SQLITE_TMPDIR, &tmp.dir);
        temp_refused = tmp.refused.map(|e| (ram, e));
    }
    let runtime = crate::memory::runtime().context("cannot start the async runtime")?;

    match cli.command() {
        Command::BenchSeed { db, scale } => {
            let seeded = crate::bench::seed_file(&db, scale)
                .with_context(|| format!("cannot seed {}", db.display()))?;
            writeln!(
                out,
                "{}: {} titles, {} roms, {} files",
                db.display(),
                seeded.titles,
                seeded.roms,
                seeded.files
            )?;
        }
        Command::BenchSearch {
            db,
            platform,
            term,
            iterations,
            shapes,
        } => {
            let shapes = if shapes.is_empty() {
                SearchShape::ALL.to_vec()
            } else {
                shapes
            };
            let timings = crate::bench::search(&db, &platform, &term, iterations, &shapes)
                .with_context(|| format!("cannot time searches on {}", db.display()))?;
            writeln!(out, "{platform} {term:?}, {iterations} runs")?;
            write!(out, "{}", crate::bench::report(&timings))?;
        }
        Command::Doctor {
            hash_mib,
            rebuild_groups,
        } => {
            if rebuild_groups {
                let groups = crate::doctor::rebuild_groups(&config.paths.db())
                    .context("cannot rebuild the title groups")?;
                writeln!(out, "title groups rebuilt: {groups}")?;
            }
            runtime
                .block_on(crate::doctor::run(&config, hash_mib, out))
                .context("cannot write the report")?;
        }
        Command::ListenAddr => {}
        Command::Serve => {
            std::fs::create_dir_all(&config.paths.data)
                .with_context(|| format!("cannot create {}", config.paths.data.display()))?;
            crate::logging::init(Some(&config.paths.log())).context("cannot open the log file")?;
            if let Some((ram, e)) = temp_refused {
                tracing::warn!(error = %e, dir = %ram.display(), "SQLite temporary files go to the card");
            }
            if let Some(bytes) = data_limit {
                tracing::info!(mib = bytes >> 20, "memory limit");
            } else {
                tracing::info!("no memory limit");
            }
            runtime.block_on(serve(config, options))?;
        }
    }
    Ok(())
}

async fn serve(config: Config, options: app::Options) -> Result<()> {
    let running = app::start(config, options).await?;
    tracing::info!(
        url = %format!("http://{}/", running.addr),
        version = crate::version::version(),
        "mistarr started"
    );
    wait_for_signal().await?;
    tracing::info!("shutting down");
    running.shutdown().await?;
    Ok(())
}

async fn wait_for_signal() -> Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).context("cannot watch SIGTERM")?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r.context("cannot watch SIGINT")?,
        _ = term.recv() => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn listen_addr_prints_the_effective_address() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::try_parse_from([
            "mistarr",
            "listen-addr",
            "--data",
            dir.path().to_str().unwrap(),
            "--listen",
            "127.0.0.1:9",
        ])
        .unwrap();
        let mut out = Vec::new();
        run(&cli, &mut out).unwrap();
        assert_eq!(String::from_utf8(out).unwrap(), "127.0.0.1:9\n");
    }

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
