use crate::live_handle::LiveHandle;
use crate::windowing::app::App;
use crate::windowing::client::{SurfaceEvent, SurfaceSetup};
use crate::windowing::surface::{LayerSurfaceOptions, ScaleFactor, Surface, SurfaceId};
use crate::windowing::{app, WindowingError};
use anyhow::Context;
use egui::ViewportId;
use rand::random;
use smallvec::{smallvec, SmallVec};
use smithay_client_toolkit::seat::keyboard::RepeatInfo;
use smithay_client_toolkit::seat::pointer::PointerEventKind;
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
    A: 'static,
    A::Message: 'static,
{
    AppDriverImpl {
        key,
        app,
        ctx: new_context(key, surf_driver_event_sender),
        last_rendered_pass: Cell::new(0),
    }
}

fn new_context(
    for_key: AppKey,
    surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
) -> egui::Context {
    let context = egui::Context::default();
    // the previous repaint task, if any.
    // note that this uses LiveHandle, meaning that if the `Context` is dropped, it drops the callback,
    // which drops this mutex, which drops the LiveHandle, which aborts the task!
    // this is especially useful as it prevents us from sending a `NeedsRepaintViewport` event for
    // a surface that has since been destroyed.
    let last_task: Mutex<Option<LiveHandle>> = Default::default();

    // set up the repaint callback.
    // egui will let us know when a repaint is required, optionally with a delay.
    // this logic handles that:
    context.set_request_repaint_callback(move |info| {
        let sender = surf_driver_event_sender.clone();

        // keep the handle to this task
        // this may run on a file loading thread from egui_extras, so we have to explicitly launch this task on the runtime instead of using tokio::spawn
        let abort_handle = crate::runtime()
            .spawn(async move {
                if !info.delay.is_zero() {
                    tokio::time::sleep(info.delay).await;
                }

                let _ = sender.try_send(SurfaceEvent::NeedsRepaintViewport(
                    for_key,
                    info.viewport_id,
                    info.current_cumulative_pass_nr,
                ));
            })
            .into();

        // because we can safely abort the last one, and store the current (new) task as the last one.
        if let Some(handle) = last_task.lock().unwrap().replace(abort_handle) {
            handle.abort();
        }
    });

    // in order to display icons, we need to load their images.
    // egui requires us to opt into the different type of image loaders, which we do here:
    egui_extras::install_image_loaders(&context);

    const ZOOM_FACTOR: f32 = 2.0;

    context.set_theme(egui::Theme::Dark);
    context.all_styles_mut(|style| {
        for (_, font_id) in style.text_styles.iter_mut() {
            font_id.size *= ZOOM_FACTOR;
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
    pub fn create(
        surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
        surface_setup: SurfaceSetup,
    ) -> Self {
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
                    self.with_app_surf_mut(&surf_id, |app, surf| {
                        app.request_repaint(surf.viewport_id())
                    })?;
                }
                Ok(())
            }
            SurfaceEvent::NeedsRepaintSurface(id) => self.paint(&id, None),
            SurfaceEvent::NeedsRepaintViewport(key, vid, pass_nr) => {
                let id = self
                    .surface_id_by_viewport_id(key, vid)
                    .context("No such surface")?;
                self.paint(&id, Some(pass_nr))?;
                Ok(())
            }
            SurfaceEvent::Closed(id) => {
                log::debug!("surface {id:?} closed");

                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.set_exit();
                Ok(())
            }
            SurfaceEvent::Configure(id, configure) => {
                let (width, height) = configure.new_size;
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.set_unscaled_size(width, height);

                self.paint(&id, None)
            }
            SurfaceEvent::KeyboardFocus(id, focus) => {
                self.with_app_surf_mut(&id, |app, surface| {
                    surface.on_focus(focus);

                    let viewport_id = surface.viewport_id();
                    let event = if focus {
                        app::SurfaceEvent::KeyboardEnter(viewport_id)
                    } else {
                        app::SurfaceEvent::KeyboardLeave(viewport_id)
                    };

                    app.on_surface_event(event);
                })?;

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
                            let _ = sender
                                .send(SurfaceEvent::RepeatKey(id.clone(), text.clone(), key))
                                .await;
                            tokio::time::sleep(sleep_inbetween).await;
                        }
                    })
                    .abort_handle();
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
                    surface.on_key(key, true, false);
                }
                Ok(())
            }
            SurfaceEvent::RepeatKey(id, text, key) => {
                // same as above ^^
                self.with_app_surf_mut(&id, |app, surface| {
                    if let Some(text) = text {
                        surface.push_event(egui::Event::Text(text));
                    }
                    if let Some(key) = key {
                        surface.on_key(key, true, true);
                    }

                    app.request_repaint(surface.viewport_id());
                })
            }
            SurfaceEvent::ReleaseKey(_, None) => {
                // no key -> ignore
                self.cancel_repetition_task(); // but do cancel the repetition task

                Ok(())
            }
            SurfaceEvent::ReleaseKey(id, Some(key)) => {
                // if a key is released, stop the repetition task.
                // we don't bother to differentiate between which key was being repeated,
                // as this is an edge case we don't really care about and should be handled better
                // once layer_shell is landed in
                self.cancel_repetition_task();

                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.on_key(key, false, false);
                Ok(())
            }
            SurfaceEvent::UpdateModifiers(id, modifiers) => {
                let surface = self.surface_by_id(&id).context("No such surface")?;
                surface.set_modifiers(modifiers);
                Ok(())
            }
            SurfaceEvent::Pointer(id, pointer_event) => {
                self.with_app_surf_mut(&id, |app, surface| {
                    surface.handle_pointer_event(&pointer_event);

                    let viewport_id = surface.viewport_id();
                    let event = match pointer_event.kind {
                        PointerEventKind::Enter { .. } => {
                            Some(app::SurfaceEvent::PointerEnter(viewport_id))
                        }
                        PointerEventKind::Leave { .. } => {
                            Some(app::SurfaceEvent::PointerLeave(viewport_id))
                        }
                        _ => None,
                    };

                    if let Some(event) = event {
                        app.on_surface_event(event);
                    }
                })?;
                
                Ok(())
            }
            SurfaceEvent::UpdateRepeatInfo(info) => {
                self.repeat_info = Some(info);
                Ok(())
            }
            SurfaceEvent::Scale(surface, scale) => {
                let scale = match scale {
                    ScaleFactor::Scalar(factor) => factor as f32,
                    ScaleFactor::Fractional(factor) => factor,
                };

                self.with_app_surf_mut(&surface, |app, surf| {
                    app.set_scale(scale, surf);
                })
            }
        }
    }

    fn cancel_repetition_task(&mut self) {
        if let Some(abort) = self.abort_repeat_task.take() {
            abort.abort();
        }
    }

    /// Add an app.
    ///
    /// This
    /// - creates the app's initial surface, which is assigned the ROOT viewport id.
    /// - adds that surface to `surfaces`
    /// - adds a mapping for that surface to `app_surface_map`
    /// - adds the driver to `apps`
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

    fn remove_app(&mut self, app_key_to_remove: AppKey) {
        // start by collecting the surfaces for this app.
        // we need to be careful that, for each surface removed, we
        // - remove it from `surfaces`, `app_surface_map`, and destroy the surface properly.
        let mut associated_surfaces: SmallVec<[SurfaceId; 1]> = smallvec![];
        self.app_surface_map.retain(|(fid, app_key)| {
            if *app_key == app_key_to_remove {
                associated_surfaces.push(fid.surface_id.clone());
                // this surface is going to be removed, so do not retain the entry.
                false
            } else {
                // retain this entry.
                true
            }
        });

        self.surfaces.retain(|surf| {
            if associated_surfaces.contains(&surf.surface_id()) {
                // drop this surface, which should clean it up.
                false
            } else {
                // do not remove this surface.
                true
            }
        });

        self.apps.retain(|driver| {
            if driver.key() == app_key_to_remove {
                // this is the driver for the to-remove app: do not keep it.
                false
            } else {
                // this is another driver, which must be retained.
                true
            }
        });

        // hack: finally, it is quite likely that the current repeat task was
        // bound to the just-destroyed surface. we'll have to abort it, otherwise it will keep
        // sending key repeat events for a surface that doesn't exist anymore!
        if let Some(abort) = self.abort_repeat_task.take() {
            abort.abort();
        }
    }

    /// Is an App of the type `A` running on this AppSurfaceDriver?
    fn is_running(&self, id: &str) -> bool {
        self.apps.iter().any(|app| app.app_type() == id)
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

    fn surface_by_id(&mut self, surface_id: &SurfaceId) -> Option<&mut Surface> {
        self.surfaces
            .iter_mut()
            .find(|surf| &surf.surface_id() == surface_id)
    }

    fn surface_id_by_viewport_id(
        &self,
        app_key: AppKey,
        viewport_id: ViewportId,
    ) -> Option<SurfaceId> {
        self.app_surface_map
            .iter()
            .find(|(fid, key)| app_key == *key && fid.viewport_id == viewport_id)
            .map(|(fid, _)| fid.surface_id.clone())
    }
}

