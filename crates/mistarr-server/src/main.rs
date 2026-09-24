//! The mistarr binary. Assembles the crates into an HTTP server with a
//! SQLite database, a job scheduler and the embedded web UI.
//! See `docs/ARCHITECTURE.md` and `docs/WORKPLAN.md` WP-09 onwards.

#![forbid(unsafe_code)]

fn main() -> anyhow::Result<()> {
    println!(
        "mistarr {} (skeleton; see docs/WORKPLAN.md)",
        env!("CARGO_PKG_VERSION")
    );
    Ok(())
}
