mod app_surface_driver;
mod config;
mod fuzzy_search;
mod live_handle;
mod mode;
mod polymodo;
mod windowing;
mod xdg;

use std::sync::OnceLock;
use std::time::Instant;
use tokio::task::LocalSet;
use tracing::metadata::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

/// Some starting time.
///
/// Relative to whoever asks first.
pub fn start_time() -> Instant {
    static LOCK: OnceLock<Instant> = OnceLock::new();
    *LOCK.get_or_init(Instant::now)
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter)
        .try_init()?;

    log_panics::init();

    LocalSet::new()
        .run_until(async move {
            let Err(e) = polymodo::run().await;

            log::error!("Error running polymodo: {e}");
        })
        .await;

    Ok(())
}
