pub mod app;

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
}

pub struct LayerWindowing<A> {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,

    exit: bool,
    first_configure: bool,
    width: u32,
    height: u32,
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
    fn configure_surface(&self) {
        let format = self.render_state.target_format;
        self.surface.configure(
            &self.render_state.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format,
                view_formats: vec![format.add_srgb_suffix()],
                alpha_mode: wgpu::CompositeAlphaMode::PreMultiplied,
                width: self.width,
                height: self.height,
                desired_maximum_frame_latency: 2,
                present_mode: wgpu::PresentMode::Mailbox,
            },
        );
    }

    fn update_size(&mut self, width: u32, height: u32) {
        self.width = width;
        self.height = height;
        self.configure_surface();
    }

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
    ) -> Result<(EventQueue<LayerWindowing<A>>, LayerWindowing<A>), LayerWindowingError>
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
            width,
            height,
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

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
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
                (self.width as f32, self.height as f32).into(),
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
                size_in_pixels: [self.width, self.height],
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
        if configure.new_size.0 == 0 || configure.new_size.1 == 0 {
            self.width = 256;
            self.height = 256;
        } else {
            self.width = configure.new_size.0;
            self.height = configure.new_size.1;
        }

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
            self.keyboard = Some(keyboard);
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            let pointer = self
                .seat_state
                .get_pointer(qh, &seat)
                .expect("Failed to create pointer");
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
            println!("Unset keyboard capability");
            self.keyboard.take().unwrap().release();
        }

        if capability == Capability::Pointer && self.pointer.is_some() {
            println!("Unset pointer capability");
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
        keysyms: &[Keysym], // TODO
    ) {
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
        let key = keysym_to_key(event.keysym);
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
        let key = keysym_to_key(event.keysym);
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
                        button: pointer_button_to_egui(button),
                        pressed: true,
                        modifiers: self.modifiers,
                    });
                }
                Release { button, .. } => {
                    self.events.push(egui::Event::PointerButton {
                        pos,
                        button: pointer_button_to_egui(button),
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

fn keysym_to_key(sym: Keysym) -> Option<Key> {
    match sym {
        Keysym::Down => Some(Key::ArrowDown),
        Keysym::Left => Some(Key::ArrowLeft),
        Keysym::Right => Some(Key::ArrowRight),
        Keysym::Up => Some(Key::ArrowUp),
        Keysym::Escape => Some(Key::Escape),
        Keysym::Tab => Some(Key::Tab),
        Keysym::BackSpace => Some(Key::Backspace),
        Keysym::Return => Some(Key::Enter),
        Keysym::space => Some(Key::Space),
        Keysym::Insert => Some(Key::Insert),
        Keysym::Delete => Some(Key::Delete),
        Keysym::Home => Some(Key::Home),
        Keysym::End => Some(Key::End),
        Keysym::Prior => Some(Key::PageUp),
        Keysym::Next => Some(Key::PageDown),
        // Keysym::Copy => Key::Copy,
        // Keysym::Cut => Key::Cut,
        // Keysym::Paste => Key::Paste,
        Keysym::colon => Some(Key::Colon),
        Keysym::comma => Some(Key::Comma),
        Keysym::slash => Some(Key::Slash),
        Keysym::bar => Some(Key::Pipe),
        Keysym::question => Some(Key::Questionmark),
        Keysym::exclam => Some(Key::Exclamationmark),
        Keysym::bracketleft => Some(Key::OpenBracket),
        Keysym::bracketright => Some(Key::CloseBracket),
        Keysym::braceleft => Some(Key::OpenCurlyBracket),
        Keysym::braceright => Some(Key::CloseCurlyBracket),
        Keysym::grave => Some(Key::Backtick),
        Keysym::period => Some(Key::Period),
        Keysym::plus => Some(Key::Plus),
        Keysym::equal => Some(Key::Equals),
        Keysym::semicolon => Some(Key::Semicolon),
        Keysym::apostrophe => Some(Key::Quote),
        // Keysym::Num0 => Key::Num0, TODO
        // Keysym::Num1 => Key::Num1,
        // Keysym::Num2 => Key::Num2,
        // Keysym::Num3 => Key::Num3,
        // Keysym::Num4 => Key::Num4,
        // Keysym::Num5 => Key::Num5,
        // Keysym::Num6 => Key::Num6,
        // Keysym::Num7 => Key::Num7,
        // Keysym::Num8 => Key::Num8,
        // Keysym::Num9 => Key::Num9,
        Keysym::a | Keysym::A => Some(Key::A),
        Keysym::b | Keysym::B => Some(Key::B),
        Keysym::c | Keysym::C => Some(Key::C),
        Keysym::d | Keysym::D => Some(Key::D),
        Keysym::e | Keysym::E => Some(Key::E),
        Keysym::f | Keysym::F => Some(Key::F),
        Keysym::g | Keysym::G => Some(Key::G),
        Keysym::h | Keysym::H => Some(Key::H),
        Keysym::i | Keysym::I => Some(Key::I),
        Keysym::j | Keysym::J => Some(Key::J),
        Keysym::k | Keysym::K => Some(Key::K),
        Keysym::l | Keysym::L => Some(Key::L),
        Keysym::m | Keysym::M => Some(Key::M),
        Keysym::n | Keysym::N => Some(Key::N),
        Keysym::o | Keysym::O => Some(Key::O),
        Keysym::p | Keysym::P => Some(Key::P),
        Keysym::q | Keysym::Q => Some(Key::Q),
        Keysym::r | Keysym::R => Some(Key::R),
        Keysym::s | Keysym::S => Some(Key::S),
        Keysym::t | Keysym::T => Some(Key::T),
        Keysym::u | Keysym::U => Some(Key::U),
        Keysym::v | Keysym::V => Some(Key::V),
        Keysym::w | Keysym::W => Some(Key::W),
        Keysym::x | Keysym::X => Some(Key::X),
        Keysym::y | Keysym::Y => Some(Key::Y),
        Keysym::z | Keysym::Z => Some(Key::Z),
        Keysym::F1 => Some(Key::F1),
        Keysym::F2 => Some(Key::F2),
        Keysym::F3 => Some(Key::F3),
        Keysym::F4 => Some(Key::F4),
        Keysym::F5 => Some(Key::F5),
        Keysym::F6 => Some(Key::F6),
        Keysym::F7 => Some(Key::F7),
        Keysym::F8 => Some(Key::F8),
        Keysym::F9 => Some(Key::F9),
        Keysym::F10 => Some(Key::F10),
        Keysym::F11 => Some(Key::F11),
        Keysym::F12 => Some(Key::F12),
        Keysym::F13 => Some(Key::F13),
        Keysym::F14 => Some(Key::F14),
        Keysym::F15 => Some(Key::F15),
        Keysym::F16 => Some(Key::F16),
        Keysym::F17 => Some(Key::F17),
        Keysym::F18 => Some(Key::F18),
        Keysym::F19 => Some(Key::F19),
        Keysym::F20 => Some(Key::F20),
        Keysym::F21 => Some(Key::F21),
        Keysym::F22 => Some(Key::F22),
        Keysym::F23 => Some(Key::F23),
        Keysym::F24 => Some(Key::F24),
        Keysym::F25 => Some(Key::F25),
        Keysym::F26 => Some(Key::F26),
        Keysym::F27 => Some(Key::F27),
        Keysym::F28 => Some(Key::F28),
        Keysym::F29 => Some(Key::F29),
        Keysym::F30 => Some(Key::F30),
        Keysym::F31 => Some(Key::F31),
        Keysym::F32 => Some(Key::F32),
        Keysym::F33 => Some(Key::F33),
        Keysym::F34 => Some(Key::F34),
        Keysym::F35 => Some(Key::F35),
        Keysym::Shift_L
        | Keysym::Shift_R
        | Keysym::Control_L
        | Keysym::Control_R
        | Keysym::Super_L
        | Keysym::Super_R => None,
        _ => {
            eprintln!("dont get it: {sym:?}; {:?}", sym.name());
            None
        }
    }
}

fn pointer_button_to_egui(button: u32) -> PointerButton {
    match button {
        sctk::seat::pointer::BTN_LEFT => PointerButton::Primary,
        sctk::seat::pointer::BTN_RIGHT => PointerButton::Secondary,
        sctk::seat::pointer::BTN_MIDDLE => PointerButton::Middle,
        sctk::seat::pointer::BTN_BACK => PointerButton::Extra1,
        sctk::seat::pointer::BTN_FORWARD => PointerButton::Extra2,
        _ => PointerButton::Primary,
    }
}
