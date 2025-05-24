mod app_surface_driver;
mod cli;
mod config;
mod fuzzy_search;
mod ipc;
mod live_handle;
mod mode;
mod polymodo;
mod windowing;
mod xdg;

use crate::ipc::{AppDescription, ServerboundMessage};
use clap::Parser;
use std::io::ErrorKind;
use std::sync::OnceLock;
use std::time::Instant;
use tokio::task::LocalSet;
use tracing::metadata::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

static RUNTIME: OnceLock<tokio::runtime::Handle> = OnceLock::new();

/// Some starting time.
///
/// Relative to whoever asks first.
pub fn start_time() -> Instant {
    static LOCK: OnceLock<Instant> = OnceLock::new();
    *LOCK.get_or_init(Instant::now)
}

/// Returns a handle to the application's tokio runtime, to be used to spawn tasks
/// from threads not handled by tokio.
pub fn runtime() -> tokio::runtime::Handle {
    RUNTIME.get().cloned().expect("the runtime wasn't set")
}

#[tokio::main(flavor = "current_thread")]
async fn main() -> anyhow::Result<()> {
    RUNTIME
        .set(tokio::runtime::Handle::current())
        .expect("failed to set the runtime");

    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter)
        .try_init()?;

    log_panics::init();

    let args = cli::Args::parse();

    if args.standalone {
        log::info!("Starting standalone polymodo");

        run_polymodo_standalone().await;

        std::process::exit(0);
    }

    // try connecting to a running polymodo daemon.
    match ipc::connect_to_polymodo_daemon().await {
        Ok(client) => {
            // ok, we have a client, let's talk with the server!

            client
                .send(ServerboundMessage::Spawn(AppDescription::Launcher))
                .await
                .expect("failed to send");

            println!("{:?}", client.recv().await);

            client
                .send(ServerboundMessage::Goodbye)
                .await
                .expect("send failed");
            client.shutdown().await.expect("shutdown failed");
        }
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {
            // ConnectionRefused happens when there is no one listening on the other end, i.e.
            // there isn't a polymodo daemon yet.
            // let's become that!
            log::info!("Starting polymodo daemon");

            run_polymodo_daemon().await;
        }
        Err(e) => {
            // errors other than ConnectionRefused are considered fatal, as something other went
            // wrong other than "there isn't anyone listening"
            log::error!("Failed to connect to running polymodo daemon: {e}");
            log::error!("If this happens even though you are sure there is no instance of polymodo running already, then this is a bug: please report it!");

            std::process::exit(-1);
        }
    }

    Ok(())
}

/// Start polymodo in standalone mode
async fn run_polymodo_standalone() {
    LocalSet::new()
        .run_until(async move {
            let _ = polymodo::run_standalone().await;
        })
        .await;
}

/// Start the polymodo server.
async fn run_polymodo_daemon() {
    LocalSet::new()
        .run_until(async move {
            let Err(e) = polymodo::run_server().await;

            log::error!("Error running polymodo: {e}");
        })
        .await;
}
