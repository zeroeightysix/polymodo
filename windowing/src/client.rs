use wayland_backend::client::WaylandError;
use std::io::ErrorKind;
use smithay_client_toolkit::reexports::client::EventQueue;
use crate::{app, LayerShellOptions, LayerWindowing, LayerWindowingError};

pub struct Client<A> {
    event_queue: EventQueue<LayerWindowing<A>>,
    layer_windowing: LayerWindowing<A>,
}

impl<A: app::App + 'static> Client<A> {
    pub async fn create(
        options: LayerShellOptions<'_>,
        app: A
    ) -> Result<Self, LayerWindowingError> {
        let (event_queue, layer_windowing) = LayerWindowing::create(options, app).await?;

        Ok(Self {
            event_queue,
            layer_windowing,
        })
    }

    pub async fn update(&mut self, repaint: bool) -> Result<(), LayerWindowingError> {
        let eq = &mut self.event_queue;
        let windowing = &mut self.layer_windowing;
        let dispatched = eq.dispatch_pending(windowing)?;
        if dispatched > 0 {
            return Ok(());
        }

        eq.flush()?;

        if repaint || !windowing.events.is_empty() || windowing.ctx.has_requested_repaint() {
            windowing.render()?;
        }

        if let Some(events) = eq.prepare_read() {
            let fd = events.connection_fd().try_clone_to_owned()?;
            let async_fd = tokio::io::unix::AsyncFd::new(fd)?;
            let mut ready_guard = async_fd.readable().await?;
            match events.read() {
                Ok(_) => {
                    ready_guard.clear_ready();
                }
                Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
                Err(e) => Err(e)?,
            }
            drop(ready_guard);
        }

        Ok(())
    }

    pub fn app(&mut self) -> &mut A {
        &mut self.layer_windowing.app
    }
}
