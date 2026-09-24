//! The `mistarr` binary: flags, logging, the tokio runtime and signal handling.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use anyhow::Context as _;
use clap::Parser as _;
use mistarr_server::app::{self, Options};
use mistarr_server::cli::{Cli, Command};
use mistarr_server::{db, doctor, logging, memory};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = cli.config()?;
    // Set before any thread starts, so every stack and heap counts against it.
    let data_limit =
        memory::limit_data(config.memory.data_limit_mib).context("cannot set the memory limit")?;
    if matches!(cli.command(), Command::Serve) {
        let tmp = config.paths.tmp();
        db::prepare_temp_dir(&tmp).with_context(|| format!("cannot create {}", tmp.display()))?;
        // The board's /tmp is RAM; set before any thread starts, as the environment is shared.
        std::env::set_var(db::SQLITE_TMPDIR, &tmp);
    }
    let runtime = memory::runtime().context("cannot start the async runtime")?;

    match cli.command() {
        Command::Doctor { hash_mib } => runtime.block_on(async {
            let mut out = std::io::stdout().lock();
            doctor::run(&config, hash_mib, &mut out).await
        })?,
        Command::Serve => {
            std::fs::create_dir_all(&config.paths.data)
                .with_context(|| format!("cannot create {}", config.paths.data.display()))?;
            logging::init(Some(&config.paths.log())).context("cannot open the log file")?;
            if let Some(bytes) = data_limit {
                tracing::info!(mib = bytes >> 20, "memory limit");
            } else {
                tracing::info!("no memory limit");
            }
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
