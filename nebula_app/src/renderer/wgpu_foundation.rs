//! Experimental wgpu backend foundation.
//!
//! This module deliberately does not replace the OpenGL renderer yet. It owns
//! the platform-neutral instance/adapter/device/surface lifecycle so later
//! phases can migrate UI quads, images and glyphs without redesigning window
//! creation again.

use std::fmt;
use std::sync::Arc;

use winit::dpi::PhysicalSize;
use winit::window::Window;

/// User-facing backend preference for the future renderer selector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BackendPreference {
    /// Let wgpu select the best supported native backend.
    #[default]
    Auto,
    Vulkan,
    Dx12,
    Metal,
    Gl,
}

impl BackendPreference {
    fn backends(self) -> wgpu::Backends {
        match self {
            Self::Auto => wgpu::Backends::PRIMARY | wgpu::Backends::GL,
            Self::Vulkan => wgpu::Backends::VULKAN,
            Self::Dx12 => wgpu::Backends::DX12,
            Self::Metal => wgpu::Backends::METAL,
            Self::Gl => wgpu::Backends::GL,
        }
    }
}

#[derive(Debug)]
pub enum Error {
    Surface(wgpu::CreateSurfaceError),
    Adapter(wgpu::RequestAdapterError),
    Device(wgpu::RequestDeviceError),
    NoSurfaceFormat,
    NoPresentMode,
    NoAlphaMode,
    SurfaceAcquire(wgpu::SurfaceError),
}

impl fmt::Display for Error {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Surface(error) => write!(formatter, "unable to create wgpu surface: {error}"),
            Self::Adapter(error) => write!(formatter, "unable to select wgpu adapter: {error}"),
            Self::Device(error) => write!(formatter, "unable to create wgpu device: {error}"),
            Self::NoSurfaceFormat => formatter.write_str("wgpu surface exposes no texture format"),
            Self::NoPresentMode => formatter.write_str("wgpu surface exposes no present mode"),
            Self::NoAlphaMode => formatter.write_str("wgpu surface exposes no alpha mode"),
            Self::SurfaceAcquire(error) => {
                write!(formatter, "unable to acquire wgpu surface texture: {error}")
            },
        }
    }
}

impl std::error::Error for Error {}

/// Stable capability snapshot for diagnostics and the future renderer selector.
#[derive(Debug, Clone)]
pub struct AdapterCapabilities {
    pub name: String,
    pub backend: wgpu::Backend,
    pub device_type: wgpu::DeviceType,
    pub driver: String,
    pub driver_info: String,
    pub format: wgpu::TextureFormat,
    pub present_mode: wgpu::PresentMode,
    pub alpha_mode: wgpu::CompositeAlphaMode,
}

fn select_surface_format(formats: &[wgpu::TextureFormat]) -> Option<wgpu::TextureFormat> {
    formats.iter().copied().find(wgpu::TextureFormat::is_srgb).or_else(|| formats.first().copied())
}

fn select_present_mode(modes: &[wgpu::PresentMode]) -> Option<wgpu::PresentMode> {
    if modes.contains(&wgpu::PresentMode::Fifo) {
        Some(wgpu::PresentMode::Fifo)
    } else {
        modes.first().copied()
    }
}

fn select_alpha_mode(
    modes: &[wgpu::CompositeAlphaMode],
    transparent: bool,
) -> Option<wgpu::CompositeAlphaMode> {
    if transparent {
        modes
            .iter()
            .copied()
            .find(|mode| *mode != wgpu::CompositeAlphaMode::Opaque)
            .or_else(|| modes.first().copied())
    } else if modes.contains(&wgpu::CompositeAlphaMode::Opaque) {
        Some(wgpu::CompositeAlphaMode::Opaque)
    } else {
        modes.first().copied()
    }
}

/// Minimal persistent wgpu state shared by every future rendering phase.
///
/// One instance owns one adapter/device/queue and one window surface. No glyph
/// atlas, image cache or terminal buffer is duplicated at this stage.
pub struct WgpuFoundation {
    _window: Arc<Window>,
    pub instance: wgpu::Instance,
    pub adapter: wgpu::Adapter,
    pub device: wgpu::Device,
    pub queue: wgpu::Queue,
    pub surface: wgpu::Surface<'static>,
    pub config: wgpu::SurfaceConfiguration,
    pub capabilities: AdapterCapabilities,
    suspended: bool,
}

impl WgpuFoundation {
    pub fn new(
        window: Arc<Window>,
        size: PhysicalSize<u32>,
        transparent: bool,
        preference: BackendPreference,
    ) -> Result<Self, Error> {
        pollster::block_on(Self::new_async(window, size, transparent, preference))
    }

