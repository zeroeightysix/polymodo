use crate::ipc::{AppSpawnOptions, ClientboundMessage, IpcS2C, IpcServer, ServerboundMessage};
use crate::mode::launch::Launcher;
use crate::polymodo::{Polymodo, PolymodoHandle};

pub fn run_server() -> anyhow::Result<std::convert::Infallible> {
    // set up the polymodo daemon socket for clients to connect to
    let ipc_server = crate::ipc::create_ipc_server()?; // TODO: try? here is probably not good

    crate::setup_slint_backend();

    let poly = Polymodo::new().into_handle();

    let _task = smol::spawn(accept_clients(poly.clone(), ipc_server));

    let key = poly.spawn_app::<Launcher>()?;

    log::info!("spawned launcher with key {key}");

    slint::run_event_loop_until_quit()?;

    unreachable!()
}

pub async fn accept_clients(
    polymodo: PolymodoHandle,
    ipc_server: IpcServer,
) {
    loop {
        let Ok(client) = ipc_server.accept().await else {
            continue;
        };

        log::debug!("accept new connection at {:?}", client.addr());

        let task = smol::spawn(serve_client(polymodo.clone(), client));
        task.detach(); // detach so it doesn't cancel when we drop `task`
    }
}

/// Given an [IpcClient], perform the read loop, serving any requests made by the client.
async fn serve_client(polymodo: PolymodoHandle, client: IpcS2C) {
    loop {
        let message = match client.recv().await {
            Err(crate::ipc::IpcReceiveError::DecodeError(e)) => {
                log::error!("could not decode message from client: {e}");
                log::error!("this is fatal: aborting connection with client.");
                return;
            }
            Err(crate::ipc::IpcReceiveError::IoError(e)) => {
                log::error!("io error while reading from client: {e}");
                log::error!("this is fatal: aborting connection with client.");
                return;
            }
            Ok(m) => m,
        };

        let _ = match message {
            ServerboundMessage::Ping => client.send(ClientboundMessage::Pong).await,
            ServerboundMessage::Spawn(AppSpawnOptions { app_name, single }) => {
                if single
                    && polymodo.is_app_running(app_name).await {
                        return;
                    }
                
                let result = polymodo.spawn_app::<Launcher>();
                let client = client.clone();

                // TODO: polymodo.wait_for_stop(app_key).await

                Ok(())
            }
            // this client is about to quit.
            ServerboundMessage::Goodbye => {
                log::debug!("closing connection at {:?}", client.addr());
                let _ = client.shutdown().await;

                return;
            }
        };
    }
}
