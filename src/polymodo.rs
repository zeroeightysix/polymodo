use crate::app_surface_driver;
use crate::app_surface_driver::{create_app_driver, new_app_key, AppEvent};
use crate::ipc::{AppDescription, ClientboundMessage, IpcS2C, IpcServer, ServerboundMessage};
use crate::mode::launch::Launcher;
use crate::windowing::app::{App, AppSender, AppSetup};
use crate::windowing::client::{SurfaceEvent, WaylandClient};
use std::rc::Rc;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct Polymodo {
    surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
    surf_driver_app_sender: local_channel::mpsc::Sender<AppEvent>,
}

impl Polymodo {
    fn spawn_app<A: App + 'static>(&self) -> JoinHandle<<A as App>::Output>
    where
        A::Message: 'static,
        A::Output: 'static,
    {
        // create a new key for this app.
        // (it's just a number)
        let key = new_app_key();
        let surf_driver_app_sender = self.surf_driver_app_sender.clone();
        let send = AppSender::new(key, surf_driver_app_sender.clone());
        let AppSetup { app, mut effects } = A::create(send);
        let driver = create_app_driver(key, app, self.surf_driver_event_sender.clone());

        self.surf_driver_app_sender
            .send(AppEvent::NewApp {
                app_driver: Box::new(driver),
                layer_surface_options: Launcher::layer_surface_options(),
            })
            .expect("failed to spawn new app; thus the app driver is dead; polymodo cannot function anymore.");

        tokio::task::spawn_local(async move {
            let output = effects.join_next().await.unwrap().unwrap(); // TODO: we need an abstraction on AppSetup to guarantee an effect

            // the app has finished, so we must remove it now.
            if let Err(e) = surf_driver_app_sender.send(AppEvent::DestroyApp { app_key: key }) {
                log::error!("failed to send destruction event to `surf_driver_app_sender`: that's pretty bad");
                log::error!("{e:?}");
            }

            output
            // TODO: handle output
        })
    }
}

pub async fn run() -> anyhow::Result<std::convert::Infallible> {
    // set up the polymodo daemon socket for clients to connect to
    // TODO: should be configurable if this is done at all, for a standalone mode!
    let ipc_server = crate::ipc::create_ipc_server().await?; // TODO: try? here is probably not good

    // two channels: one for events (that is Send + Sync)
    let (surf_driver_event_sender, event_receive) = mpsc::channel(128);
    // and one for app creation, which is !Send and !Sync, so that we do not need a Send+Sync requirement
    // on App implementations. This allows apps to use normal sync and memory sharing utils, like Rc.
    let (surf_driver_app_sender, app_receive) = local_channel::mpsc::channel();

    // set up the connection to wayland and wgpu
    let client = WaylandClient::create(surf_driver_event_sender.clone()).await?;
    let surface_setup = client.new_surface_setup(Default::default()).await?;

    // set up the surface app driver task.
    // this one processes events coming from the dispatcher,
    // render requests from the dispatcher and other sources,
    // and holds app state.
    let _surf_drive_task = app_surface_driver::create_surface_driver_task(
        surf_driver_event_sender.clone(),
        event_receive,
        app_receive,
        surface_setup,
    );

    // set up the dispatch task which polls wayland and sends client to the surface app driver
    let dispatch_task = create_dispatch_task(client);

    let poly = Rc::new(Polymodo {
        surf_driver_event_sender,
        surf_driver_app_sender,
    });

    let _server_task = create_server_task(poly.clone(), ipc_server);

    poly.spawn_app::<Launcher>();

    // both surf_drive_task and dispatch_task should never complete.
    // we could join and wait on them here, but either will never finish.. so we just pick one:
    Ok(dispatch_task.await?)
}

fn create_server_task(
    polymodo: Rc<Polymodo>,
    ipc_server: IpcServer,
) -> JoinHandle<std::convert::Infallible> {
    tokio::task::spawn_local(async move {
        loop {
            let Ok(client) = ipc_server.accept().await else {
                continue;
            };

            log::debug!("accept new connection");

            let _ = tokio::task::spawn_local(serve_client(Rc::clone(&polymodo), client));
        }
    })
}

/// Given an [IpcClient], perform the read loop, serving any requests made by the client.
async fn serve_client(polymodo: Rc<Polymodo>, client: IpcS2C) {
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
            ServerboundMessage::Spawn(AppDescription::Launcher) => {
                let result = polymodo.spawn_app::<Launcher>();
                let client = client.clone();

                tokio::task::spawn_local(async move {
                    let app_result = result.await.expect("failed to join app task");
                    let app_result = format!("{:?}", app_result);

                    let _ = client.send(ClientboundMessage::AppResult(app_result)).await;
                });

                Ok(())
            }
        };
    }
}

fn create_dispatch_task(mut client: WaylandClient) -> JoinHandle<std::convert::Infallible> {
    tokio::task::spawn_blocking(move || loop {
        if let Err(e) = client.dispatch() {
            log::warn!("error dispatching: {e}");
            std::process::exit(1);
        }
    })
}
