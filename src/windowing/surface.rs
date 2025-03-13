use crate::start_time;
use crate::windowing::{convert, WindowingError};
use egui::{Context, Rect, ViewportId};
use egui_wgpu::{RenderState, ScreenDescriptor, WgpuConfiguration};
use smithay_client_toolkit::reexports::client::{protocol, Proxy};
use smithay_client_toolkit::seat::keyboard::RepeatInfo;
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

/// An owned wayland layer surface, with all render state and events related to it.
pub struct Surface {
    // which egui viewport was this surface created for?
    viewport_id: ViewportId,
    exit: bool,
    default_size: Option<(u32, u32)>,
    size: (u32, u32),
    layer_surface: LayerSurface,
    focused: bool,

    events: Vec<egui::Event>,
    modifiers: egui::Modifiers,

    wgpu_surface: wgpu::Surface<'static>,
    render_state: RenderState,
}

impl Surface {
    pub fn create(
        viewport_id: ViewportId,
        size: (u32, u32),
        layer_surface: LayerSurface,
        wgpu_surface: wgpu::Surface<'static>,
        render_state: RenderState,
    ) -> Self {
        Self {
            viewport_id,
            exit: false,
            default_size: Some(size),
            size,
            layer_surface,
            focused: false,
            events: Default::default(),
            modifiers: Default::default(),
            wgpu_surface,
            render_state,
        }
    }

    pub fn render(
        &mut self,
        ctx: &Context,
        render_ui: impl FnMut(&Context),
    ) -> Result<egui::PlatformOutput, WindowingError> {
        let output = self.update(ctx, render_ui);

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

        Ok(output.platform_output)
    }

    pub fn update(
        &mut self,
        ctx: &Context,
        render_ui: impl FnMut(&Context) + Sized,
    ) -> egui::FullOutput {
        let raw_input = self.next_raw_input();
        ctx.run(raw_input, render_ui)
    }

    fn next_raw_input(&mut self) -> egui::RawInput {
        let size = self.size();
        let events = std::mem::take(&mut self.events);

        egui::RawInput {
            viewport_id: self.viewport_id,
            screen_rect: Some(Rect::from_min_size(
                Default::default(),
                (size.0 as f32, size.1 as f32).into(),
            )),
            modifiers: self.modifiers(),
            focused: self.focused(),
            time: Some((std::time::Instant::now() - start_time()).as_secs_f64()),
            events,
            ..Default::default()
        }
    }

    pub fn configure_surface(&self) {
        let format = self.render_state.target_format;
        let (width, height) = self.size;

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

    pub fn update_size(&mut self, mut width: u32, mut height: u32) {
        if width == 0 {
            width = self.default_size.map(|(w, _)| w).unwrap_or(256);
        }
        if height == 0 {
            height = self.default_size.map(|(_, h)| h).unwrap_or(256);
        }

        self.size = (width, height);
        self.configure_surface();
    }

    pub fn handle_pointer_event(&mut self, event: &PointerEvent) {
        let pos = (event.position.0 as f32, event.position.1 as f32).into();
        let events = &mut self.events;
        match event.kind {
            Enter { .. } => {
                events.push(egui::Event::PointerMoved(pos));
            }
            Leave { .. } => events.push(egui::Event::PointerGone),
            Motion { .. } => {
                events.push(egui::Event::PointerMoved(pos));
            }
            Press { button, .. } => {
                events.push(egui::Event::PointerButton {
                    pos,
                    button: convert::pointer_button_to_egui(button),
                    pressed: true,
                    modifiers: self.modifiers,
                });
            }
            Release { button, .. } => {
                events.push(egui::Event::PointerButton {
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
            } => events.push(egui::Event::MouseWheel {
                unit: egui::MouseWheelUnit::Point,
                delta: (horizontal.absolute as f32, -vertical.absolute as f32).into(),
                modifiers: self.modifiers,
            }),
        }
    }

    pub fn has_events(&self) -> bool {
        !self.events.is_empty()
    }

    pub fn surface_id(&self) -> SurfaceId {
        self.layer_surface.wl_surface().into()
    }

    #[inline]
    pub fn size(&self) -> (u32, u32) {
        self.size
    }

    #[inline]
    pub fn focused(&self) -> bool {
        self.focused
    }

    #[inline]
    pub fn modifiers(&self) -> egui::Modifiers {
        self.modifiers
    }

    #[inline]
    pub fn viewport_id(&self) -> ViewportId {
        self.viewport_id
    }

    pub fn set_modifiers(&mut self, modifiers: egui::Modifiers) {
        self.modifiers = modifiers;
    }

    pub fn set_exit(&mut self) {
        self.exit = true;
    }

    pub fn on_focus(&mut self, focus: bool) {
        self.focused = focus;
        self.push_event(egui::Event::WindowFocused(focus));
    }

    pub fn on_key(&mut self, key: egui::Key, pressed: bool) {
        self.push_event(egui::Event::Key {
            key,
            physical_key: None,
            pressed,
            modifiers: self.modifiers(),
            repeat: false,
        });
    }

    #[inline]
    pub fn push_event(&mut self, event: egui::Event) {
        self.events.push(event);
    }
}

impl Default for LayerSurfaceOptions<'_> {
    fn default() -> Self {
        Self {
            wgpu_options: Default::default(),
            layer: Layer::Top,
            namespace: None,
            anchor: Anchor::empty(),
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
