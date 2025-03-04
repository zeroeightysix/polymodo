use crate::{app, WindowingError};
use egui::{Context, Rect};
use egui_wgpu::{RenderState, ScreenDescriptor, WgpuConfiguration};
use smithay_client_toolkit::shell::wlr_layer::{Anchor, Layer, LayerSurface};

#[derive(Debug, Clone)]
pub struct LayerShellOptions<'a> {
    pub wgpu_options: WgpuConfiguration,
    pub layer: Layer,
    pub namespace: Option<&'a str>,
    pub anchor: Anchor,
    pub width: u32,
    pub height: u32,
}

pub struct Surface {
    pub(crate) exit: bool,
    pub(crate) first_configure: bool,
    pub(crate) default_size: Option<(u32, u32)>,
    pub(crate) size: (u32, u32),
    pub(crate) layer: LayerSurface,
    pub(crate) focused: bool,

    pub events: Vec<egui::Event>,
    pub ctx: Context,
    pub(crate) modifiers: egui::Modifiers,
    pub(crate) start_time: std::time::Instant,

    pub(crate) wpgu_surface: wgpu::Surface<'static>,
    pub(crate) render_state: RenderState,
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

    pub(crate) fn update_size(&mut self, width: u32, height: u32) {
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
