use crate::windowing::Windowing;
use crate::{app, WindowingError};
use smithay_client_toolkit::reexports::client::protocol::wl_surface;
use smithay_client_toolkit::reexports::client::EventQueue;
use std::io::ErrorKind;
use wayland_backend::client::WaylandError;

pub struct Client<A> {
    event_queue: EventQueue<Windowing<A>>,
    pub layer_windowing: Windowing<A>,
}

impl<A: app::App + 'static> Client<A> {
    pub async fn create(wgpu_setup: egui_wgpu::WgpuSetup, app: A) -> Result<Self, WindowingError> {
        let (event_queue, layer_windowing) = Windowing::create(wgpu_setup, app).await?;

        Ok(Self {
            event_queue,
            layer_windowing,
        })
    }

    pub async fn update(
        &mut self,
        surface: &wl_surface::WlSurface,
        repaint: bool,
    ) -> Result<(), WindowingError> {
        let eq = &mut self.event_queue;
        let windowing = &mut self.layer_windowing;
        let dispatched = eq.dispatch_pending(windowing)?;
        if dispatched > 0 {
            return Ok(());
        }

        eq.flush()?;

        windowing.render(surface, repaint)?;

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
