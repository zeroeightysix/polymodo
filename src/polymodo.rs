use std::any::Any;
use crate::app;
use crate::app::{AppEvent, AppMessage, AppSender};
use std::collections::HashMap;
use std::ops::Deref;
use std::sync::Arc;
use smol::Task;

pub struct Polymodo {
    apps: smol::lock::Mutex<HashMap<app::AppKey, Box<dyn app::AppDriver>>>,
    app_finish_senders: smol::lock::Mutex<HashMap<app::AppKey, oneshot::Sender<Option<Box<dyn Any + Send>>>>>,
    app_message_channel: (
        smol::channel::Sender<AppEvent>,
        smol::channel::Receiver<AppEvent>,
    ),
}

impl Polymodo {
    pub fn new() -> Self {
        let channel = smol::channel::unbounded::<AppEvent>();

        Self {
            apps: Default::default(),
            app_finish_senders: Default::default(),
            app_message_channel: channel,
        }
    }

    pub async fn wait_for_app_stop(&self, app_key: app::AppKey) -> anyhow::Result<Option<Box<dyn Any + Send>>> {
        // set up the channel of a "finish sender" stored in Polymodo:
        let (sender, receiver) = oneshot::channel();

        // the sender bit we'll put into polymodo for it to find when an app finishes:
        {
            let mut senders = self.app_finish_senders.lock().await;

            if let Some(previous_sender) = senders.insert(app_key, sender) {
                // oops. we're overwriting a sender that came before us!
                // send it a None, to notify it that it may stop listening:
                let _ = previous_sender.send(None);
            }

            drop(senders);
        }

        // and now, we wait:
        Ok(receiver.await?)
    }

    /// Stop an app. Returns its output value, boxed as any.
    async fn stop_app(
        &self,
        app: app::AppKey,
    ) -> Result<Box<dyn Any + Send>, PolymodoError> {
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
        let Ok(AppEvent { app_key, message }) = self.app_message_channel.1.recv().await else {
            // `recv` only returns an error if the channel is closed (impossible: `app_message_channel` holds a sender),
            // or full (impossible: we make an unbounded channel!),
            // thus this should really never happen.
            unreachable!();
        };

        match message {
            AppMessage::Finished => {
                let Ok(result) = self.stop_app(app_key).await else {
                    log::error!("got a Finished message for an app that doesn't exist");
                    return;
                };

                // check if anyone's listening for this app's result:
                let mut senders = self.app_finish_senders.lock().await;
                if let Some(sender) = senders.remove(&app_key) {
                    if let Err(_) = sender.send(Some(result)) {
                        log::warn!("could not deliver app result because the receiver has been dropped");
                    }
                } else {
                    // no one's listening. do we want to log the result somehow?
                    log::warn!("app finished, but no listener was registered for its result");

                }
            }
            AppMessage::Message(message) => {
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
        }
    }

    pub fn app_sender<M: Send + 'static>(&self, app_key: app::AppKey) -> AppSender<M> {
        let sender = self.app_message_channel.0.clone();

        AppSender::new(app_key, sender)
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

#[derive(Debug, derive_more::Error, derive_more::Display, derive_more::From)]
pub enum PolymodoError {
    #[display("no app with app key {_0} exists")]
    NoSuchApp(#[error(not(source))] app::AppKey),
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
        A::Output: Send
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

    pub fn start_running(&self) -> Task<std::convert::Infallible> {
        let poly = self.clone();
        smol::spawn(async move {
            loop {
                poly.handle_app_message().await;
            }
        })
    }
}
