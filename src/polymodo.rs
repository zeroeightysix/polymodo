use crate::ipc::{AppSpawnOptions, ClientboundMessage, IpcS2C, IpcServer, ServerboundMessage};
use crate::mode::launch::Launcher;
use crate::windowing::app;
use crate::windowing::app::{AppMessage, AppSender};
use slint::winit_030::winit::platform::wayland::{
    KeyboardInteractivity, Layer, WindowAttributesWayland,
};
use slint::BackendSelector;
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

struct Polymodo {
    apps: smol::lock::Mutex<HashMap<app::AppKey, Box<dyn app::AppDriver>>>,
    app_message_channel: (
        smol::channel::Sender<AppMessage>,
        smol::channel::Receiver<AppMessage>,
    ),
}

#[derive(Debug, derive_more::Error, derive_more::Display, derive_more::From)]
enum PolymodoError {
    #[display("no app with app key {_0} exists")]
    NoSuchApp(#[error(not(source))] app::AppKey),
}

impl Polymodo {
    pub fn new() -> Self {
        let channel = smol::channel::unbounded::<AppMessage>();

        Self {
            apps: Default::default(),
            app_message_channel: channel,
        }
    }

    pub fn app_sender<M: Send + 'static>(&self, app_key: app::AppKey) -> AppSender<M> {
        let sender = self.app_message_channel.0.clone();

        AppSender::new(app_key, sender)
    }

    /// Request an app to stop. Returns its output value, boxed as any.
    pub async fn stop_app(
        &self,
        app: app::AppKey,
    ) -> Result<Box<dyn std::any::Any>, PolymodoError> {
        let mut app = self
            .apps
            .lock()
            .await
            .remove(&app)
            .ok_or(PolymodoError::NoSuchApp(app))?;

        Ok(app.stop())
    }

    /// Receive one message from the messages channel (potentially waiting if there are none) and
    /// forward it to the app it came from.
    async fn handle_app_message(&self) {
        let Ok(AppMessage { app_key, message }) = self.app_message_channel.1.recv().await else {
            // `recv` only returns an error if the channel is closed (impossible: `app_message_channel` holds a sender),
            // or full (impossible: we make an unbounded channel!),
            // thus this should really never happen.
            unreachable!();
        };

        // handling messages requires mutable access to the app,
        // so we lock apps here.
        let mut apps = self.apps.lock().await;
        let Some(app) = apps.get_mut(&app_key) else {
            // might happen if an app sends a message, but is stopped before that message ever gets processed.
            log::warn!("failed to send message to app, because app does not exist.");
            return;
        };

        app.on_message(message);

        drop(apps); // explicitly release the lock, in case we ever add code below here ;)
    }
    
    /// Is an app with this `app_name` running?
    async fn is_app_running(&self, app_name: app::AppName) -> bool {
        let apps = self.apps.lock().await;
        apps.values().any(|x| x.app_name() == app_name)
    }

    pub fn into_handle(self) -> PolymodoHandle {
        PolymodoHandle(Arc::new(self))
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
    /// Returns the associated app key.
    ///
    /// This method only exists on `PolymodoHandle`, as a new handle is created to pass onto the event loop.
    pub fn spawn_app<A>(&self) -> anyhow::Result<app::AppKey>
    where
        A: app::App + 'static,
        A::Message: Send + 'static,
    {
        // create a new key for this app.
        // (it's just a number)
        let key = app::new_app_key();
        let app_sender = self.app_sender(key);
        let handle = self.clone();

        slint::invoke_from_event_loop(move || {
            // Create the app and its driver (wrapper)
            let app = A::create(app_sender);
            let driver = app::driver_for(key, app);

            // Add it to the list
            let mut apps = handle.apps.lock_blocking();
            apps.insert(key, Box::new(driver));
            drop(apps);
        })?;

        Ok(key)
    }
}

pub fn run_server() -> anyhow::Result<std::convert::Infallible> {
    // set up the polymodo daemon socket for clients to connect to
    let ipc_server = crate::ipc::create_ipc_server()?; // TODO: try? here is probably not good

    setup_slint_backend();

    let poly = Polymodo::new().into_handle();

    let _task = smol::spawn(accept_clients(poly.clone(), ipc_server));

    let key = poly.spawn_app::<Launcher>()?;

    log::info!("spawned launcher with key {key}");

    slint::run_event_loop_until_quit()?;

    unreachable!()
}

pub fn run_standalone() -> anyhow::Result<()> {
    setup_slint_backend();

    let poly = Polymodo::new().into_handle();

    poly.spawn_app::<Launcher>().expect("Failed to spawn app");

    slint::run_event_loop_until_quit()?;

    Ok(())
    // result
}

fn setup_slint_backend() {
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

async fn accept_clients(
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
