use crate::windowing::surface::{LayerSurfaceOptions, Surface, SurfaceId};
use crate::windowing::client::SurfaceEvent::NeedsRepaint;
use crate::windowing::WindowingError;
use egui::ViewportId;
use egui_wgpu::{RenderState, WgpuSetup};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::GlobalList;
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
use std::ffi::c_void;
use std::ptr::NonNull;
use tokio::sync::mpsc;
use wayland_backend::client;
use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};
use crate::windowing::convert::keysym_to_key;

pub struct WaylandClient {
    connection: Connection,
    globals: GlobalList,
    event_queue: EventQueue<Dispatcher>,
    dispatcher: Dispatcher,
}

impl WaylandClient {
    pub async fn create(surf_driver_event_sender: mpsc::Sender<SurfaceEvent>) -> anyhow::Result<Self> {
        let connection = Connection::connect_to_env().map_err(|_e| WindowingError::NotWayland)?;
        let (globals, event_queue) = globals::registry_queue_init(&connection)?;
        let qh: QueueHandle<Dispatcher> = event_queue.handle();

        let dispatcher = Dispatcher::create(&globals, &qh, surf_driver_event_sender).await?;

        Ok(Self {
            connection,
            globals,
            event_queue,
            dispatcher,
        })
    }

    /// Set up the WGPU instance and create a [SurfaceSetup] used for spawning new surfaces.
    pub async fn new_surface_setup(
        &self,
        wgpu_setup: WgpuSetup,
    ) -> Result<SurfaceSetup, WindowingError> {
        let qh = self.event_queue.handle();

        let compositor_state = CompositorState::bind(&self.globals, &qh).unwrap();
        let layer_shell =
            LayerShell::bind(&self.globals, &qh).map_err(|_| WindowingError::NoLayerShell)?;

        // create the wgpu instance from provided setup config
        let instance = wgpu_setup.new_instance().await;

        Ok(SurfaceSetup {
            backend: self.connection.backend(),
            qh,
            instance,
            compositor_state,
            layer_shell,
        })
    }

    /// Dispatch wayland messages, maybe blocking if there are none to wait for messages.
    ///
    /// Returns an error if dispatch failed (either a bad message was sent, or the compositor
    /// sent an error back)
    pub fn dispatch(&mut self) -> anyhow::Result<()> {
        self.dispatcher.dispatch(&mut self.event_queue)
    }
}

/// The main wayland event handler.
pub struct Dispatcher {
    surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,

    // state for the dispatch delegates to work
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,

    keyboard: Option<protocol::wl_keyboard::WlKeyboard>,
    keyboard_entered_surface: Option<protocol::wl_surface::WlSurface>,
    pointer: Option<protocol::wl_pointer::WlPointer>,
}

impl Dispatcher {
    pub async fn create(
        globals: &GlobalList,
        qh: &QueueHandle<Dispatcher>,
        surf_driver_event_sender: mpsc::Sender<SurfaceEvent>,
    ) -> Result<Self, WindowingError> {
        let seat_state = SeatState::new(globals, qh);
        let output_state = OutputState::new(globals, qh);

        let state = Dispatcher {
            surf_driver_event_sender,
            registry_state: RegistryState::new(&globals),
            seat_state,
            output_state,
            keyboard: None,
            keyboard_entered_surface: None,
            pointer: None,
        };

        Ok(state)
    }

    /// Dispatch wayland events and repaint surfaces with pending events.
    // pub async fn dispatch(
    //     &mut self,
    //     event_queue: &mut EventQueue<Self>,
    // ) -> Result<(), WindowingError> {
    //     let dispatched = event_queue.dispatch_pending(self)?;
    //
    //     event_queue.flush()?;
    //
    //     self.push_event(SurfaceEvent::RepaintAll);
    //
    //     if dispatched > 0 {
    //         return Ok(());
    //     }
    //
    //     if let Some(events) = event_queue.prepare_read() {
    //         let fd = events.connection_fd().try_clone_to_owned()?;
    //         let async_fd = tokio::io::unix::AsyncFd::new(fd)?;
    //         let mut ready_guard = async_fd.readable().await?;
    //         match events.read() {
    //             Ok(_) => {
    //                 ready_guard.clear_ready();
    //             }
    //             Err(WaylandError::Io(e)) if e.kind() == ErrorKind::WouldBlock => {}
    //             Err(e) => Err(e)?,
    //         }
    //         drop(ready_guard);
    //     }
    //
    //     Ok(())
    // }

    pub fn dispatch(&mut self, event_queue: &mut EventQueue<Self>) -> anyhow::Result<()> {
        event_queue.blocking_dispatch(self)?;

        self.push_event(SurfaceEvent::RepaintAllWithEvents);

        Ok(())
    }

    fn push_event(&self, event: SurfaceEvent) {
        if let Err(e) = self.surf_driver_event_sender.blocking_send(event) {
            log::warn!("dispatcher: failed to push surface event ({e:?})");
        }
    }
}

