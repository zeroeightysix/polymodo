use std::marker::PhantomData;

pub type AppKey = u32;

pub fn new_app_key() -> AppKey {
    rand::random()
}

pub trait App: Sized + Send {
    type Message;
    type Output;

    fn create(message_sender: AppSender<Self::Message>) -> Self;

    #[allow(unused_variables)]
    fn on_message(&mut self, message: Self::Message) {
        // do nothing by default.
    }

    fn stop(self) -> Self::Output;
}

/// Trait to 'drive' apps, being, to be able to access their methods in a dyn object-compatible way.
///
/// This serves to provide a dyn compatible trait for `AppSurfaceDriver` to use, as `App` itself
/// has GATs that make it dyn incompatible.
pub trait AppDriver: Send {
    fn key(&self) -> AppKey;

    fn app_type(&self) -> &'static str;

    fn on_message(&mut self, message: Box<dyn std::any::Any>);

    /// Stop the driven application. This mirrors [App]'s `stop` function, but is non-consuming.
    /// This is because `AppDriver` is meant to be used as a dynamic trait object, on which methods
    /// accepting `self` (instead of a reference) cannot be called.
    ///
    /// Panics if called twice.
    fn stop(&mut self) -> Box<dyn std::any::Any>;
}

struct AppDriverImpl<A> {
    key: AppKey,
    app: Option<A>,
}

impl<A> AppDriverImpl<A> {
    pub fn new(key: AppKey, app: A) -> Self {
        Self {
            key,
            app: Some(app),
        }
    }
}

impl<A: App> AppDriver for AppDriverImpl<A>
where
    A: 'static,
    A::Message: 'static,
    A::Output: 'static,
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

        self.app
            .as_mut()
            .expect("app has been stopped")
            .on_message(*message);
    }

    fn stop(&mut self) -> Box<dyn std::any::Any> {
        let app = self.app.take().expect("app has been already been stopped");

        Box::new(app.stop())
    }
}

pub fn driver_for<A>(key: AppKey, app: A) -> impl AppDriver
where
    A: App + 'static,
    A::Message: 'static,
    A::Output: 'static,
{
    AppDriverImpl::new(key, app)
}

/// The sender end of a channel for apps to send messages to themselves.
pub struct AppSender<M> {
    sender: smol::channel::Sender<AppMessage>,
    app_key: AppKey,
    data: PhantomData<M>,
}

impl<M> AppSender<M>
where
    M: Send + 'static,
{
    pub fn new(app_key: AppKey, sender: smol::channel::Sender<AppMessage>) -> AppSender<M> {
        Self {
            sender,
            app_key,
            data: Default::default(),
        }
    }

    /// Send a message to the App, which will be received by its [App::on_message] method.
    pub fn send(&self, message: M) {
        if let Err(_) = self.sender.try_send(AppMessage {
            app_key: self.app_key,
            message: Box::new(message),
        }) {
            log::error!("tried sending message to app, but the message receiver has been dropped: is polymodo dead?");
        }
    }
}

pub struct AppMessage {
    pub app_key: AppKey,
    pub message: Box<dyn std::any::Any + Send>,
}
