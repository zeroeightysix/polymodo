pub mod app;
pub mod client;
mod convert;

use derive_more::with_trait::From;
use derive_more::{Display, Error};
use egui::{Color32, Context, Key, MouseWheelUnit, PointerButton, Rect};
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
pub use smithay_client_toolkit as sctk;

#[derive(Debug, Clone)]
pub struct LayerShellOptions<'a> {
    pub wgpu_setup: WgpuSetup,
    pub wgpu_options: WgpuConfiguration,
    pub layer: Layer,
    pub namespace: Option<&'a str>,
    pub anchor: Anchor,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Display, Error, From)]
pub enum LayerWindowingError {
    NotWayland,
    GlobalError(GlobalError),
    NoLayerShell,
    NoAdapter,
    RequestDeviceError(wgpu::RequestDeviceError),
    SurfaceError(wgpu::SurfaceError),
    CreateSurfaceError(wgpu::CreateSurfaceError),
    WgpuError(WgpuError),
    WaylandError(wayland_backend::client::WaylandError),
    DispatchError(sctk::reexports::client::DispatchError),
    IoError(std::io::Error),
}

pub struct LayerWindowing<A> {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,

    exit: bool,
    first_configure: bool,
    default_size: Option<(u32, u32)>,
    size: (u32, u32),
    layer: LayerSurface,
    focused: bool,
    keyboard: Option<protocol::wl_keyboard::WlKeyboard>,
    pointer: Option<protocol::wl_pointer::WlPointer>,
    pub events: Vec<egui::Event>,
    start_time: std::time::Instant,

    modifiers: egui::Modifiers,
    render_state: RenderState,
    surface: wgpu::Surface<'static>,
    pub ctx: Context,
    pub app: A,
}

impl<A> LayerWindowing<A> {
    pub async fn create(
        LayerShellOptions {
            wgpu_setup,
            wgpu_options,
            layer,
            namespace,
            anchor,
            width,
            height,
        }: LayerShellOptions<'_>,
        app: A,
    ) -> Result<(EventQueue<Self>, Self), LayerWindowingError>
    where
        A: 'static + app::App,
    {
        let connection =
            Connection::connect_to_env().map_err(|_| LayerWindowingError::NotWayland)?;
        let (globals, event_queue) = globals::registry_queue_init(&connection)?;
        let qh: QueueHandle<LayerWindowing<A>> = event_queue.handle();

        let compositor = CompositorState::bind(&globals, &qh).unwrap();
        let layer_shell =
            LayerShell::bind(&globals, &qh).map_err(|_| LayerWindowingError::NoLayerShell)?;

        let surface = compositor.create_surface(&qh);
        let surf_id = surface.id();
        let layer = layer_shell.create_layer_surface(&qh, surface, layer, namespace, None);

        layer.set_anchor(anchor);
        layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
        layer.set_size(width, height);
        layer.commit();

        let instance = wgpu_setup.new_instance().await;

        let surface = unsafe {
            instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
                raw_display_handle: RawDisplayHandle::Wayland(WaylandDisplayHandle::new(
                    NonNull::new(connection.backend().display_ptr() as *mut std::ffi::c_void)
                        .unwrap(),
                )),
                raw_window_handle: RawWindowHandle::Wayland(WaylandWindowHandle::new(
                    NonNull::new(surf_id.as_ptr() as *mut std::ffi::c_void).unwrap(),
                )),
            })?
        };
        let render_state =
            RenderState::create(&wgpu_options, &instance, Some(&surface), None, 1, true).await?;

        let ctx: Context = Default::default();
        ctx.set_theme(egui::Theme::Light);
        ctx.all_styles_mut(|sty| {
            sty.visuals.panel_fill = Color32::TRANSPARENT;
        });

        let state = LayerWindowing {
            registry_state: RegistryState::new(&globals),
            seat_state: SeatState::new(&globals, &qh),
            output_state: OutputState::new(&globals, &qh),
            exit: false,
            first_configure: true,
            default_size: Some((width, height)),
            size: (width, height),
            layer,
            focused: false,
            keyboard: None,
            pointer: None,

            events: vec![],
            start_time: std::time::Instant::now(),
            modifiers: Default::default(),
            ctx,
            render_state,
            surface,
            app,
        };

        state.configure_surface();

        Ok((event_queue, state))
    }

    fn configure_surface(&self) {
        let format = self.render_state.target_format;
        let (width, height) = self.size;
        self.surface.configure(
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
}

impl<A: app::App> LayerWindowing<A> {
    pub fn render(&mut self) -> Result<(), LayerWindowingError> {
        let output_frame = self.surface.get_current_texture()?;
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
                        load: wgpu::LoadOp::Clear(wgpu::Color {
                            r: 0.0,
                            g: 1.0,
                            b: 0.0,
                            a: 0.1,
                        }),
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
            self.app.update(ctx);
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
            wgpu_setup: Default::default(),
            wgpu_options: Default::default(),
            layer: Layer::Top,
            namespace: None,
            anchor: Anchor::all(),
            width: 1024,
            height: 1024,
        }
    }
}

