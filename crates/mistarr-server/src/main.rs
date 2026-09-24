//! The `mistarr` binary: flags, logging, the tokio runtime and signal handling.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use anyhow::Context as _;
use clap::Parser as _;
use mistarr_server::app::{self, Options};
use mistarr_server::cli::{Cli, Command};
use mistarr_server::{doctor, logging};

/// tokio worker threads; see the budgets in `docs/ARCHITECTURE.md`.
const WORKERS: usize = 2;

/// Cap on threads running blocking SQLite and file work.
const BLOCKING_THREADS: usize = 4;

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = cli.config()?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(WORKERS)
        .max_blocking_threads(BLOCKING_THREADS)
        .enable_all()
        .build()
        .context("cannot start the async runtime")?;

    match cli.command() {
        Command::Doctor { hash_mib } => runtime.block_on(async {
            let mut out = std::io::stdout().lock();
            doctor::run(&config, hash_mib, &mut out).await
        })?,
        Command::Serve => {
            std::fs::create_dir_all(&config.paths.data)
                .with_context(|| format!("cannot create {}", config.paths.data.display()))?;
            logging::init(Some(&config.paths.log())).context("cannot open the log file")?;
            runtime.block_on(serve(config))?;
        }
    }
    Ok(())
}

async fn serve(config: mistarr_server::config::Config) -> anyhow::Result<()> {
    let running = app::start(config, Options::default()).await?;
    tracing::info!(url = %format!("http://{}/", running.addr), "mistarr started");
    wait_for_signal().await?;
    tracing::info!("shutting down");
    running.shutdown().await?;
    Ok(())
}

async fn wait_for_signal() -> anyhow::Result<()> {
    use tokio::signal::unix::{signal, SignalKind};
    let mut term = signal(SignalKind::terminate()).context("cannot watch SIGTERM")?;
    tokio::select! {
        r = tokio::signal::ctrl_c() => r.context("cannot watch SIGINT")?,
        _ = term.recv() => {}
    }
    Ok(())
}
