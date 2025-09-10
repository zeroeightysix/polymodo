use crate::ipc::{AppDescription, ClientboundMessage, IpcS2C, IpcServer, ServerboundMessage};
use crate::mode::launch::Launcher;
use crate::windowing::app;
use crate::windowing::app::AppEvent;
use std::collections::HashMap;
use std::future::IntoFuture;
use std::ops::Deref;
use std::sync::Arc;
use anyhow::bail;
use futures::FutureExt;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

struct Polymodo {
    apps: HashMap<app::AppKey, Box<dyn app::AppDriver>>,
    app_event_sender: mpsc::UnboundedSender<AppEvent>,
}

impl Polymodo {
    pub fn send_app_event(
        &self,
        app_event: AppEvent,
    ) -> Result<(), mpsc::error::SendError<AppEvent>> {
        self.app_event_sender.send(app_event)
    }
}

#[derive(Clone)]
struct PolymodoHandle(Arc<Polymodo>);

impl Deref for PolymodoHandle {
    type Target = Polymodo;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl PolymodoHandle {
    /// Create a new instance of an [app::App] and run it on the slint event loop.
    ///
    /// This method returns a feature that may be awaited to get the app's return value.
    /// The future will return `None` if the channel serving it was dropped; this is an error.
    /// Dropping the future does not cancel the app.
    fn spawn_app<A: app::App + 'static>(&self) -> impl std::future::Future<Output = Option<A::Output>> + Send + 'static
    where
        A::Message: Send + 'static,
        A::Output: Send + 'static,
    {
        // create a new key for this app.
        // (it's just a number)
        let key = app::new_app_key();
        let surf_driver_app_sender = self.0.app_event_sender.clone();
        let send = app::AppSender::new(key, surf_driver_app_sender.clone());

        let app::AppSetup { app, mut effects } = A::create(send);

        let (result_sender, result_receiver) = oneshot::channel();

        slint::invoke_from_event_loop(move || {
            slint::spawn_local(async_compat::Compat::new(async move {
                let output = effects.join_next().await.unwrap().unwrap(); // TODO: we need an abstraction on AppSetup to guarantee an effect

                // the app has finished, so we must remove it now.
                if let Err(e) = surf_driver_app_sender.send(AppEvent::DestroyApp { app_key: key }) {
                    log::error!("failed to send destruction event to `surf_driver_app_sender`: that's pretty bad");
                    log::error!("{e:?}");
                }

                let _ = result_sender.send(output);
            })).expect("an event loop");
        }).expect("an event loop");

        result_receiver.into_future()
            .map(|recv_result| recv_result.ok())
    }
}

fn setup() -> anyhow::Result<PolymodoHandle> {
    // create the app message channel. this is the main entrypoint for apps to asynchronously
    // talk to polymodo, or send messages to themselves to be handled on the UI thread.
    let (sender, mut receiver) = mpsc::unbounded_channel::<AppEvent>();

    let polymodo_handle = PolymodoHandle(Arc::new(Polymodo {
        apps: Default::default(),
        app_event_sender: sender,
    }));

    {
        let polymodo_handle = polymodo_handle.clone();

        // polymodo's logic should run as a future on the event loop thread,
        // so first we have to make sure we're on that thread:
        slint::invoke_from_event_loop(move || {
            // spawn a future that handles polymodo's app logic
            let _ = slint::spawn_local(async_compat::Compat::new(async move {
                while let Some(event) = receiver.recv().await {
                    // polymodo_handle.handle_event()
                    todo!()
                }

                // we got a None out of the receiver at this point,
                // meaning all senders have disappeared!
                log::warn!("polymodo app message task finished");
            }));
        })?;
    }

    Ok(polymodo_handle)
}

pub fn run_server() -> anyhow::Result<std::convert::Infallible> {
    // set up the polymodo daemon socket for clients to connect to
    // TODO
    // let ipc_server = crate::ipc::create_ipc_server().await?; // TODO: try? here is probably not good

    let poly = setup()?;

    // TODO
    // let _server_task = create_server_task(poly.clone(), ipc_server);

    drop(poly.spawn_app::<Launcher>());

    tokio::task::block_in_place(slint::run_event_loop_until_quit)?;

    unreachable!()
}

pub fn run_standalone() -> anyhow::Result<()> {
    let poly = setup()?;

    // The output of the app launched is used as the return value for the standalone run:
    let Some(result) = smol::block_on(poly.spawn_app::<Launcher>()) else {
        bail!("failed to await app result");
    };

    slint::run_event_loop_until_quit()?;
    
    result
}

fn create_server_task(
    polymodo: PolymodoHandle,
    ipc_server: IpcServer,
) -> JoinHandle<std::convert::Infallible> {
    tokio::task::spawn_local(async move {
        loop {
            let Ok(client) = ipc_server.accept().await else {
                continue;
            };

            log::debug!("accept new connection at {:?}", client.addr());

            // explicit drop: dropping a JoinHandle does not cancel the task;
            // we're simply not interested in ever joining this task
            drop(tokio::task::spawn_local(serve_client(
                polymodo.clone(),
                client,
            )));
        }
    })
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
            ServerboundMessage::IsRunning(type_id) => {
                todo!()
                // let (tx, rx) = tokio::sync::oneshot::channel();
                // polymodo.send_app_event(AppEvent::AppExistsQuery {
                //     app_type_id: type_id.clone(),
                //     response: tx,
                // });
                //
                // let running = rx.await.expect("sender half closed");
                //
                // if let Err(e) = client
                //     .send(ClientboundMessage::Running(type_id, running))
                //     .await
                // {
                //     log::error!("failed to reply to client: {e}");
                // }
                //
                // Ok(())
            }
            ServerboundMessage::Spawn(AppDescription::Launcher) => {
                let result = polymodo.spawn_app::<Launcher>();
                let client = client.clone();

                tokio::task::spawn_local(async move {
                    let app_result = result.await.expect("failed to join app task");
                    let app_result = format!("{app_result:?}");

                    let _ = client.send(ClientboundMessage::AppResult(app_result)).await;
                });

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
