use crate::ipc::{AppSpawnOptions, ClientboundMessage, IpcS2C, IpcServer, ServerboundMessage};
use crate::mode::launch::Launcher;
use crate::polymodo::{Polymodo, PolymodoHandle};

pub fn run_server() -> anyhow::Result<std::convert::Infallible> {
    crate::setup_slint_backend();

    // set up the polymodo daemon socket for clients to connect to
    let ipc_server = crate::ipc::create_ipc_server()?; // TODO: try? here is probably not good

    slint::invoke_from_event_loop(|| {
        let poly = Polymodo::new().into_handle();
        let _run_task = poly.start_running();

        let _server_task = slint::spawn_local(accept_clients(poly.clone(), ipc_server));

        let key = poly.spawn_app::<Launcher>().expect("failed to spawn app");
        log::info!("spawned launcher with key {key}");
    })
    .expect("an event loop");

    slint::run_event_loop_until_quit()?;

    unreachable!()
}

async fn accept_clients(polymodo: PolymodoHandle, ipc_server: IpcServer) {
    loop {
        let Ok(client) = ipc_server.accept().await else {
            continue;
        };

        log::debug!("accept new connection at {:?}", client.addr());

        // explicit drop: not interested in the return value of this task.
        // dropping it does not cancel the task
        drop(slint::spawn_local(serve_client(polymodo.clone(), client)).expect("an event loop"));
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
                if single && polymodo.is_app_running(app_name).await {
                    return;
                }

                let app_key = polymodo
                    .spawn_app::<Launcher>()
                    .expect("failed to spawn app");
                let app_result = polymodo
                    .wait_for_app_stop(app_key)
                    .await
                    .expect("sender closed");

                // TODO: what to do with this result?

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
