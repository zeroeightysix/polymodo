pub mod app;
pub mod client;
mod convert;

use derive_more::with_trait::From;
use derive_more::{Display, Error};
use egui::{Color32, Context, MouseWheelUnit, Rect};
use egui_wgpu::{RenderState, ScreenDescriptor, WgpuConfiguration, WgpuError, WgpuSetup};
use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
use smithay_client_toolkit::reexports::client::globals::GlobalError;
use smithay_client_toolkit::reexports::client::{globals, QueueHandle};
use smithay_client_toolkit::reexports::client::{protocol, Proxy};
use smithay_client_toolkit::reexports::client::{Connection, EventQueue};
use smithay_client_toolkit::registry::{ProvidesRegistryState, RegistryState};
use smithay_client_toolkit::seat::keyboard::{KeyEvent, KeyboardHandler, Keysym, Modifiers};
use smithay_client_toolkit::seat::pointer::{PointerEvent, PointerEventKind, PointerHandler};
use smithay_client_toolkit::seat::{Capability, SeatHandler, SeatState};
use smithay_client_toolkit::shell::wlr_layer::{
    Anchor, KeyboardInteractivity, Layer, LayerShell, LayerShellHandler, LayerSurface,
    LayerSurfaceConfigure,
};
use smithay_client_toolkit::shell::WaylandSurface;
use smithay_client_toolkit::{
    delegate_compositor, delegate_keyboard, delegate_layer, delegate_output, delegate_pointer,
    delegate_registry, delegate_seat, registry_handlers,
};
use std::fmt::Debug;
use std::ptr::NonNull;
use wgpu::rwh::{RawDisplayHandle, RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle};

pub use egui;
use egui::ahash::HashMap;
pub use smithay_client_toolkit as sctk;

#[derive(Debug, Clone)]
pub struct LayerShellOptions<'a> {
    pub wgpu_options: WgpuConfiguration,
    pub layer: Layer,
    pub namespace: Option<&'a str>,
    pub anchor: Anchor,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Display, Error, From)]
pub enum WindowingError {
    NotWayland,
    GlobalError(GlobalError),
    NoLayerShell,
    NoAdapter,
    RequestDeviceError(wgpu::RequestDeviceError),
    SurfaceError(wgpu::SurfaceError),
    CreateSurfaceError(wgpu::CreateSurfaceError),
    WgpuError(WgpuError),
    #[allow(unused_qualifications)]
    WaylandError(wayland_backend::client::WaylandError),
    DispatchError(sctk::reexports::client::DispatchError),
    IoError(std::io::Error),
    NoSuchSurface,
}

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

pub struct Surface {
    exit: bool,
    first_configure: bool,
    default_size: Option<(u32, u32)>,
    size: (u32, u32),
    layer: LayerSurface,
    focused: bool,

    pub events: Vec<egui::Event>,
    pub ctx: Context,
    modifiers: egui::Modifiers,
    start_time: std::time::Instant,

    wpgu_surface: wgpu::Surface<'static>,
    render_state: RenderState,
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

impl Surface {
    fn configure_surface(&self) {
        let format = self.render_state.target_format;
        let (width, height) = self.size;
        log::trace!("configure wgpu surface");

        self.wpgu_surface.configure(
            &self.render_state.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                view_formats: vec![format.add_srgb_suffix()],
                alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
                width,
                height,
                desired_maximum_frame_latency: 2,
                present_mode: wgpu::PresentMode::Mailbox,
            },
        );
    }

