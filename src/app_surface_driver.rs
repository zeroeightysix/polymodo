use crate::windowing::app::App;
use crate::windowing::client::{SurfaceEvent, SurfaceSetup};
use crate::windowing::surface::{LayerSurfaceOptions, Surface, SurfaceId};
use crate::windowing::WindowingError;
use anyhow::Context;
use egui::ViewportId;
use rand::random;
use smithay_client_toolkit::seat::keyboard::RepeatInfo;
use std::cell::Cell;
use std::sync::Mutex;
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::{AbortHandle, JoinHandle};

pub type AppKey = u32;

pub fn create_app_driver<A: App>(
    key: AppKey,
    app: A,
    surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
) -> impl AppDriver
where
    A::Message: 'static,
{
    AppDriverImpl {
        key,
        app,
        ctx: new_context(surf_driver_event_sender),
        last_rendered_pass: Cell::new(0),
    }
}

fn new_context(surf_driver_event_sender: mpsc::Sender<SurfaceEvent>) -> egui::Context {
    let context = egui::Context::default();
    let last_task: Mutex<Option<AbortHandle>> = Default::default();

    // set up the repaint callback.
    // egui will let us know when a repaint is required, optionally with a delay.
    // this logic handles that:
    context.set_request_repaint_callback(move |info| {
        let sender = surf_driver_event_sender.clone();

        // keep the handle to this task,
        let abort_handle = tokio::spawn(async move {
            tokio::time::sleep(info.delay).await;

            let _ = sender.try_send(SurfaceEvent::NeedsRepaintViewport(
                info.viewport_id,
                info.current_cumulative_pass_nr,
            ));
        })
        .abort_handle();

        // because we can safely abort the last one, and store the current (new) task as the last one.
        if let Some(handle) = last_task.lock().unwrap().replace(abort_handle) {
            handle.abort();
        }
    });

    context
}

pub fn new_app_key() -> AppKey {
    random()
}

/// The `AppSurfaceDriver` is responsible for rendering `App`s, keeping track of which `App` has
/// created which surface, and using their `render` method to perform repaints on surfaces.
///
/// As polymodo has a number of sources that will want to cause a surface repaint, this struct is
/// generally used in a separate task and driven by a mpsc channel, hence the "Driver" in its name.
pub struct AppSurfaceDriver {
    // apps: Vec<(AppKey, Box<dyn App>, egui::Context)>,
    apps: Vec<Box<dyn AppDriver>>,
    // To perform the render requests, `Polymodo` needs to know which surfaces (or viewports) belong
    // to which apps.
    app_surface_map: Vec<(FullSurfaceId, AppKey)>, // `find` in a vec is faster for small quantities
    surface_setup: SurfaceSetup,
    surfaces: Vec<Surface>,

    self_sender: mpsc::Sender<SurfaceEvent>,
    abort_repeat_task: Option<AbortHandle>,
    repeat_info: Option<RepeatInfo>,
}

impl AppSurfaceDriver {
    pub fn create(surf_driver_event_sender: mpsc::Sender<SurfaceEvent>, surface_setup: SurfaceSetup) -> Self {
        Self {
            apps: Default::default(),
            app_surface_map: vec![],
            surface_setup,
            surfaces: vec![],
            self_sender: surf_driver_event_sender,
            abort_repeat_task: None,
            repeat_info: None,
        }
    }

