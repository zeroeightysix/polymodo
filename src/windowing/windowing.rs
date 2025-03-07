use crate::windowing::surface::{FullSurfaceId, LayerSurfaceOptions, Surface, SurfaceId};
use crate::windowing::{convert, WindowingError};
use egui::ahash::HashMap;
use egui::ViewportId;
use egui_wgpu::{RenderState, WgpuSetup};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::{
    globals, protocol, Connection, EventQueue, Proxy, QueueHandle,
};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    KeyboardInteractivity, LayerShell, LayerShellHandler, LayerSurface, LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, registry_handlers,
};
use std::ptr::NonNull;
use tokio::sync::mpsc;
use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};

pub struct Windowing {
    connection: Connection,
    compositor: CompositorState,
    layer_shell: LayerShell,
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,
    qh: QueueHandle<Self>,

    instance: wgpu::Instance,

    keyboard: Option<protocol::wl_keyboard::WlKeyboard>,
    keyboard_entered_surface: Option<protocol::wl_surface::WlSurface>,
    pointer: Option<protocol::wl_pointer::WlPointer>,
    start_time: std::time::Instant,

    surfaces: HashMap<SurfaceId, Surface>,
    dispatch_sender: mpsc::Sender<DispatcherRequest>,
}

impl Windowing {
    pub async fn create(
        wgpu_setup: WgpuSetup,
        sender: mpsc::Sender<DispatcherRequest>,
    ) -> Result<(EventQueue<Self>, Self), WindowingError> {
        let connection = Connection::connect_to_env().map_err(|_| WindowingError::NotWayland)?;
        let (globals, event_queue) = globals::registry_queue_init(&connection)?;
        let qh: QueueHandle<Windowing> = event_queue.handle();

        let compositor = CompositorState::bind(&globals, &qh).unwrap();
        let layer_shell =
            LayerShell::bind(&globals, &qh).map_err(|_| WindowingError::NoLayerShell)?;

        // create the wgpu instance from provided setup config
        let instance = wgpu_setup.new_instance().await;

        let state = Windowing {
            connection,
            compositor,
            layer_shell,
            registry_state: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            qh,
            instance,
            keyboard: None,
            keyboard_entered_surface: None,
            pointer: None,

            start_time: std::time::Instant::now(),
            surfaces: Default::default(),
            dispatch_sender: sender,
        };

        Ok((event_queue, state))
    }

    pub async fn create_surface(
        &mut self,
        viewport_id: ViewportId,
        LayerSurfaceOptions {
            wgpu_options,
            layer,
            namespace,
            anchor,
            width,
            height,
        }: LayerSurfaceOptions<'_>,
    ) -> Result<FullSurfaceId, WindowingError> {
        let Self { qh, instance, .. } = &self;

        // create a new wayland surface and assign the layer_shell role
        let wl_surface = self.compositor.create_surface(qh);
        let wl_surface_id = wl_surface.id();
        let layer_surface = self
            .layer_shell
            .create_layer_surface(qh, wl_surface, layer, namespace, None);

        // set up layer_shell options as provided
        layer_surface.set_anchor(anchor);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer_surface.set_size(width, height);
        layer_surface.commit();

        // create the wgpu surface (handle to all graphics related stuff on this wayland surface)
        // SAFETY: the raw window handles constructed are always created by us, and we know that
        // they're pointers to the correct types
        let wgpu_surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    NonNull::new(self.connection.backend().display_ptr() as *mut std::ffi::c_void)
                        .unwrap(),
                )),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    NonNull::new(wl_surface_id.as_ptr() as *mut std::ffi::c_void).unwrap(),
                )),
            })?
        };

        // set up the egui render state
        let render_state =
            RenderState::create(&wgpu_options, instance, Some(&wgpu_surface), None, 1, true)
                .await?;
        let surface_id: SurfaceId = wl_surface_id.into();
        let full_id = FullSurfaceId {
            surface_id: surface_id.clone(),
            viewport_id,
        };

        let surface = Surface::create(
            full_id.clone(),
            (width, height),
            layer_surface,
            self.start_time,
            wgpu_surface,
            render_state,
        );

        // set up the surface for rendering given the default size
        // new_surface.configure_surface();

        // finally, set up the handle and insert it into our internal store of surfaces
        self.surfaces.insert(surface_id, surface);

        Ok(full_id)
    }

    pub fn repaint_surface(
        &mut self,
        surface_id: SurfaceId,
        ctx: &egui::Context,
        render_ui: impl FnMut(&egui::Context),
    ) {
        self.with_surface_mut(surface_id, |surf| {
            match surf.render(ctx, render_ui) {
                Ok(_) => {}
                Err(e) => {
                    log::error!("could not repaint surface, {}", e);
                }
            };
        });
    }

    pub(crate) fn surfaces(&self) -> impl Iterator<Item = &Surface> {
        self.surfaces.values()
    }

    fn with_surface_mut<R>(
        &mut self,
        id: SurfaceId,
        f: impl FnOnce(&mut Surface) -> R,
    ) -> Option<R> {
        let surf = self.surfaces.get_mut(&id)?; //.and_then(|weak| weak.upgrade())?;
                                                // let mut surf = surf.lock().unwrap();

        Some(f(&mut *surf))
    }

    fn ask_to_repaint(&self, surface: SurfaceId) {
        match self
            .dispatch_sender
            .try_send(DispatcherRequest::RepaintSurface(surface))
        {
            Ok(_) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                log::error!(
                    "could not repaint surface, as the buffer for asking to do so, is full!"
                )
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {
                log::error!("god has abandoned us");
            }
        }
    }

    // // TODO: not very pretty
    // pub fn render(
    //     &mut self,
    //     surface: &protocol::wl_surface::WlSurface,
    //     repaint: bool,
    // ) -> Result<(), WindowingError> {
    //     let Some(surface) = self.surfaces.get_mut(surface) else {
    //         return Err(WindowingError::NoSuchSurface);
    //     };
    //
    //     if repaint || !surface.events.is_empty() || surface.ctx.has_requested_repaint() {
    //         surface.render(&mut self.app)?;
    //     }
    //     Ok(())
    // }
}

