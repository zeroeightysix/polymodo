use crate::windowing::app::App;
use crate::windowing::surface::{Surface, SurfaceId};
use crate::windowing::client::{SurfaceEvent, SurfaceSetup};
use anyhow::Context;
use egui::ViewportId;
use rand::random;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

pub type AppKey = u32;

pub fn new_app_key() -> AppKey {
    random()
}

/// The `AppSurfaceDriver` is responsible for rendering `App`s, keeping track of which `App` has
/// created which surface, and using their `render` method to perform repaints on surfaces.
///
/// As polymodo has a number of sources that will want to cause a surface repaint, this struct is
/// generally used in a separate task and driven by a mpsc channel, hence the "Driver" in its name.
pub struct AppSurfaceDriver {
    apps: Vec<(AppKey, Box<dyn App>, egui::Context)>,
    // To perform the render requests, `Polymodo` needs to know which surfaces (or viewports) belong
    // to which apps.
    app_surface_map: Vec<(FullSurfaceId, AppKey)>, // `find` in a vec is faster for small quantities
    surface_setup: SurfaceSetup,
    surfaces: Vec<Surface>,
}

impl AppSurfaceDriver {
    pub fn create(surface_setup: SurfaceSetup) -> Self {
        Self {
            apps: Default::default(),
            app_surface_map: vec![],
            surface_setup,
            surfaces: vec![],
        }
    }

    pub async fn handle_event(&mut self, event: SurfaceEvent) -> anyhow::Result<()> {
        match event {
            SurfaceEvent::RepaintAllWithEvents => {
                let ids = self.surfaces
                    .iter()
                    .filter(|surf| surf.has_events())
                    .map(|surf| surf.surface_id())
                    .collect::<Vec<_>>();
                for surf_id in ids {
                    self.paint(&surf_id)?;
                }
                Ok(())
            }
            SurfaceEvent::NeedsRepaint(id) => self.paint(&id),
            SurfaceEvent::Closed(id) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.set_exit();
                Ok(())
            }
            SurfaceEvent::Configure(id, configure) => {
                let (width, height) = configure.new_size;
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.update_size(width, height);

                self.paint(&id)
            }
            SurfaceEvent::KeyboardFocus(id, focus) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.on_focus(focus);

                Ok(())
            }
            SurfaceEvent::PressKey(_, None, None) => { Ok(()) }, // no text and no key -> ignore.
            SurfaceEvent::PressKey(id, text, key) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                if let Some(text) = text {
                    surface.push_event(egui::Event::Text(text));
                }
                if let Some(key) = key {
                    surface.on_key(key, true);
                }
                Ok(())
            }
            SurfaceEvent::ReleaseKey(_, None) => { Ok(()) } // no key -> ignore
            SurfaceEvent::ReleaseKey(id, Some(key)) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.on_key(key, false);
                Ok(())
            }
            SurfaceEvent::UpdateModifiers(id, modifiers) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.set_modifiers(modifiers);
                Ok(())
            }
            SurfaceEvent::Pointer(id, pointer_event) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.handle_pointer_event(&pointer_event);
                Ok(())
            }
        }
    }

    async fn add_app(
        &mut self,
        app_key: AppKey,
        app: Box<dyn App>,
    ) -> anyhow::Result<()> {
        let viewport_id = ViewportId::ROOT;
        let initial_surface = self.surface_setup.create_surface(viewport_id, Default::default())
            .await?;
        let surface_id = initial_surface.surface_id();
        let fid = FullSurfaceId {
            viewport_id,
            surface_id,
        };

        self.surfaces.push(initial_surface);
        self.app_surface_map.push((fid, app_key));
        self.apps.push((app_key, app, Self::new_context()));
        
        Ok(())
    }
    
    fn new_context() -> egui::Context {
        // TODO
        egui::Context::default()
    }
    
    fn paint(&mut self, surface_id: &SurfaceId) -> anyhow::Result<()> {
        // this method is quite hideous because it mostly just duplicates code from the above
        // functions. unfortunately, they're inlined because the borrow checker (as of now)
        // is still quite dumb when deducing if functions return disjoint references to borrowed
        // data; so inlining is the easiest solution for that here.
        
        // surface = surface_by_id(surface_id);
        let surface = self
            .surfaces
            .iter_mut()
            .find(|surf| &surf.surface_id() == surface_id)
            .context("No such surface")?;

        // app_key = find_app_key_for_surf(surface_id)
        let app_key = self
            .app_surface_map
            .iter()
            .find(|(fsid, _)| &fsid.surface_id == surface_id)
            .map(|(_, app_key)| *app_key)
            .context("No such app")?;

        // get the app and corresponding context for rendering
        let (_, app, ref ctx) = self.apps.iter_mut()
            .find(|(key, _, _)| *key == app_key)
            .context("No such app")?;

        // and paint it.
        let render_ui = move |ctx: &egui::Context| {
            app.render(ctx);
        };
        surface.render(ctx, render_ui)?;

        Ok(())
    }

    fn find_app_key_for_surf(&self, id: &SurfaceId) -> Option<AppKey> {
        self.app_surface_map
            .iter()
            .find(|(fsid, _)| &fsid.surface_id == id)
            .map(|(_, app_key)| *app_key)
    }

    fn surface_by_id(&mut self, surface_id: &SurfaceId) -> Option<&mut Surface> {
        self.surfaces
            .iter_mut()
            .find(|surf| &surf.surface_id() == surface_id)
    }
}

pub fn create_surface_driver_task(
    mut event_receive: mpsc::Receiver<SurfaceEvent>,
    mut app_receive: local_channel::mpsc::Receiver<NewAppEvent>,
    surface_setup: SurfaceSetup
) -> JoinHandle<std::convert::Infallible> {
    tokio::task::spawn_local(async move {
        let mut driver = AppSurfaceDriver::create(surface_setup);

        fn die_horrific_death() -> ! {
            log::error!("surface driver task channel has closed: that's quite bad!");

            // if there's no one sending events to surfaces anymore,
            // that means the wayland dispatcher is dead,
            // so our application really has no business still being alive.
            std::process::exit(1);
        }
        
        async fn on_event(driver: &mut AppSurfaceDriver, event: SurfaceEvent) {
            if let Err(e) = driver.handle_event(event).await {
                log::error!("surface driver handle event error: {}", e);
            }
        }

        loop {
            tokio::select! {
                event = event_receive.recv() => {
                    let Some(event) = event else {
                        die_horrific_death()
                    };
                    
                    on_event(&mut driver, event).await;
                }
                event = app_receive.recv() => {
                    let Some(NewAppEvent {
                        app_key,
                        app
                    }) = event else {
                        die_horrific_death()
                    };
                    
                    if let Err(e) = driver.add_app(app_key, app).await {
                        // TODO: even though this is rare,
                        // we probably need some feedback here,
                        // to kill the app that wasn't able to spawn.
                        log::error!("failed to spawn the surface for app {app_key}; it will probably stay alive forever (this is a leak): {e}");
                    }
                }
            }
        }
    })
}

pub struct NewAppEvent {
    pub app_key: AppKey,
    pub app: Box<dyn App>,
}

#[derive(Debug, Clone)]
pub struct FullSurfaceId {
    pub surface_id: SurfaceId,
    pub viewport_id: ViewportId,
}
