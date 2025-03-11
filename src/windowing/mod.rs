pub mod app;
pub mod client;
mod convert;
pub mod surface;

use derive_more::with_trait::From;
use derive_more::{Display, Error};

use smithay_client_toolkit as sctk;

#[derive(Debug, Display, Error, From)]
pub enum WindowingError {
    NotWayland,
    GlobalError(sctk::reexports::client::globals::GlobalError),
    NoLayerShell,
    RequestDeviceError(wgpu::RequestDeviceError),
    SurfaceError(wgpu::SurfaceError),
    CreateSurfaceError(wgpu::CreateSurfaceError),
    WgpuError(egui_wgpu::WgpuError),
    #[allow(unused_qualifications)]
    WaylandError(wayland_backend::client::WaylandError),
    DispatchError(sctk::reexports::client::DispatchError),
    IoError(std::io::Error),
}
