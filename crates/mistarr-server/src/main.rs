//! The `mistarr` binary: parses flags and calls [`mistarr_server::cli::run`].

use clap::Parser as _;
use mistarr_server::cli::{run, Cli};

fn main() -> anyhow::Result<()> {
    Ok(run(&Cli::parse(), &mut std::io::stdout())?)
}
