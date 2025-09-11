mod cli;
mod config;
mod fuzzy_search;
mod ipc;
mod mode;
mod notify;
mod persistence;
mod polymodo;
mod windowing;
mod xdg;

pub mod modules {
    slint::include_modules!();
}

use crate::cli::Args;
use crate::ipc::{AppSpawnOptions, ClientboundMessage, IpcC2S, ServerboundMessage};
use crate::windowing::app::AppName;
use clap::Parser;
use std::io::ErrorKind;
use std::sync::OnceLock;
use std::time::Instant;
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

fn main() -> anyhow::Result<()> {
    setup_logging()?;

    let args = cli::Args::parse();

    if args.standalone {
        log::info!("Starting standalone polymodo");

        polymodo::run_standalone();

        std::process::exit(0);
    }

    // try connecting to a running polymodo daemon.
    match ipc::connect_to_polymodo_daemon() {
        Ok(client) => {
            // ok, we have a client, let's talk with the server!
            // the client is written in async code, so set up a runtime here.
            let _ = smol::block_on(run_polymodo_client(args, client));

            todo!()
        }
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {
            // ConnectionRefused happens when there is no one listening on the other end, i.e.
            // there isn't a polymodo daemon yet.
            // let's become that!
            log::info!("Starting polymodo daemon");

            polymodo::run_server()?;

            unreachable!();
        }
        Err(e) => {
            // errors other than ConnectionRefused are considered fatal, as something other went
            // wrong other than "there isn't anyone listening"
            log::error!("Failed to connect to running polymodo daemon: {e}");
            log::error!("If this happens even though you are sure there is no instance of polymodo running already, then this is a bug: please report it!");

            std::process::exit(-1);
        }
    }
}

async fn run_polymodo_client(args: Args, client: IpcC2S) -> anyhow::Result<Option<String>> {
    client
        .send(ServerboundMessage::Spawn(AppSpawnOptions {
            app_name: AppName::Launcher,
            single: args.single,
        }))
        .await
        .expect("failed to send");

    let app_result = client.recv().await?;

    client
        .send(ServerboundMessage::Goodbye)
        .await
        .expect("send failed");
    client.shutdown().await.expect("shutdown failed");

    Ok(match app_result {
        ClientboundMessage::AppResult(result) => Some(result),
        _ => None,
    })
}

fn setup_logging() -> anyhow::Result<()> {
    let env_filter = EnvFilter::builder()
        .with_default_directive(LevelFilter::WARN.into())
        .from_env_lossy();

    tracing_subscriber::registry()
        .with(tracing_subscriber::fmt::layer())
        .with(env_filter)
        .try_init()?;

    log_panics::init();
    Ok(())
}