impl<A: app::App> CompositorHandler for LayerWindowing<A> {
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
        _surface: &protocol::wl_surface::WlSurface,
        _time: u32,
    ) {
        let _ = self.render();
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

impl<A> OutputHandler for LayerWindowing<A> {
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

impl<A: app::App> LayerShellHandler for LayerWindowing<A> {
    fn closed(&mut self, _conn: &Connection, _qh: &QueueHandle<Self>, _layer: &LayerSurface) {
        self.exit = true;
    }

    fn configure(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _layer: &LayerSurface,
        configure: LayerSurfaceConfigure,
        _serial: u32,
    ) {
        let width = if configure.new_size.0 == 0 {
            self.default_size.map(|(w, _)| w).unwrap_or(256)
        } else {
            configure.new_size.0
        };
        let height = if configure.new_size.1 == 0 {
            self.default_size.map(|(_, h)| h).unwrap_or(256)
        } else {
            configure.new_size.1
        };

        self.update_size(width, height);

        // Initiate the first draw.
        if self.first_configure {
            self.first_configure = false;
            let _ = self.render();
        }
    }
}

impl<A: 'static> SeatHandler for LayerWindowing<A> {
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

impl<A> KeyboardHandler for LayerWindowing<A> {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        _surface: &protocol::wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        _keysyms: &[Keysym],
    ) {
        log::trace!("keyboard enter");
        self.focused = true;
        self.events.push(egui::Event::WindowFocused(true));
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        _surface: &protocol::wl_surface::WlSurface,
        _: u32,
    ) {
        log::trace!("keyboard leave");

        self.focused = false;
        self.events.push(egui::Event::WindowFocused(false));
    }

    fn press_key(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        _: u32,
        event: KeyEvent,
    ) {
        log::trace!("key press {:?}", event);

        let key = convert::keysym_to_key(event.keysym);
        if let Some(t) = event.utf8 {
            if !(t.is_empty() || t.chars().all(|c| c.is_ascii_control())) {
                self.events.push(egui::Event::Text(t));
            }
        }
        if let Some(key) = key {
            self.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: true,
                modifiers: self.modifiers,
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
        let key = convert::keysym_to_key(event.keysym);
        if let Some(key) = key {
            self.events.push(egui::Event::Key {
                key,
                physical_key: None,
                pressed: false,
                modifiers: self.modifiers,
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
        log::trace!("keyboard modifiers {:?}", modifiers);
        self.modifiers = egui::Modifiers {
            alt: modifiers.alt,
            ctrl: modifiers.ctrl,
            shift: modifiers.shift,
            mac_cmd: false,
            command: false,
        }
    }
}

impl<A> PointerHandler for LayerWindowing<A> {
    fn pointer_frame(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _pointer: &protocol::wl_pointer::WlPointer,
        events: &[PointerEvent],
    ) {
        log::trace!("pointer_frame {events:?}");

        use PointerEventKind::*;
        for event in events {
            // Ignore events for other surfaces
            if &event.surface != self.layer.wl_surface() {
                continue;
            }
            let pos = (event.position.0 as f32, event.position.1 as f32).into();
            match event.kind {
                Enter { .. } => {
                    self.events.push(egui::Event::PointerMoved(pos));
                }
                Leave { .. } => self.events.push(egui::Event::PointerGone),
                Motion { .. } => {
                    self.events.push(egui::Event::PointerMoved(pos));
                }
                Press { button, .. } => {
                    self.events.push(egui::Event::PointerButton {
                        pos,
                        button: convert::pointer_button_to_egui(button),
                        pressed: true,
                        modifiers: self.modifiers,
                    });
                }
                Release { button, .. } => {
                    self.events.push(egui::Event::PointerButton {
                        pos,
                        button: convert::pointer_button_to_egui(button),
                        pressed: false,
                        modifiers: self.modifiers,
                    });
                }
                Axis {
                    horizontal,
                    vertical,
                    ..
                } => self.events.push(egui::Event::MouseWheel {
                    unit: MouseWheelUnit::Point,
                    delta: (horizontal.absolute as f32, vertical.absolute as f32).into(),
                    modifiers: self.modifiers,
                }),
            }
        }
    }
}

impl<A: 'static + app::App> ProvidesRegistryState for LayerWindowing<A> {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![@<A> OutputState, SeatState];
}

delegate_compositor!(@<A: (app::App) + 'static> LayerWindowing<A>);
delegate_output!(@<A: 'static> LayerWindowing<A>);

delegate_seat!(@<A: (app::App) + 'static> LayerWindowing<A>);
delegate_keyboard!(@<A: 'static> LayerWindowing<A>);
delegate_pointer!(@<A> LayerWindowing<A>);

delegate_layer!(@<A: (app::App) + 'static> LayerWindowing<A>);

delegate_registry!(@<A: (app::App) + 'static> LayerWindowing<A>);
