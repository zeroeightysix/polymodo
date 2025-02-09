use smithay_client_toolkit::compositor::{CompositorHandler, CompositorState};
use smithay_client_toolkit::output::{OutputHandler, OutputState};
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
use wgpu::rwh::{
    DisplayHandle, HandleError, HasDisplayHandle, HasWindowHandle, RawDisplayHandle,
    RawWindowHandle, WaylandDisplayHandle, WaylandWindowHandle, WindowHandle,
};

#[derive(Debug, Copy, Clone)]
pub struct LayerShellOptions<'a> {
    layer: Layer,
    namespace: Option<&'a str>,
    anchor: Anchor,
    width: u32,
    height: u32,
}

impl Default for LayerShellOptions<'_> {
    fn default() -> Self {
        Self {
            layer: Layer::Top,
            namespace: None,
            anchor: Anchor::all(),
            width: 256,
            height: 256,
        }
    }
}

pub struct LayerShellState {
    registry_state: RegistryState,
    seat_state: SeatState,
    output_state: OutputState,

    exit: bool,
    first_configure: bool,
    width: u32,
    height: u32,
    layer: LayerSurface,
    keyboard: Option<protocol::wl_keyboard::WlKeyboard>,
    keyboard_focus: bool,
    pointer: Option<protocol::wl_pointer::WlPointer>,

    device: wgpu::Device,
    queue: wgpu::Queue,
    surface: wgpu::Surface<'static>,
    surface_format: wgpu::TextureFormat,
}

impl CompositorHandler for LayerShellState {
    fn scale_factor_changed(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _new_factor: i32,
    ) {
        // Not needed for this example.
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
        self.render();
    }

    fn surface_enter(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _output: &protocol::wl_output::WlOutput,
    ) {
        // Not needed for this example.
    }

    fn surface_leave(
        &mut self,
        _conn: &Connection,
        _qh: &QueueHandle<Self>,
        _surface: &protocol::wl_surface::WlSurface,
        _output: &protocol::wl_output::WlOutput,
    ) {
        // Not needed for this example.
    }
}

impl OutputHandler for LayerShellState {
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

impl LayerShellHandler for LayerShellState {
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
            self.render();
        }
    }
}

impl SeatHandler for LayerShellState {
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
            println!("Set keyboard capability");
            let keyboard = self
                .seat_state
                .get_keyboard(qh, &seat, None)
                .expect("Failed to create keyboard");
            self.keyboard = Some(keyboard);
        }

        if capability == Capability::Pointer && self.pointer.is_none() {
            println!("Set pointer capability");
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

impl KeyboardHandler for LayerShellState {
    fn enter(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        surface: &protocol::wl_surface::WlSurface,
        _: u32,
        _: &[u32],
        keysyms: &[Keysym],
    ) {
        if self.layer.wl_surface() == surface {
            println!("Keyboard focus on window with pressed syms: {keysyms:?}");
            self.keyboard_focus = true;
        }
    }

    fn leave(
        &mut self,
        _: &Connection,
        _: &QueueHandle<Self>,
        _: &protocol::wl_keyboard::WlKeyboard,
        surface: &protocol::wl_surface::WlSurface,
        _: u32,
    ) {
        if self.layer.wl_surface() == surface {
            println!("Release keyboard focus on window");
            self.keyboard_focus = false;
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
        println!("Key press: {event:?}");
        if event.keysym == Keysym::Escape {
            self.exit = true;
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
        println!("Key release: {event:?}");
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
        println!("Update modifiers: {modifiers:?}");
    }
}

impl PointerHandler for LayerShellState {
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
            match event.kind {
                Enter { .. } => {
                    println!("Pointer entered @{:?}", event.position);
                }
                Leave { .. } => {
                    println!("Pointer left");
                }
                Motion { .. } => {}
                Press { button, .. } => {
                    println!("Press {:x} @ {:?}", button, event.position);
                }
                Release { button, .. } => {
                    println!("Release {:x} @ {:?}", button, event.position);
                }
                Axis {
                    horizontal,
                    vertical,
                    ..
                } => {
                    println!("Scroll H:{horizontal:?}, V:{vertical:?}");
                }
            }
        }
    }
}

impl ProvidesRegistryState for LayerShellState {
    fn registry(&mut self) -> &mut RegistryState {
        &mut self.registry_state
    }

    registry_handlers![OutputState, SeatState];
}

delegate_compositor!(LayerShellState);
delegate_output!(LayerShellState);

delegate_seat!(LayerShellState);
delegate_keyboard!(LayerShellState);
delegate_pointer!(LayerShellState);

delegate_layer!(LayerShellState);

delegate_registry!(LayerShellState);

struct LayerWindowHandle(RawWindowHandle, RawDisplayHandle);

impl HasWindowHandle for LayerWindowHandle {
    fn window_handle(&self) -> Result<WindowHandle<'_>, HandleError> {
        Ok(unsafe { WindowHandle::borrow_raw(self.0) })
    }
}

