//! The `mistarr` binary: flags, logging, the tokio runtime and signal handling.

#![forbid(unsafe_code)]
#![warn(missing_docs)]
#![warn(clippy::pedantic)]

use anyhow::Context as _;
use clap::Parser as _;
use mistarr_server::app::{self, Options};
use mistarr_server::cli::{Cli, Command};
use mistarr_server::db::titles::SearchShape;
use mistarr_server::{db, doctor, logging, memory};

fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    let config = cli.config()?;
    if cli.command() == Command::ListenAddr {
        println!("{}", config.server.listen);
        return Ok(());
    }
    // Set before any thread starts, so every stack and heap counts against it.
    let data_limit =
        memory::limit_data(config.memory.data_limit_mib).context("cannot set the memory limit")?;
    let mut temp_refused = None;
    let ram = std::env::var_os(db::TEMP_DIR_ENV)
        .map_or_else(|| std::path::PathBuf::from(db::RAM_TEMP_DIR), Into::into);
    let frozen_file = ram.join(mistarr_server::freeze::FROZEN_NAME);
    if matches!(cli.command(), Command::Serve) {
        let disk = config.paths.tmp();
        let tmp = db::choose_temp_dir(&ram, &disk)
            .with_context(|| format!("cannot create {}", disk.display()))?;
        // Set before any thread starts, as the environment is shared.
        std::env::set_var(db::SQLITE_TMPDIR, &tmp.dir);
        temp_refused = tmp.refused.map(|e| (ram, e));
    }
    let runtime = memory::runtime().context("cannot start the async runtime")?;

    match cli.command() {
        Command::BenchSeed { db, scale } => {
            let seeded = mistarr_server::bench::seed_file(&db, scale)
                .with_context(|| format!("cannot seed {}", db.display()))?;
            println!(
                "{}: {} titles, {} roms, {} files",
                db.display(),
                seeded.titles,
                seeded.roms,
                seeded.files
            );
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
            let timings = mistarr_server::bench::search(&db, &platform, &term, iterations, &shapes)
                .with_context(|| format!("cannot time searches on {}", db.display()))?;
            println!("{platform} {term:?}, {iterations} runs");
            print!("{}", mistarr_server::bench::report(&timings));
        }
        Command::Doctor {
            hash_mib,
            rebuild_groups,
        } => {
            if rebuild_groups {
                let groups = doctor::rebuild_groups(&config.paths.db())
                    .context("cannot rebuild the title groups")?;
                println!("title groups rebuilt: {groups}");
            }
            runtime.block_on(async {
                let mut out = std::io::stdout().lock();
                doctor::run(&config, hash_mib, &mut out).await
            })?;
        }
        Command::ListenAddr => {}
        Command::Serve => {
            std::fs::create_dir_all(&config.paths.data)
                .with_context(|| format!("cannot create {}", config.paths.data.display()))?;
            logging::init(Some(&config.paths.log())).context("cannot open the log file")?;
            if let Some((ram, e)) = temp_refused {
                tracing::warn!(error = %e, dir = %ram.display(), "SQLite temporary files go to the card");
            }
            if let Some(bytes) = data_limit {
                tracing::info!(mib = bytes >> 20, "memory limit");
            } else {
                tracing::info!("no memory limit");
            }
            runtime.block_on(serve(config, frozen_file))?;
        }
    }
    Ok(())
}

async fn serve(
    config: mistarr_server::config::Config,
    frozen_file: std::path::PathBuf,
) -> anyhow::Result<()> {
    let options = Options {
        frozen_file,
        ..Options::default()
    };
    let running = app::start(config, options).await?;
    tracing::info!(
        url = %format!("http://{}/", running.addr),
        version = mistarr_server::version::version(),
        "mistarr started"
    );
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
