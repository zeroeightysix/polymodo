use crate::surface::{FullSurfaceId, LayerSurfaceOptions, SurfaceId};
use crate::windowing::{DispatcherRequest, Windowing};
use crate::WindowingError;
use egui::ViewportId;
use smithay_client_toolkit::reexports::client::EventQueue;
use std::io::ErrorKind;
use tokio::sync::mpsc;
use wayland_backend::client::WaylandError;

pub struct Client {
    event_queue: EventQueue<Windowing>,
    sender: mpsc::Sender<crate::windowing::DispatcherRequest>,
    pub windowing: Windowing,
}

impl Client {
    pub async fn create(wgpu_setup: egui_wgpu::WgpuSetup, sender: mpsc::Sender<crate::windowing::DispatcherRequest>) -> Result<Self, WindowingError> {
        let (event_queue, windowing) = Windowing::create(wgpu_setup, sender.clone()).await?;

        Ok(Self {
            event_queue,
            sender,
            windowing,
        })
    }

    pub async fn update(&mut self) -> Result<(), WindowingError> {
        let eq = &mut self.event_queue;
        let windowing = &mut self.windowing;
        let dispatched = eq.dispatch_pending(windowing)?;
        if dispatched > 0 {
            return Ok(());
        }

        eq.flush()?;

        self.windowing.surfaces()
            .filter(|surf| surf.has_events())
            .map(|surf| surf.surface_id())
            .for_each(|id| {
                let _ = self.sender.try_send(DispatcherRequest::RepaintSurface(id));
            });

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

    pub fn repaint_surface(&mut self, surface_id: SurfaceId, ctx: &egui::Context, render_ui: impl FnMut(&egui::Context)) {
        self.windowing.repaint_surface(surface_id, ctx, render_ui);
    }

    pub async fn create_surface(
        &mut self,
        viewport_id: ViewportId,
        options: LayerSurfaceOptions<'_>,
    ) -> Result<FullSurfaceId, WindowingError> {
        self.windowing.create_surface(viewport_id, options).await
    }
}