    pub async fn handle_event(&mut self, event: SurfaceEvent) -> anyhow::Result<()> {
        match event {
            SurfaceEvent::UpdateAllWithEvents => {
                let ids = self
                    .surfaces
                    .iter()
                    .filter(|surf| surf.has_events())
                    .map(|surf| surf.surface_id())
                    .collect::<Vec<_>>();
                for surf_id in ids {
                    self.update(&surf_id)?;
                }
                Ok(())
            }
            SurfaceEvent::NeedsRepaintSurface(id) => self.paint(&id, None),
            SurfaceEvent::NeedsRepaintViewport(vid, pass_nr) => {
                let id = self
                    .surface_id_by_viewport_id(vid)
                    .context("No such surface")?;
                self.paint(&id, Some(pass_nr))?;
                Ok(())
            }
            SurfaceEvent::Closed(id) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.set_exit();
                Ok(())
            }
            SurfaceEvent::Configure(id, configure) => {
                let (width, height) = configure.new_size;
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.update_size(width, height);

                self.paint(&id, None)
            }
            SurfaceEvent::KeyboardFocus(id, focus) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.on_focus(focus);

                Ok(())
            }
            SurfaceEvent::PressKey(_, None, None) => Ok(()), // no text and no key -> ignore.
            SurfaceEvent::PressKey(id, text, key) => {
                // set up the key repetition task
                if let Some(RepeatInfo::Repeat { rate, delay }) = self.repeat_info {
                    let id = id.clone();
                    let text = text.clone();
                    let sender = self.self_sender.clone();

                    let abort = tokio::spawn(async move {
                        // wait the initial delay,
                        tokio::time::sleep(Duration::from_millis(delay as u64)).await;

                        // and then start sending a RepeatKey event every sleep_inbetween.
                        let sleep_secs = 1f64 / rate.get() as f64;
                        let sleep_inbetween = Duration::from_secs_f64(sleep_secs);

                        loop {
                            let _ = sender.send(SurfaceEvent::RepeatKey(id.clone(), text.clone(), key)).await;
                            tokio::time::sleep(sleep_inbetween).await;
                        }
                    }).abort_handle();
                    // replace the abort handle and abort the last one
                    if let Some(handle) = self.abort_repeat_task.replace(abort) {
                        handle.abort();
                    }
                }

                let surface = self.surface_by_id(&id).context("No such surface")?;
                if let Some(text) = text {
                    surface.push_event(egui::Event::Text(text));
                }
                if let Some(key) = key {
                    surface.on_key(key, true);
                }
                Ok(())
            }
            SurfaceEvent::RepeatKey(id, text, key) => {
                // same as above ^^
                let surface = self.surface_by_id(&id).context("No such surface")?;
                if let Some(text) = text {
                    surface.push_event(egui::Event::Text(text));
                }
                if let Some(key) = key {
                    surface.on_key(key, true);
                }

                self.paint(&id, None)
            }
            SurfaceEvent::ReleaseKey(_, None) => Ok(()), // no key -> ignore
            SurfaceEvent::ReleaseKey(id, Some(key)) => {
                // if a key is released, stop the repetition task.
                // we don't bother to differentiate between which key was being repeated,
                // as this is an edge case we don't really care about and should be handled better
                // once layer_shell is landed in
                if let Some(abort) = self.abort_repeat_task.take() {
                    abort.abort();
                }

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
            SurfaceEvent::UpdateRepeatInfo(info) => {
                self.repeat_info = Some(info);
                Ok(())
            }
        }
    }

    async fn add_app(
        &mut self,
        app_driver: Box<dyn AppDriver>,
        layer_surface_options: LayerSurfaceOptions<'static>,
    ) -> anyhow::Result<()> {
        let app_key = app_driver.key();

        let viewport_id = ViewportId::ROOT;
        let initial_surface = self
            .surface_setup
            .create_surface(viewport_id, layer_surface_options)
            .await?;
        let surface_id = initial_surface.surface_id();
        let fid = FullSurfaceId {
            viewport_id,
            surface_id,
        };

        self.surfaces.push(initial_surface);
        self.app_surface_map.push((fid, app_key));
        self.apps.push(app_driver);

        Ok(())
    }

    fn on_app_message(
        &mut self,
        app_key: AppKey,
        message: Box<dyn std::any::Any>,
    ) -> anyhow::Result<()> {
        let driver = self
            .apps
            .iter_mut()
            .find(|driver| driver.key() == app_key)
            .context("No such app")?;

        driver.on_message(message);

        // After processing a message, redraw the app, assuming its contents have been changed.
        // first: find the surfaces for this app
        let app_key = driver.key();
        let ids = self
            .app_surface_map
            .iter()
            .filter(|(_, key)| *key == app_key)
            .map(|(fid, _)| &fid.surface_id)
            .collect::<Vec<_>>();
        for surface in &mut self.surfaces {
            if ids.contains(&&surface.surface_id()) {
                driver.paint(surface, None)?
            }
        }

        Ok(())
    }

    fn with_app_surf_mut<R>(
        &mut self,
        surface_id: &SurfaceId,
        writer: impl FnOnce(&mut Box<dyn AppDriver>, &mut Surface) -> R,
    ) -> anyhow::Result<R> {
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
        let app = self
            .apps
            .iter_mut()
            .find(|app| app.key() == app_key)
            .context("No such app")?;

        Ok(writer(app, surface))
    }

    fn paint(&mut self, surface_id: &SurfaceId, pass_nr: Option<u64>) -> anyhow::Result<()> {
        self.with_app_surf_mut(surface_id, |app, surf| app.paint(surf, pass_nr))??;

        Ok(())
    }

    fn update(&mut self, surface_id: &SurfaceId) -> anyhow::Result<()> {
        self.with_app_surf_mut(surface_id, |app, surf| app.update(surf))
    }

    fn surface_by_id(&mut self, surface_id: &SurfaceId) -> Option<&mut Surface> {
        self.surfaces
            .iter_mut()
            .find(|surf| &surf.surface_id() == surface_id)
    }

    fn surface_id_by_viewport_id(&self, viewport_id: ViewportId) -> Option<SurfaceId> {
        self.app_surface_map
            .iter()
            .find(|(fid, _)| fid.viewport_id == viewport_id)
            .map(|(fid, _)| fid.surface_id.clone())
    }
}

