use crate::windowing::app;
use crate::windowing::app::{AppMessage, AppSender};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;

pub struct Polymodo {
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
    pub async fn is_app_running(&self, app_name: app::AppName) -> bool {
        let apps = self.apps.lock().await;
        apps.values().any(|x| x.app_name() == app_name)
    }

    pub fn into_handle(self) -> PolymodoHandle {
        PolymodoHandle(Arc::new(self))
    }
}

#[derive(Clone)]
pub struct PolymodoHandle(Arc<Polymodo>);

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