impl CompositorHandler for Windowing {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        // TODO: does egui have a scale?
    }

    fn transform_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _new_transform: protocol::wl_output::Transform,
    ) {
        // Not needed for this example.
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &protocol::wl_surface::WlSurface,
        _time: u32,
    ) {
        log::trace!("frame");

        self.ask_to_repaint(surface.into());
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _output: &protocol::wl_output::WlOutput,
    ) {
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _output: &protocol::wl_output::WlOutput,
    ) {
    }
}

impl OutputHandler for Windowing {
    fn output_state(&mut self) -> &mut OutputState {
        &mut self.output_state
    }

    fn new_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: protocol::wl_output::WlOutput,
    ) {
    }

    fn update_output(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: protocol::wl_output::WlOutput,
    ) {
    }

    fn output_destroyed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _output: protocol::wl_output::WlOutput,
    ) {
    }
}

impl LayerShellHandler for Windowing {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.with_surface_mut(layer.wl_surface().into(), |surface| {
            surface.set_exit();
        })
        .unwrap_or_else(|| {
            log::error!("closed event for unknown surface");
        });
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let id: SurfaceId = layer.wl_surface().into();
        let redraw = self
            .with_surface_mut(id.clone(), |surface| {
                log::trace!(
                    "configure {:?} (first: {})",
                    configure,
                    surface.is_first_configure()
                );

                let (width, height) = configure.new_size;
                surface.update_size(width, height);

                // Initiate the first draw, if applicable.
                // TODO: surface.first_draw(&mut self.app);
                surface.is_first_configure()
            })
            .unwrap_or_else(|| {
                log::error!("configure event for unknown surface");
                false
            });

        if redraw {
            self.ask_to_repaint(id);
        }
    }
}

impl SeatHandler for Windowing {
    fn seat_state(&mut self) -> &mut SeatState {
        &mut self.seat_state
    }

