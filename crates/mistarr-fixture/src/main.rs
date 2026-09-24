//! `mistarr-fixture`: builds DATs, torrents and the synthetic set, and runs a
//! local tracker. Usage is in `docs/TESTING.md`.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use std::io::Write;
use std::path::PathBuf;

use anyhow::Context;
use clap::{Parser, Subcommand};
use mistarr_fixture::{dat, set, torrent, tracker};

/// Synthetic fixtures for testing mistarr.
#[derive(Debug, Parser)]
#[command(name = "mistarr-fixture", version)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Print a Logiqx DAT describing every file under DIR.
    Dat {
        /// Platform id from docs/PLATFORMS.md; picks the header name and hashing rule.
        #[arg(long)]
        platform: String,
        /// Set name, prefixed to the platform name in the DAT header.
        #[arg(long)]
        name: String,
        /// DAT version string.
        #[arg(long, default_value = "1")]
        version: String,
        /// Directory of files.
        dir: PathBuf,
    },
    /// Print a v1 multi-file .torrent of DIR with 256 KiB pieces.
    Torrent {
        /// Announce URL.
        #[arg(long)]
        tracker: String,
        /// Optional web seed URL, written as url-list.
        #[arg(long)]
        web_seed: Option<String>,
        /// Directory of files; its name becomes the torrent name.
        dir: PathBuf,
    },
    /// Write the synthetic set and its DATs under OUT.
    Set {
        /// Output directory.
        out: PathBuf,
    },
    /// Run a local HTTP tracker until interrupted.
    Tracker {
        /// Address to listen on.
        #[arg(long, default_value = "127.0.0.1:0")]
        listen: String,
        /// Hand out peers announcing from loopback as this IPv4 address; `auto` picks this host's.
        #[arg(long)]
        loopback_as: Option<String>,
    },
}

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let mut stdout = std::io::stdout().lock();
    match cli.command {
        Command::Dat {
            platform,
            name,
            version,
            dir,
        } => {
            let xml = dat::build(&dir, &platform, &name, &version)?;
            stdout.write_all(xml.as_bytes())?;
        }
        Command::Torrent {
            tracker,
            web_seed,
            dir,
        } => {
            let bytes = torrent::build(&dir, &tracker, web_seed.as_deref())?;
            stdout.write_all(&bytes)?;
        }
        Command::Set { out } => {
            let layout =
                set::generate(&out).with_context(|| format!("writing {}", out.display()))?;
            for path in [
                &layout.cart_dir,
                &layout.disc_dir,
                &layout.cart_dat,
                &layout.disc_dat,
            ] {
                writeln!(stdout, "{}", path.display())?;
            }
        }
        Command::Tracker {
            listen,
            loopback_as,
        } => {
            let loopback_as = match loopback_as.as_deref() {
                None => None,
                Some("auto") => Some(tracker::local_ipv4().context("no local IPv4 address")?),
                Some(ip) => Some(ip.parse().context("--loopback-as takes an IPv4 address")?),
            };
            let t = tracker::Tracker::start(&listen, loopback_as)?;
            writeln!(stdout, "{}", t.announce_url())?;
            stdout.flush()?;
            // Serves until the process is interrupted; `t` must stay alive.
            loop {
                std::thread::park();
            }
        }
    }
    stdout.flush()?;
    Ok(())
}
