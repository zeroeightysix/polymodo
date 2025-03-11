use crate::app_surface_driver::{AppEvent, AppKey};
use local_channel::mpsc::SendError;
use std::future::Future;
use std::marker::PhantomData;
use tokio::task::JoinSet;

pub trait App: Sized {
    type Message;
    type Output;

    fn create(
        message_sender: AppSender<Self::Message>,
    ) -> AppSetup<Self, Self::Output>;

    #[allow(unused_variables)]
    fn on_message(&mut self, message: Self::Message) {
        // do nothing by default.
    }

    fn render(&mut self, ctx: &egui::Context);
}

pub struct AppSetup<A, O> {
    pub app: A,
    pub effects: JoinSet<O>,
}

impl<A, O: 'static> AppSetup<A, O> {
    pub fn new(app: A) -> Self {
        Self {
            app,
            effects: JoinSet::new(),
        }
    }

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

/// The sender end of a channel for apps to send messages to themselves.
pub struct AppSender<M> {
    sender: local_channel::mpsc::Sender<AppEvent>,
    app_key: AppKey,
    data: PhantomData<M>,
}

impl<M> AppSender<M>
where
    M: 'static,
{
    pub fn new(app_key: AppKey, sender: local_channel::mpsc::Sender<AppEvent>) -> AppSender<M> {
        Self {
            sender,
            app_key,
            data: Default::default(),
        }
    }

    /// Send a message to the App, which will be received by its [App::on_message] method.
    pub fn send(&self, message: M) -> Result<(), SendError<AppEvent>> {
        self.sender.send(AppEvent::AppMessage {
            app_key: self.app_key,
            message: Box::new(message),
        })
    }
}