    fn update_size(&mut self, width: u32, height: u32) {
        self.size = (width, height);
        self.configure_surface();
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub fn render<A>(&mut self, app: &mut A) -> Result<(), WindowingError>
    where
        A: app::App,
    {
        let output_frame = self.wpgu_surface.get_current_texture()?;
        let output_view = output_frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self
            .render_state
            .device
            .create_command_encoder(&Default::default());
        let mut pass = encoder
            .begin_render_pass(&wgpu::RenderPassDescriptor {
                label: None,
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &output_view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::TRANSPARENT),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            })
            .forget_lifetime();

        let events = std::mem::take(&mut self.events);

        let raw_input = egui::RawInput {
            screen_rect: Some(Rect::from_min_size(
                Default::default(),
                (self.size.0 as f32, self.size.1 as f32).into(),
            )),
            modifiers: self.modifiers,
            focused: self.focused,
            time: Some((std::time::Instant::now() - self.start_time).as_secs_f64()),
            events,
            ..Default::default()
        };
        let output = self.ctx.run(raw_input, |ctx| {
            app.update(ctx);
        });
        // TODO: output.platform_output
        let prims = self.ctx.tessellate(output.shapes, output.pixels_per_point);
        {
            let mut renderer = self.render_state.renderer.write();
            let descriptor = ScreenDescriptor {
                size_in_pixels: self.size.into(),
                pixels_per_point: output.pixels_per_point,
            };
            for (id, delta) in output.textures_delta.set {
                renderer.update_texture(
                    &self.render_state.device,
                    &self.render_state.queue,
                    id,
                    &delta,
                );
            }
            renderer.update_buffers(
                &self.render_state.device,
                &self.render_state.queue,
                &mut encoder,
                &prims,
                &descriptor,
            );
            renderer.render(&mut pass, &prims, &descriptor);
        }
        drop(pass);

        self.render_state
            .queue
            .submit(std::iter::once(encoder.finish()));

        {
            let mut renderer = self.render_state.renderer.write();
            for id in &output.textures_delta.free {
                renderer.free_texture(id);
            }
        }

        output_frame.present();

        Ok(())
    }
}

impl Default for LayerShellOptions<'_> {
    fn default() -> Self {
        Self {
            wgpu_options: Default::default(),
            layer: Layer::Top,
            namespace: None,
            anchor: Anchor::all(),
            width: 1024,
            height: 1024,
        }
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

        surface.exit = true;
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

        let width = if configure.new_size.0 == 0 {
            surface.default_size.map(|(w, _)| w).unwrap_or(256)
        } else {
            configure.new_size.0
        };
        let height = if configure.new_size.1 == 0 {
            surface.default_size.map(|(_, h)| h).unwrap_or(256)
        } else {
            configure.new_size.1
        };

        surface.update_size(width, height);

        // Initiate the first draw.
        if surface.first_configure {
            surface.first_configure = false;
            let render_result = surface.render(&mut self.app);
            log::trace!("(first configure) render result {:?}", render_result);
        }
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
        surface.modifiers = egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: false,
        }
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
        use PointerEventKind::*;
        for event in events {
            let wl_surface = &event.surface;
            let Some(surface) = self.surfaces.get_mut(wl_surface) else {
                log::error!("pointer event for unknown surface");

                return;
            };

            let pos = (event.position.0 as f32, event.position.1 as f32).into();
            match event.kind {
                Enter { .. } => {
                    surface.events.push(egui::Event::PointerMoved(pos));
                }
                Leave { .. } => surface.events.push(egui::Event::PointerGone),
                Motion { .. } => {
                    surface.events.push(egui::Event::PointerMoved(pos));
                }
                Press { button, .. } => {
                    surface.events.push(egui::Event::PointerButton {
                        pos,
                        button: convert::pointer_button_to_egui(button),
                        pressed: true,
                        modifiers: surface.modifiers,
                    });
                }
                Release { button, .. } => {
                    surface.events.push(egui::Event::PointerButton {
                        pos,
                        button: convert::pointer_button_to_egui(button),
                        pressed: false,
                        modifiers: surface.modifiers,
                    });
                }
                Axis {
                    horizontal,
                    vertical,
                    ..
                } => surface.events.push(egui::Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: (horizontal.absolute as f32, -vertical.absolute as f32).into(),
                    modifiers: surface.modifiers,
                }),
            }
        }
    }
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