pub enum SurfaceEvent {
    RepaintAllWithEvents,
    NeedsRepaint(SurfaceId),
    Closed(SurfaceId),
    Configure(SurfaceId, LayerSurfaceConfigure),
    KeyboardFocus(SurfaceId, bool),
    PressKey(SurfaceId, Option<String>, Option<egui::Key>),
    ReleaseKey(SurfaceId, Option<egui::Key>),
    UpdateModifiers(SurfaceId, egui::Modifiers),
    Pointer(SurfaceId, PointerEvent),
}

/// All you need to create a new wayland surface with a GPU rendering context attached.
pub struct SurfaceSetup {
    // all state required to create a new surface
    backend: client::Backend,
    qh: QueueHandle<Dispatcher>,
    instance: wgpu::Instance,
    compositor_state: CompositorState,
    layer_shell: LayerShell,
}

impl SurfaceSetup {
    pub async fn create_surface(
        &self,
        viewport_id: ViewportId,
        LayerSurfaceOptions {
            wgpu_options,
            layer,
            namespace,
            anchor,
            width,
            height,
        }: LayerSurfaceOptions<'_>,
    ) -> Result<Surface, WindowingError> {
        // create a new wayland surface and assign the layer_shell role
        let wl_surface = self.compositor_state.create_surface(&self.qh);
        let wl_surface_id = wl_surface.id();
        let layer_surface = self
            .layer_shell
            .create_layer_surface(&self.qh, wl_surface, layer, namespace, None);

        // set up layer_shell options as provided
        layer_surface.set_anchor(anchor);
        layer_surface.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer_surface.set_size(width, height);
        layer_surface.commit();

        // create the wgpu surface (handle to all graphics related stuff on this wayland surface)
        // SAFETY: the raw window handles constructed are always created by us, and we know that
        // they're pointers to the correct types
        let wgpu_surface = unsafe {
            self.instance
                .create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                    raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                        NonNull::new(self.backend.display_ptr() as *mut c_void).unwrap(),
                    )),
                    raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                        NonNull::new(wl_surface_id.as_ptr() as *mut c_void).unwrap(),
                    )),
                })?
        };

        // set up the egui render state
        let render_state = RenderState::create(
            &wgpu_options,
            &self.instance,
            Some(&wgpu_surface),
            None,
            1,
            true,
        )
        .await?;
        let surface = Surface::create(
            viewport_id,
            (width, height),
            layer_surface,
            wgpu_surface,
            render_state,
        );
        
        surface.configure_surface();

        Ok(surface)
    }
}

impl CompositorHandler for Dispatcher {
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
    }

    fn frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        surface: &protocol::wl_surface::WlSurface,
        _time: u32,
    ) {
        log::trace!("frame");
        self.push_event(NeedsRepaint(surface.into()));
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

impl OutputHandler for Dispatcher {
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

impl LayerShellHandler for Dispatcher {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        self.push_event(SurfaceEvent::Closed(layer.wl_surface().into()));
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        self.push_event(SurfaceEvent::Configure(
            layer.wl_surface().into(),
            configure,
        ));
    }
}

impl SeatHandler for Dispatcher {
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

impl KeyboardHandler for Dispatcher {
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
        self.push_event(SurfaceEvent::KeyboardFocus(wl_surface.into(), true));
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        wl_surface: &protocol::wl_surface::WlSurface,
        _: u32,
    ) {
        self.push_event(SurfaceEvent::KeyboardFocus(wl_surface.into(), false));

        if let Some(previous_focused) = self.keyboard_entered_surface.take() {
            if previous_focused != *wl_surface {
                log::warn!("previous focused surface did not match up with the one we just left");
            }
        }
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

        let mut text = None;
        if let Some(t) = event.utf8 {
            if !(t.is_empty() || t.chars().all(|c| c.is_ascii_control())) {
                text = Some(t);
            }
        }

        self.push_event(SurfaceEvent::PressKey(wl_surface.into(), text, keysym_to_key(event.keysym)));
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

        self.push_event(SurfaceEvent::ReleaseKey(
            wl_surface.into(),
            keysym_to_key(event.keysym),
        ));
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

        self.push_event(SurfaceEvent::UpdateModifiers(
            wl_surface.into(),
            egui::Modifiers {
                alt: modifiers.alt,
                ctrl: modifiers.ctrl,
                shift: modifiers.shift,
                mac_cmd: false,
                command: false,
            }
        ));
    }
}

impl PointerHandler for Dispatcher {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let wl_surface = &event.surface;

            self.push_event(SurfaceEvent::Pointer(wl_surface.into(), event.clone()));
        }
    }
}

impl ProvidesRegistryState for Dispatcher {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(Dispatcher);
delegate_output!(Dispatcher);

delegate_seat!(Dispatcher);
delegate_keyboard!(Dispatcher);
delegate_pointer!(Dispatcher);

delegate_layer!(Dispatcher);

delegate_registry!(Dispatcher);