    fn new_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: protocol::wl_seat::WlSeat) {}

    fn new_capability(
        &mut self,
        _conn: &Connection,
        qh: &QueueHandle<Self>,
        seat: protocol::wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_none() {
            let keyboard = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("Failed to create keyboard");
            log::trace!("Keyboard capability: {:?}", keyboard);
            self.keyboard = Some(keyboard);
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            let pointer = self
                .seat_state
                .get_pointer(qh, &seat)
                .expect("Failed to create pointer");
            log::trace!("Pointer capability: {:?}", pointer);
            self.pointer = Some(pointer);
        }
    }

    fn remove_capability(
        &mut self,
        _conn: &Connection,
        _: &QueueHandle<Self>,
        _: protocol::wl_seat::WlSeat,
        capability: Capability,
    ) {
        if capability == Capability::Keyboard && self.keyboard.is_some() {
            log::trace!("Unset keyboard capability");
            self.keyboard.take().unwrap().release();
        }

        if capability == Capability::Pointer && self.pointer.is_some() {
            log::trace!("Unset pointer capability");
            self.pointer.take().unwrap().release();
        }
    }

    fn remove_seat(&mut self, _: &Connection, _: &QueueHandle<Self>, _: protocol::wl_seat::WlSeat) {
    }
}

impl KeyboardHandler for Windowing {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        wl_surface: &protocol::wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _keysyms: &[Keysym],
    ) {
        self.keyboard_entered_surface = self
            .with_surface_mut(wl_surface.into(), |surface| {
                log::trace!("keyboard enter");

                surface.on_focus(true);

                // set this surface as currently entered
                Some(wl_surface.clone())
            })
            .unwrap_or_else(|| {
                log::error!("enter event for unknown surface");

                // unknown surface: unset the currently entered surface
                None
            });
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        wl_surface: &protocol::wl_surface::WlSurface,
        _: u32,
    ) {
        log::trace!("keyboard leave");

        if let Some(previous_focused) = self.keyboard_entered_surface.take() {
            if previous_focused != *wl_surface {
                log::warn!("previous focused surface did not match up with the one we just left");
            }
        }

        self.with_surface_mut(wl_surface.into(), |surface| surface.on_focus(false))
            .unwrap_or_else(|| {
                log::error!("leave event for unknown surface");
            });
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let Some(wl_surface) = &self.keyboard_entered_surface else {
            log::warn!("key press without a focused surface");
            return;
        };

        self.with_surface_mut(wl_surface.into(), |surface| {
            log::trace!("key press {:?}", event);

            let key = convert::keysym_to_key(event.keysym);
            if let Some(t) = event.utf8 {
                if !(t.is_empty() || t.chars().all(|c| c.is_ascii_control())) {
                    surface.push_event(egui::Event::Text(t));
                }
            }

            if let Some(key) = key {
                surface.on_key(key, true);
            }
        })
        .unwrap_or_else(|| {
            log::error!("key press event for unknown surface");
        });
    }

    fn release_key(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        let Some(wl_surface) = &self.keyboard_entered_surface else {
            log::warn!("key release without a focused surface");
            return;
        };

        self.with_surface_mut(wl_surface.into(), |surface| {
            let key = convert::keysym_to_key(event.keysym);
            if let Some(key) = key {
                surface.on_key(key, false);
            }
        })
        .unwrap_or_else(|| {
            log::error!("key release event for unknown surface");
        });
    }

    fn update_modifiers(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        _serial: u32,
        modifiers: Modifiers,
        _layout: u32,
    ) {
        let Some(wl_surface) = &self.keyboard_entered_surface else {
            return;
        };

        self.with_surface_mut(wl_surface.into(), |surface| {
            log::trace!("keyboard modifiers {:?}", modifiers);
            surface.set_modifiers(egui::Modifiers {
                alt: modifiers.alt,
                ctrl: modifiers.ctrl,
                shift: modifiers.shift,
                mac_cmd: false,
                command: false,
            });
        })
        .unwrap_or_else(|| {
            log::warn!("modifiers without a focused surface");
        });
    }
}

impl PointerHandler for Windowing {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let wl_surface = &event.surface;

            self.with_surface_mut(wl_surface.into(), |surface| {
                surface.handle_pointer_event(event);
            })
            .unwrap_or_else(|| {
                log::error!("pointer event for unknown surface");
            });
        }
    }
}

impl ProvidesRegistryState for Windowing {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

#[derive(Debug, Clone)]
pub enum DispatcherRequest {
    RepaintSurface(SurfaceId),
    RepaintViewport(ViewportId, u64),
}

delegate_compositor!(Windowing);
delegate_output!(Windowing);

delegate_seat!(Windowing);
delegate_keyboard!(Windowing);
delegate_pointer!(Windowing);

delegate_layer!(Windowing);

delegate_registry!(Windowing);
