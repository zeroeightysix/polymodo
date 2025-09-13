mod cli;
mod config;
mod fuzzy_search;
mod ipc;
mod mode;
mod notify;
mod persistence;
mod polymodo;
mod xdg;
mod server;
pub mod app;

pub mod modules {
    slint::include_modules!();
}

use crate::cli::Args;
use crate::ipc::{AppSpawnOptions, ClientboundMessage, IpcC2S, ServerboundMessage};
use app::AppName;
use clap::Parser;
use std::io::ErrorKind;
use tracing::metadata::LevelFilter;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;
use slint::BackendSelector;
use slint::winit_030::winit::platform::wayland::{KeyboardInteractivity, Layer, WindowAttributesWayland};
use crate::mode::launch::Launcher;
use crate::polymodo::Polymodo;

fn main() -> anyhow::Result<()> {
    setup_logging()?;

    let args = cli::Args::parse();

    if args.standalone {
        log::info!("Starting standalone polymodo");

        run_standalone()?;

        std::process::exit(0);
    }

    // try connecting to a running polymodo daemon.
    match ipc::connect_to_polymodo_daemon() {
        Ok(client) => {
            // ok, we have a client, let's talk with the server!
            // the client is written in async code, so set up a runtime here.
            let _ = smol::block_on(run_client(args, client));

            todo!()
        }
        Err(err) if err.kind() == ErrorKind::ConnectionRefused => {
            // ConnectionRefused happens when there is no one listening on the other end, i.e.
            // there isn't a polymodo daemon yet.
            // let's become that!
            log::info!("Starting polymodo daemon");

            server::run_server()?;

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

/// Run polymodo as a client interacting with the incumbent polymodo daemon.
///
/// This, more or less, just sets up IPC, spawns the desired app, and waits for its result.
async fn run_client(args: Args, client: IpcC2S) -> anyhow::Result<Option<String>> {
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

/// Run polymodo without connecting to a server and without setting up IPC.
///
/// This function returns when the spawned app dies.
pub fn run_standalone() -> anyhow::Result<()> {
    setup_slint_backend();

    slint::invoke_from_event_loop(|| {
        let poly = Polymodo::new().into_handle();
        let _run_task = poly.start_running();
        let app = poly.spawn_app::<Launcher>().expect("Failed to spawn app");

        slint::spawn_local(async move {
            let result = poly.wait_for_app_stop(app);

            let result1 = result.await;

            slint::quit_event_loop().expect("failed to quit");
        }).expect("an event loop");
    }).expect("an event loop");

    slint::run_event_loop_until_quit().expect("slint failed");

    Ok(())
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

pub fn setup_slint_backend() {
    BackendSelector::default()
        .with_winit_window_attributes_hook(|mut attrs| {
            attrs.platform = Some(Box::new(
                WindowAttributesWayland::layer_shell()
                    .with_layer(Layer::Overlay)
                    .with_keyboard_interactivity(KeyboardInteractivity::OnDemand),
            ));
            attrs
        })
        .select()
        .expect("failed to select");
}