impl HasDisplayHandle for LayerWindowHandle {
    fn display_handle(&self) -> Result<DisplayHandle<'_>, HandleError> {
        Ok(unsafe { DisplayHandle::borrow_raw(self.1) })
    }
}

unsafe impl Send for LayerWindowHandle {}
unsafe impl Sync for LayerWindowHandle {}

pub async fn init(
    LayerShellOptions {
        layer,
        namespace,
        anchor,
        width,
        height,
    }: LayerShellOptions<'_>,
) -> (EventQueue<LayerShellState>, LayerShellState) {
    let connection = Connection::connect_to_env().unwrap();
    let (globals, event_queue) = globals::registry_queue_init(&connection).unwrap();
    let qh: QueueHandle<LayerShellState> = event_queue.handle();

    let compositor = CompositorState::bind(&globals, &qh).unwrap();
    let layer_shell = LayerShell::bind(&globals, &qh).expect("layer_shell is not available");

    let surface = compositor.create_surface(&qh);
    let surf_id = surface.id();
    let layer = layer_shell.create_layer_surface(&qh, surface, layer, namespace, None);

    layer.set_anchor(anchor);
    layer.set_keyboard_interactivity(KeyboardInteractivity::OnDemand);
    layer.set_size(width, height);
    layer.commit();

    let instance = wgpu::Instance::default();
    let adapter = instance
        .request_adapter(&wgpu::RequestAdapterOptions::default())
        .await
        .unwrap();

    let (device, queue) = adapter
        .request_device(&wgpu::DeviceDescriptor::default(), None)
        .await
        .unwrap();

    let surface_ptr = NonNull::new(surf_id.as_ptr() as *mut std::ffi::c_void).unwrap();
    let display_ptr =
        NonNull::new(connection.backend().display_ptr() as *mut std::ffi::c_void).unwrap();

    let wh = LayerWindowHandle(
        RawWindowHandle::Wayland(WaylandWindowHandle::new(surface_ptr)),
        RawDisplayHandle::Wayland(WaylandDisplayHandle::new(display_ptr)),
    );
    let st = wgpu::SurfaceTarget::Window(Box::new(wh));

    let wgpu_surface = instance.create_surface(st).unwrap();
    let cap = wgpu_surface.get_capabilities(&adapter);
    let surface_format = cap.formats[0];

    let state = LayerShellState {
        registry_state: RegistryState::new(&globals),
        seat_state: SeatState::new(&globals, &qh),
        output_state: OutputState::new(&globals, &qh),
        exit: false,
        first_configure: true,
        width,
        height,
        layer,
        keyboard: None,
        keyboard_focus: false,
        pointer: None,
        device,
        queue,
        surface: wgpu_surface,
        surface_format,
    };

    state.configure_surface();

    (event_queue, state)
}

impl LayerShellState {
    fn configure_surface(&self) {
        self.surface.configure(
            &self.device,
            &wgpu::SurfaceConfiguration {
                usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
                format: self.surface_format,
                view_formats: vec![self.surface_format.add_srgb_suffix()],
                alpha_mode: wgpu::CompositeAlphaMode::Auto,
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

    fn render(&mut self) {
        let surf_texture = self
            .surface
            .get_current_texture()
            .expect("failed to acquire surface texture");
        let tex_view = surf_texture
            .texture
            .create_view(&wgpu::TextureViewDescriptor {
                format: Some(self.surface_format.add_srgb_suffix()),
                ..Default::default()
            });
        let mut encoder = self.device.create_command_encoder(&Default::default());
        let pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
            label: None,
            color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                view: &tex_view,
                resolve_target: None,
                ops: wgpu::Operations {
                    load: wgpu::LoadOp::Clear(wgpu::Color::GREEN), // TODO
                    store: wgpu::StoreOp::Store,
                },
            })],
            depth_stencil_attachment: None,
            timestamp_writes: None,
            occlusion_query_set: None,
        });

        // TODO: egui render

        drop(pass);
        self.queue.submit(std::iter::once(encoder.finish()));
        surf_texture.present();
    }
}

#[cfg(test)]
mod tests {
    use crate::{init, LayerShellOptions};
    use smithay_client_toolkit::shell::wlr_layer::Anchor;

    #[test]
    fn it_works() {
        let runtime = tokio::runtime::Runtime::new().unwrap();

        runtime.block_on(async {
            let (mut eq, mut lst) = init(LayerShellOptions {
                anchor: Anchor::empty(),
                ..Default::default()
            })
            .await;

            loop {
                eq.blocking_dispatch(&mut lst).unwrap();

                if lst.exit {
                    eprintln!("stop alive");
                    break;
                }
            }
        })
    }
}
