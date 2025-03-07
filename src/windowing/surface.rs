use crate::windowing::{convert, WindowingError};
use egui::{Context, Rect, ViewportId};
use egui_wgpu::{RenderState, ScreenDescriptor, WgpuConfiguration};
use smithay_client_toolkit::reexports::client::{protocol, Proxy};
use smithay_client_toolkit::seat::pointer::PointerEvent;
use smithay_client_toolkit::seat::pointer::PointerEventKind::*;
use smithay_client_toolkit::shell::wlr_layer::{Anchor, Layer, LayerSurface};
use smithay_client_toolkit::shell::WaylandSurface;
use wayland_backend::client::ObjectId;

#[derive(Debug, Clone)]
pub struct LayerSurfaceOptions<'a> {
    pub wgpu_options: WgpuConfiguration,
    pub layer: Layer,
    pub namespace: Option<&'a str>,
    pub anchor: Anchor,
    pub width: u32,
    pub height: u32,
}

pub struct Surface {
    full_id: FullSurfaceId,
    exit: bool,
    first_configure: bool,
    default_size: Option<(u32, u32)>,
    size: (u32, u32),
    layer_surface: LayerSurface,
    focused: bool,

    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,
    start_time: std::time::Instant,

    wgpu_surface: wgpu::Surface<'static>,
    render_state: RenderState,
}

impl Surface {
    // pub(crate) fn first_draw<A: app::App>(&mut self, app: &mut A) where  {
    //     if self.first_configure {
    //         self.first_configure = false;
    //         let render_result = self.render(|ctx| {
    //             app.render(ctx);
    //         });
    //         log::trace!("(first configure) render result {:?}", render_result);
    //     }

    pub fn create(
        full_id: FullSurfaceId,
        size: (u32, u32),
        layer_surface: LayerSurface,
        start_time: std::time::Instant,
        wgpu_surface: wgpu::Surface<'static>,
        render_state: RenderState,
    ) -> Self {
        Self {
            full_id,
            exit: false,
            first_configure: true,
            default_size: Some(size),
            size,
            layer_surface,
            focused: false,
            events: vec![],
            modifiers: Default::default(),
            start_time,
            wgpu_surface,
            render_state,
        }
    }

    pub fn render(
        &mut self,
        ctx: &Context,
        render_ui: impl FnMut(&Context),
    ) -> Result<(), WindowingError> {
        let output_frame = self.wgpu_surface.get_current_texture()?;
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

        let raw_input = self.next_raw_input();
        let output = ctx.run(raw_input, render_ui);
        // TODO: output.platform_output
        let prims = ctx.tessellate(output.shapes, output.pixels_per_point);
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

    fn next_raw_input(&mut self) -> egui::RawInput {
        let events = std::mem::take(&mut self.events);

        egui::RawInput {
            viewport_id: self.full_id.viewport_id,
            screen_rect: Some(Rect::from_min_size(
                Default::default(),
                (self.size.0 as f32, self.size.1 as f32).into(),
            )),
            modifiers: self.modifiers,
            focused: self.focused,
            time: Some((std::time::Instant::now() - self.start_time).as_secs_f64()),
            events,
            ..Default::default()
        }
    }

    fn configure_surface(&self) {
        let format = self.render_state.target_format;
        let (width, height) = self.size;
        log::trace!("configure wgpu surface");

        self.wgpu_surface.configure(
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

    pub(crate) fn update_size(&mut self, mut width: u32, mut height: u32) {
        if width == 0 {
            width = self.default_size.map(|(w, _)| w).unwrap_or(256);
        }
        if height == 0 {
            height = self.default_size.map(|(_, h)| h).unwrap_or(256);
        }

        self.size = (width, height);
        self.configure_surface();
    }

    pub(crate) fn handle_pointer_event(&mut self, event: &PointerEvent) {
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
                unit: egui::MouseWheelUnit::Point,
                delta: (horizontal.absolute as f32, -vertical.absolute as f32).into(),
                modifiers: self.modifiers,
            }),
        }
    }

    pub(crate) fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    pub(crate) fn surface_id(&self) -> SurfaceId {
        self.layer_surface.wl_surface().into()
    }

    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    pub fn focused(&self) -> bool {
        self.focused
    }

    pub(crate) fn modifiers(&self) -> egui::Modifiers {
        self.modifiers
    }

    pub(crate) fn set_modifiers(&mut self, modifiers: egui::Modifiers) {
        self.modifiers = modifiers;
    }

    pub(crate) fn set_exit(&mut self) {
        self.exit = true;
    }

    pub(crate) fn is_first_configure(&self) -> bool {
        self.first_configure
    }

    pub(crate) fn on_focus(&mut self, focus: bool) {
        self.focused = focus;
        self.push_event(egui::Event::WindowFocused(focus));
    }

    pub(crate) fn on_key(&mut self, key: egui::Key, pressed: bool) {
        self.push_event(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            modifiers: self.modifiers,
            repeat: false,
        });
    }

    #[inline]
    pub(crate) fn push_event(&mut self, event: egui::Event) {
        self.events.push(event);
    }
}

impl Default for LayerSurfaceOptions<'_> {
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

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct SurfaceId(ObjectId);

impl From<&protocol::wl_surface::WlSurface> for SurfaceId {
    fn from(surface: &protocol::wl_surface::WlSurface) -> Self {
        Self(surface.id())
    }
}

impl From<ObjectId> for SurfaceId {
    fn from(value: ObjectId) -> Self {
        Self(value)
    }
}

#[derive(Debug, Clone)]
pub struct FullSurfaceId {
    pub surface_id: SurfaceId,
    pub viewport_id: ViewportId,
}
