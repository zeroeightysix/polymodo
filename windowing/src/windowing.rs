use crate::surface::{LayerShellOptions, Surface};
use crate::{app, convert, WindowingError};
use egui::ahash::HashMap;
use egui::{Color32, Context};
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
use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};

pub struct Windowing<A> {
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

    surfaces: HashMap<protocol::wl_surface::WlSurface, Surface>,

    pub app: A,
}

impl<A: app::App + 'static> Windowing<A> {
    pub async fn create(
        wgpu_setup: WgpuSetup,
        app: A,
    ) -> Result<(EventQueue<Self>, Self), WindowingError> {
        let connection = Connection::connect_to_env().map_err(|_| WindowingError::NotWayland)?;
        let (globals, event_queue) = globals::registry_queue_init(&connection)?;
        let qh: QueueHandle<Windowing<A>> = event_queue.handle();

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
            app,
        };

        Ok((event_queue, state))
    }

    pub async fn create_surface(
        &mut self,
        LayerShellOptions {
            wgpu_options,
            layer,
            namespace,
            anchor,
            width,
            height,
        }: LayerShellOptions<'_>,
    ) -> Result<protocol::wl_surface::WlSurface, WindowingError> {
        let Self { qh, instance, .. } = &self;

        // create a new wayland surface and assign the layer_shell role
        let wl_surface = self.compositor.create_surface(qh);
        let wl_surf_id = wl_surface.id();
        let layer = self
            .layer_shell
            .create_layer_surface(qh, wl_surface, layer, namespace, None);

        // set up layer_shell options as provided
        layer.set_anchor(anchor);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.set_size(width, height);
        layer.commit();

        // create the wgpu surface (handle to all graphics related stuff on this wayland surface)
        // SAFETY: the raw window handles constructed are always created by us, and we know that
        // they're pointers to the correct types
        let gpu_surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    NonNull::new(self.connection.backend().display_ptr() as *mut std::ffi::c_void)
                        .unwrap(),
                )),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    NonNull::new(wl_surf_id.as_ptr() as *mut std::ffi::c_void).unwrap(),
                )),
            })?
        };
        // set up the egui render state
        let render_state =
            RenderState::create(&wgpu_options, instance, Some(&gpu_surface), None, 1, true).await?;

        // set up the egui context
        let ctx: Context = Default::default();
        ctx.set_theme(egui::Theme::Light);
        ctx.all_styles_mut(|sty| {
            sty.visuals.panel_fill = Color32::TRANSPARENT;
        });

        let new_surface = Surface {
            exit: false,
            first_configure: true,
            default_size: Some((width, height)),
            size: (width, height),
            layer,
            focused: false,
            events: vec![],
            ctx,
            modifiers: Default::default(),
            start_time: self.start_time,
            wpgu_surface: gpu_surface,
            render_state,
        };

        // set up the surface for rendering given the default size
        // new_surface.configure_surface();

        // finally, insert it into our internal store of surfaces
        let wl_surface = new_surface.layer.wl_surface().clone();
        self.surfaces.insert(wl_surface.clone(), new_surface);

        Ok(wl_surface)
    }

    // TODO: not very pretty
    pub fn render(
        &mut self,
        surface: &protocol::wl_surface::WlSurface,
        repaint: bool,
    ) -> Result<(), WindowingError> {
        let Some(surface) = self.surfaces.get_mut(surface) else {
            return Err(WindowingError::NoSuchSurface);
        };

        if repaint || !surface.events.is_empty() || surface.ctx.has_requested_repaint() {
            surface.render(&mut self.app)?;
        }
        Ok(())
    }
}

impl<A: app::App> CompositorHandler for Windowing<A> {
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

        let Some(surface) = self.surfaces.get_mut(surface) else {
            log::error!("frame event for unknown surface");
            return;
        };

