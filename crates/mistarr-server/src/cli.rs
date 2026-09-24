//! Command-line flags.

use std::path::PathBuf;

use clap::{Parser, Subcommand};

use crate::config::Config;
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
#[derive(Debug, Clone, PartialEq, Eq, Subcommand)]
pub enum Command {
    /// Run the server.
    Serve,
    /// Print the checks a bug report should include.
    Doctor {
        /// MiB of zeros hashed for the throughput line.
        #[arg(long, default_value_t = crate::doctor::DEFAULT_HASH_MIB)]
        hash_mib: u32,
    },
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
        assert_eq!(cli.command(), Command::Doctor { hash_mib: 2 });
        assert_eq!(cli.data.as_deref(), Some(std::path::Path::new("/d")));
        let cli =
            Cli::try_parse_from(["mistarr", "--listen", "0.0.0.0:1", "serve"]).expect("parse");
        assert_eq!(cli.command(), Command::Serve);
        assert!(Cli::try_parse_from(["mistarr", "bogus"]).is_err());
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
