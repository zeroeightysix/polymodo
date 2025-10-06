use std::future::Future;
use bincode::{Decode, Encode};
use std::marker::PhantomData;
use std::mem::MaybeUninit;
use smol::channel::TrySendError;

pub type AppKey = u32;

pub fn new_app_key() -> AppKey {
    rand::random()
}

pub trait App: Sized {
    type Message;
    type Output;

    const NAME: AppName;

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
pub trait AppDriver {
    // TODO: can we get rid of this?
    fn key(&self) -> AppKey;

    fn app_name(&self) -> AppName;

    fn add_abortable(&mut self, abortable: AbortOnDrop);

    fn on_message(&mut self, message: Box<dyn std::any::Any>);

    /// Stop the driven application. This mirrors [App]'s `stop` function, but is non-consuming.
    /// This is because `AppDriver` is meant to be used as a dynamic trait object, on which methods
    /// accepting `self` (instead of a reference) cannot be called.
    ///
    /// Panics if called twice.
    fn stop(&mut self) -> Box<dyn std::any::Any + Send>;
}

struct AppDriverImpl<A> {
    key: AppKey,
    app: Option<A>,
    abortables: Vec<AbortOnDrop<>>
}

impl<A> AppDriverImpl<A> {
    pub fn new(key: AppKey, app: A) -> Self {
        Self {
            key,
            app: Some(app),
            abortables: Vec::new(),
        }
    }
}

impl<A: App> AppDriver for AppDriverImpl<A>
where
    A: 'static,
    A::Message: 'static,
    A::Output: 'static + Send,
{
    fn key(&self) -> AppKey {
        self.key
    }

    fn app_name(&self) -> AppName {
        A::NAME
    }

    fn add_abortable(&mut self, abortable: AbortOnDrop) {
        self.abortables.push(abortable);
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

    fn stop(&mut self) -> Box<dyn std::any::Any + Send> {
        let app = self.app.take().expect("app has been already been stopped");

        Box::new(app.stop())
    }
}

pub fn driver_for<A>(key: AppKey, app: A) -> impl AppDriver
where
    A: App + 'static,
    A::Message: 'static,
    A::Output: 'static + Send,
{
    AppDriverImpl::new(key, app)
}

/// The sender end of a channel for apps to send messages to themselves.
#[derive(Clone)]
pub struct AppSender<M> {
    sender: smol::channel::Sender<AppEvent>,
    app_key: AppKey,
    data: PhantomData<M>,
}

impl<M> AppSender<M>
where
    M: Send + 'static,
{
    pub fn new(app_key: AppKey, sender: smol::channel::Sender<AppEvent>) -> AppSender<M> {
        Self {
            sender,
            app_key,
            data: Default::default(),
        }
    }

    fn send_event(&self, message: AppMessage) -> Result<(), TrySendError<AppEvent>> {
        self.sender.try_send(AppEvent {
            app_key: self.app_key,
            message,
        })
    }

    pub fn spawn<T: 'static>(&self, fut: impl Future<Output = T> + 'static) {
        let join_handle = slint::spawn_local(fut)
            .expect("an event loop");
        let message = AppMessage::SpawnLocal(AbortOnDrop::new(Box::new(join_handle)));

        if self.send_event(message).is_err() {
            log::error!("tried sending a task to polymodo, but the message receiver has been dropped; is polymodo dead?");
        };
    }

    /// Send a message to the App, which will be received by its [App::on_message] method.
    pub fn send(&self, message: M) {
        if self.send_event(AppMessage::Message(Box::new(message))).is_err() {
            log::error!("tried sending message to app, but the message receiver has been dropped: is polymodo dead?");
        }
    }

    pub fn finish(&self) {
        self.send_event(AppMessage::Finished)
            .expect("could not send message to polymodo");
    }
}

pub struct AppEvent {
    pub app_key: AppKey,
    pub message: AppMessage,
}

pub enum AppMessage {
    /// App requests to be stopped
    Finished,
    /// Message to app
    Message(Box<dyn std::any::Any + Send>),
    /// App spawned a task and wishes for the runtime to manage it
    SpawnLocal(AbortOnDrop)
}

pub trait Abortable {
    fn abort(&self);
}

impl<T> Abortable for slint::JoinHandle<T> {
    fn abort(&self) {
        // yeah
        let mut copy: MaybeUninit<slint::JoinHandle<T>> = MaybeUninit::uninit();
        let dst = copy.as_mut_ptr();

        let copy = unsafe {
            std::ptr::copy(self as *const _, dst, 1);

            copy.assume_init()
        };

        copy.abort()
    }
}

pub struct AbortOnDrop(Option<Box<dyn Abortable>>);

impl AbortOnDrop {
    pub fn new(value: Box<dyn Abortable>) -> Self {
        Self(Some(value))
    }
}

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        if let Some(s) = self.0.take() {
            s.abort();
        }
    }
}

#[derive(Debug, Copy, Clone, PartialEq, Eq, Decode, Encode)]
pub enum AppName {
    Launcher,
}
