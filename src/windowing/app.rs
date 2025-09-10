use std::future::Future;
use std::marker::PhantomData;
use tokio::sync::mpsc;
use tokio::task::JoinSet;

pub type AppKey = u32;

pub fn new_app_key() -> AppKey {
    rand::random()
}

pub trait App: Sized {
    type Message;
    type Output;

    fn create(message_sender: AppSender<Self::Message>) -> AppSetup<Self, Self::Output>;

    #[allow(unused_variables)]
    fn on_message(&mut self, message: Self::Message) {
        // do nothing by default.
    }
}

pub struct AppSetup<A, O> {
    pub app: A,
    pub effects: JoinSet<O>,
}

impl<A, O> AppSetup<A, O> {
    pub fn new(app: A) -> Self {
        Self {
            app,
            effects: JoinSet::new(),
        }
    }
}

impl<A, O: 'static> AppSetup<A, O> {
    pub fn spawn_local<F>(self, future: F) -> Self
    where
        F: Future<Output = O> + 'static,
    {
        let mut effects = self.effects;
        effects.spawn_local(future);

        Self {
            app: self.app,
            effects,
        }
    }
}

/// Trait to 'drive' apps, being, to be able to access their methods in a dyn object-compatible way.
///
/// This serves to provide a dyn compatible trait for `AppSurfaceDriver` to use, as `App` itself
/// has GATs that make it dyn incompatible.
pub trait AppDriver {
    fn key(&self) -> AppKey;

    fn app_type(&self) -> &'static str;

    fn on_message(&mut self, message: Box<dyn std::any::Any>);
}

struct AppDriverImpl<A> {
    key: AppKey,
    app: A,
}

impl<A> AppDriverImpl<A> {
    pub fn new(key: AppKey, app: A) -> Self {
        Self { key, app }
    }
}

impl<A: App> AppDriver for AppDriverImpl<A>
where
    A: 'static,
    A::Message: 'static,
{
    fn key(&self) -> AppKey {
        self.key
    }

    fn app_type(&self) -> &'static str {
        std::any::type_name::<A>()
    }

    fn on_message(&mut self, message: Box<dyn std::any::Any>) {
        let Ok(message) = message.downcast() else {
            return;
        };

        self.app.on_message(*message);
    }
}

/// The sender end of a channel for apps to send messages to themselves.
pub struct AppSender<M> {
    sender: mpsc::UnboundedSender<AppEvent>,
    app_key: AppKey,
    data: PhantomData<M>,
}

impl<M> AppSender<M>
where
    M: Send + 'static,
{
    pub fn new(app_key: AppKey, sender: mpsc::UnboundedSender<AppEvent>) -> AppSender<M> {
        Self {
            sender,
            app_key,
            data: Default::default(),
        }
    }

    /// Send a message to the App, which will be received by its [App::on_message] method.
    pub fn send(&self, message: M) -> Result<(), Box<mpsc::error::SendError<AppEvent>>> {
        self.sender
            .send(AppEvent::AppMessage {
                app_key: self.app_key,
                message: Box::new(message),
            })
            .map_err(Box::new)
    }
}

pub enum AppEvent {
    /// An app has finished and should be removed.
    DestroyApp { app_key: AppKey },
    /// An App sent a message to itself, which necessitates an `on_message` call from the driver
    AppMessage {
        app_key: AppKey,
        message: Box<dyn std::any::Any + Send>,
    },
}