use std::marker::PhantomData;
use local_channel::mpsc::SendError;
use crate::app_surface_driver::{AppEvent, AppKey};

pub trait App {
    type Message;
    
    // fn create(message_sender: local_channel::mpsc::Sender<Self::Message>) -> Self; 
    
    #[allow(unused_variables)]
    fn on_message(&mut self, message: Self::Message) {
        // do nothing by default.
    }
    
    fn render(&mut self, ctx: &egui::Context);
}

pub struct AppSender<M> {
    sender: local_channel::mpsc::Sender<AppEvent>,
    app_key: AppKey,
    data: PhantomData<M>
}

impl<M> AppSender<M> where M: 'static {
    pub fn new(app_key: AppKey, sender: local_channel::mpsc::Sender<AppEvent>) -> AppSender<M> {
        Self {
            sender,
            app_key,
            data: Default::default(),
        }
    }
    
    pub fn send(&self, message: M) -> Result<(), SendError<AppEvent>> {
        self.sender.send(AppEvent::AppMessage {
            app_key: self.app_key,
            message: Box::new(message)
        })
    }
}