        let render_result = surface.render(&mut self.app);
        log::trace!("render result {:?}", render_result);
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

impl<A> OutputHandler for Windowing<A> {
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

impl<A: app::App> LayerShellHandler for Windowing<A> {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, layer: &LayerSurface) {
        let Some(surface) = self.surfaces.get_mut(layer.wl_surface()) else {
            log::error!("closed event for unknown surface");
            return;
        };

        surface.set_exit();
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let Some(surface) = self.surfaces.get_mut(layer.wl_surface()) else {
            log::error!("configure event for unknown surface");

            return;
        };

        log::trace!(
            "configure {:?} (first: {})",
            configure,
            surface.first_configure
        );

        let (width, height) = configure.new_size;
        surface.update_size(width, height);

        // Initiate the first draw, if applicable.
        surface.first_draw(&mut self.app);
    }
}

impl<A: 'static> SeatHandler for Windowing<A> {
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

impl<A> KeyboardHandler for Windowing<A> {
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
        let Some(surface) = self.surfaces.get_mut(wl_surface) else {
            log::error!("enter event for unknown surface");

            return;
        };
        self.keyboard_entered_surface = Some(wl_surface.clone());

        log::trace!("keyboard enter");
        surface.focused = true;
        surface.events.push(egui::Event::WindowFocused(true));
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

        let Some(surface) = self.surfaces.get_mut(wl_surface) else {
            log::error!("leave event for unknown surface");

            return;
        };
        if let Some(previous_focused) = self.keyboard_entered_surface.take() {
            if previous_focused != *wl_surface {
                log::warn!("previous focused surface did not match up with the one we just left");
            }
        }

        surface.focused = false;
        surface.events.push(egui::Event::WindowFocused(false));
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
        let Some(surface) = self.surfaces.get_mut(wl_surface) else {
            log::error!("key press event for unknown surface");

            return;
        };

        log::trace!("key press {:?}", event);

        let key = convert::keysym_to_key(event.keysym);
        if let Some(t) = event.utf8 {
            if !(t.is_empty() || t.chars().all(|c| c.is_ascii_control())) {
                surface.events.push(egui::Event::Text(t));
            }
        }
        if let Some(key) = key {
            surface.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                modifiers: surface.modifiers,
                repeat: false,
            })
        }
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
        let Some(surface) = self.surfaces.get_mut(wl_surface) else {
            log::error!("key release event for unknown surface");

            return;
        };

        let key = convert::keysym_to_key(event.keysym);
        if let Some(key) = key {
            surface.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                modifiers: surface.modifiers,
                repeat: false,
            })
        }
        // println!("{} {key:?}", event.raw_code);
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
            log::warn!("modifiers without a focused surface");
            return;
        };
        let Some(surface) = self.surfaces.get_mut(wl_surface) else {
            log::error!("modifiers event for unknown surface");

            return;
        };

        log::trace!("keyboard modifiers {:?}", modifiers);
        surface.set_modifiers(egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: false,
        });
    }
}

impl<A> PointerHandler for Windowing<A> {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        for event in events {
            let wl_surface = &event.surface;
            let Some(surface) = self.surfaces.get_mut(wl_surface) else {
                log::error!("pointer event for unknown surface");

                return;
            };

            surface.handle_pointer_event(event);
        }
    }
}

impl<A> Windowing<A> {
    
}

impl<A: 'static + app::App> ProvidesRegistryState for Windowing<A> {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![@<A> OutputState, SeatState];
}

delegate_compositor!(@<A: (app::App) + 'static> Windowing<A>);
delegate_output!(@<A: 'static> Windowing<A>);

delegate_seat!(@<A: (app::App) + 'static> Windowing<A>);
delegate_keyboard!(@<A: 'static> Windowing<A>);
delegate_pointer!(@<A> Windowing<A>);

delegate_layer!(@<A: (app::App) + 'static> Windowing<A>);

delegate_registry!(@<A: (app::App) + 'static> Windowing<A>);