    async fn new_async(
        window: Arc<Window>,
        size: PhysicalSize<u32>,
        transparent: bool,
        preference: BackendPreference,
    ) -> Result<Self, Error> {
        let instance = wgpu::Instance::new(&wgpu::InstanceDescriptor {
            backends: preference.backends(),
            ..Default::default()
        });
        let surface = instance.create_surface(Arc::clone(&window)).map_err(Error::Surface)?;
        let adapter = instance
            .request_adapter(&wgpu::RequestAdapterOptions {
                power_preference: wgpu::PowerPreference::HighPerformance,
                force_fallback_adapter: false,
                compatible_surface: Some(&surface),
            })
            .await
            .map_err(Error::Adapter)?;

        let adapter_info = adapter.get_info();
        log::info!(
            "wgpu foundation adapter: {} ({:?}, {:?})",
            adapter_info.name,
            adapter_info.backend,
            adapter_info.device_type
        );

        let (device, queue) = adapter
            .request_device(&wgpu::DeviceDescriptor {
                label: Some("Nebula wgpu device"),
                required_features: wgpu::Features::empty(),
                required_limits: wgpu::Limits::downlevel_defaults(),
                memory_hints: wgpu::MemoryHints::MemoryUsage,
                trace: wgpu::Trace::Off,
            })
            .await
            .map_err(Error::Device)?;

        let capabilities = surface.get_capabilities(&adapter);
        // 选择策略保持确定性，避免不同平台枚举顺序让显示行为漂移。
        let format = select_surface_format(&capabilities.formats).ok_or(Error::NoSurfaceFormat)?;
        let present_mode =
            select_present_mode(&capabilities.present_modes).ok_or(Error::NoPresentMode)?;
        let alpha_mode =
            select_alpha_mode(&capabilities.alpha_modes, transparent).ok_or(Error::NoAlphaMode)?;
        let capabilities = AdapterCapabilities {
            name: adapter_info.name,
            backend: adapter_info.backend,
            device_type: adapter_info.device_type,
            driver: adapter_info.driver,
            driver_info: adapter_info.driver_info,
            format,
            present_mode,
            alpha_mode,
        };
        let config = wgpu::SurfaceConfiguration {
            usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
            format,
            width: size.width.max(1),
            height: size.height.max(1),
            present_mode,
            alpha_mode,
            view_formats: Vec::new(),
            desired_maximum_frame_latency: 2,
        };
        surface.configure(&device, &config);

        Ok(Self {
            _window: window,
            instance,
            adapter,
            device,
            queue,
            surface,
            config,
            capabilities,
            suspended: size.width == 0 || size.height == 0,
        })
    }

    /// Reconfigure the swapchain after a physical resize. Zero-sized windows
    /// are suspended instead of manufacturing invalid surface dimensions.
    pub fn resize(&mut self, size: PhysicalSize<u32>) {
        if size.width == 0 || size.height == 0 {
            self.suspended = true;
            return;
        }
        self.suspended = false;
        self.config.width = size.width;
        self.config.height = size.height;
        self.surface.configure(&self.device, &self.config);
    }

    /// Reconfigure after resume or `SurfaceError::Lost`.
    pub fn reconfigure(&mut self) {
        if !self.suspended {
            self.surface.configure(&self.device, &self.config);
        }
    }

    /// Phase-1 smoke frame: clear and present without allocating a pipeline.
    /// Future phases replace this with shared UI render commands.
    pub fn clear_and_present(&mut self, color: wgpu::Color) -> Result<(), Error> {
        if self.suspended {
            return Ok(());
        }
        let frame = self.surface.get_current_texture().map_err(Error::SurfaceAcquire)?;
        let view = frame.texture.create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = self.device.create_command_encoder(&wgpu::CommandEncoderDescriptor {
            label: Some("Nebula wgpu foundation clear"),
        });
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                label: Some("Nebula wgpu foundation pass"),
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(color),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                depth_stencil_attachment: None,
                timestamp_writes: None,
                occlusion_query_set: None,
            });
        }
        self.queue.submit(Some(encoder.finish()));
        frame.present();
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::{BackendPreference, select_alpha_mode, select_present_mode, select_surface_format};

    #[test]
    fn automatic_backend_keeps_native_and_gl_fallbacks() {
        let backends = BackendPreference::Auto.backends();
        assert!(backends.contains(wgpu::Backends::PRIMARY));
        assert!(backends.contains(wgpu::Backends::GL));
    }

    #[test]
    fn surface_format_prefers_srgb_then_first_available() {
        let formats = [wgpu::TextureFormat::Rgba8Unorm, wgpu::TextureFormat::Bgra8UnormSrgb];
        assert_eq!(select_surface_format(&formats), Some(wgpu::TextureFormat::Bgra8UnormSrgb));
        assert_eq!(
            select_surface_format(&[wgpu::TextureFormat::Rgba8Unorm]),
            Some(wgpu::TextureFormat::Rgba8Unorm)
        );
        assert_eq!(select_surface_format(&[]), None);
    }

    #[test]
    fn present_mode_prefers_fifo_and_handles_empty_capabilities() {
        assert_eq!(
            select_present_mode(&[wgpu::PresentMode::Immediate, wgpu::PresentMode::Fifo]),
            Some(wgpu::PresentMode::Fifo)
        );
        assert_eq!(
            select_present_mode(&[wgpu::PresentMode::Immediate]),
            Some(wgpu::PresentMode::Immediate)
        );
        assert_eq!(select_present_mode(&[]), None);
    }

    #[test]
    fn alpha_mode_respects_transparency_without_panicking() {
        let modes = [wgpu::CompositeAlphaMode::Opaque, wgpu::CompositeAlphaMode::PreMultiplied];
        assert_eq!(select_alpha_mode(&modes, true), Some(wgpu::CompositeAlphaMode::PreMultiplied));
        assert_eq!(select_alpha_mode(&modes, false), Some(wgpu::CompositeAlphaMode::Opaque));
        assert_eq!(select_alpha_mode(&[], true), None);
    }
}