/// Trait to 'drive' apps, being, to call their render and on_message methods on our behalf.
///
/// This serves to provide a dyn compatible trait for `AppSurfaceDriver` to use, as `App` itself
/// has GATs that make it dyn incompatible.
pub trait AppDriver {
    fn key(&self) -> AppKey;

    fn update(&mut self, surface: &mut Surface);

    fn paint(&mut self, surface: &mut Surface, pass_nr: Option<u64>) -> Result<(), WindowingError>;

    fn on_message(&mut self, message: Box<dyn std::any::Any>);
}

struct AppDriverImpl<A: App> {
    key: AppKey,
    app: A,
    ctx: egui::Context,
    last_rendered_pass: Cell<u64>,
}

impl<A: App> AppDriver for AppDriverImpl<A>
where
    A::Message: 'static,
{
    fn key(&self) -> AppKey {
        self.key
    }

    fn update(&mut self, surface: &mut Surface) {
        let _full_output = surface.update(&self.ctx, |ctx: &egui::Context| {
            self.app.render(ctx);
        });
        // TODO: handle the full_output
    }

    fn paint(&mut self, surface: &mut Surface, pass_nr: Option<u64>) -> Result<(), WindowingError> {
        // If a pass number has been provided, we should skip painting in case the pass number
        // has already passed. This is an optimization to reduce redundant paints.
        if let Some(pass_nr) = pass_nr {
            let last_pass = self.last_rendered_pass.get();
            if last_pass >= pass_nr {
                return Ok(());
            }
        };

        let _output = surface.render(&self.ctx, |ctx: &egui::Context| {
            self.app.render(ctx);
        })?;

        // TODO: handle the output

        Ok(())
    }

    fn on_message(&mut self, message: Box<dyn std::any::Any>) {
        let Ok(message) = message.downcast() else {
            return;
        };

        self.app.on_message(*message);
    }
}

pub fn create_surface_driver_task(
    surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
    mut event_receive: mpsc::Receiver<SurfaceEvent>,
    mut app_receive: local_channel::mpsc::Receiver<AppEvent>,
    surface_setup: SurfaceSetup,
) -> JoinHandle<std::convert::Infallible> {
    tokio::task::spawn_local(async move {
        let mut driver = AppSurfaceDriver::create(surf_driver_event_sender, surface_setup);

        fn die_horrific_death() -> ! {
            log::error!("surface driver task channel has closed: that's quite bad!");

            // if there's no one sending events to surfaces anymore,
            // that means the wayland dispatcher is dead,
            // so our application really has no business still being alive.
            std::process::exit(1);
        }

        async fn on_surface_event(driver: &mut AppSurfaceDriver, event: SurfaceEvent) {
            if let Err(e) = driver.handle_event(event).await {
                log::error!("surface driver handle event error: {}", e);
            }
        }

        async fn on_app_event(driver: &mut AppSurfaceDriver, event: AppEvent) {
            match event {
                AppEvent::NewApp {
                    app_driver,
                    layer_surface_options,
                } => {
                    let app_key = app_driver.key();
                    if let Err(e) = driver.add_app(app_driver, layer_surface_options).await {
                        // TODO: even though this is rare,
                        // we probably need some feedback here,
                        // to kill the app that wasn't able to spawn.
                        log::error!("failed to spawn the surface for app {app_key}; it will probably stay alive forever (this is a leak): {e}");
                    }
                }
                AppEvent::AppMessage { app_key, message } => {
                    if driver.on_app_message(app_key, message).is_err() {
                        log::error!("could not deliver message to app {app_key}; this is a bug.");
                    }
                }
            }
        }

        loop {
            tokio::select! {
                event = event_receive.recv() => {
                    let Some(event) = event else {
                        die_horrific_death()
                    };

                    on_surface_event(&mut driver, event).await;
                }
                event = app_receive.recv() => {
                    let Some(app_event) = event else {
                        die_horrific_death()
                    };

                    on_app_event(&mut driver, app_event).await;
                }
            }
        }
    })
}

pub enum AppEvent {
    /// Create a new app from its driver and spawn its initial surface.
    NewApp {
        app_driver: Box<dyn AppDriver>,
        layer_surface_options: LayerSurfaceOptions<'static>,
    },
    /// An App sent a message to itself, which necessitates an `on_message` call from the driver
    AppMessage {
        app_key: AppKey,
        message: Box<dyn std::any::Any>,
    },
}

#[derive(Debug, Clone)]
pub struct FullSurfaceId {
    pub surface_id: SurfaceId,
    pub viewport_id: ViewportId,
}