/// Trait to 'drive' apps, being, to call their render and on_message methods on our behalf.
///
/// This serves to provide a dyn compatible trait for `AppSurfaceDriver` to use, as `App` itself
/// has GATs that make it dyn incompatible.
pub trait AppDriver {
    fn key(&self) -> AppKey;

    fn app_type(&self) -> &'static str;

    fn request_repaint(&self, viewport_id: ViewportId);

    fn paint(&mut self, surface: &mut Surface, pass_nr: Option<u64>) -> Result<(), WindowingError>;

    fn on_message(&mut self, message: Box<dyn std::any::Any>);
    fn on_surface_event(&mut self, surface_event: app::SurfaceEvent);

    fn set_scale(&mut self, scale: f32, surf: &mut Surface);
}

struct AppDriverImpl<A: App> {
    key: AppKey,
    app: A,
    ctx: egui::Context,
    last_rendered_pass: Cell<u64>,
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

    fn request_repaint(&self, viewport_id: ViewportId) {
        self.ctx.request_repaint_of(viewport_id);
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

    fn on_surface_event(&mut self, surface_event: app::SurfaceEvent) {
        self.app.on_surface_event(surface_event)
    }

    fn set_scale(&mut self, scale: f32, surf: &mut Surface) {
        self.ctx.set_zoom_factor(scale);
        surf.set_scale(scale);
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
                AppEvent::DestroyApp { app_key } => {
                    driver.remove_app(app_key);
                }
                AppEvent::AppMessage { app_key, message } => {
                    if driver.on_app_message(app_key, message).is_err() {
                        log::error!("could not deliver message to app {app_key}; this is a bug.");
                    }
                }
                AppEvent::AppExistsQuery {
                    app_type_id,
                    response,
                } => {
                    let running = driver.is_running(&app_type_id);
                    let _ = response.send(running);
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
    /// An app has finished and should be removed.
    DestroyApp { app_key: AppKey },
    /// An App sent a message to itself, which necessitates an `on_message` call from the driver
    AppMessage {
        app_key: AppKey,
        message: Box<dyn std::any::Any>,
    },
    /// A query asking if an app with a given type id is running, along with a channel for the
    /// response.
    AppExistsQuery {
        app_type_id: String,
        response: tokio::sync::oneshot::Sender<bool>,
    },
}

#[derive(Debug, Clone)]
pub struct FullSurfaceId {
    pub surface_id: SurfaceId,
    pub viewport_id: ViewportId,
}
