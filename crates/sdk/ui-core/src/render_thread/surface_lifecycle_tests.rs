//! Drive the native acquisition path through real wgpu Surface/SurfaceTexture
//! wrappers. The custom swapchain makes suboptimal/occluded results deterministic;
//! the noop device needs neither a display nor a GPU.

use std::collections::VecDeque;
use std::pin::Pin;
use std::sync::{Arc, Mutex};
use vello::wgpu::{self, custom::*};

use super::{FrameOutcome, acquire_surface_texture};

#[derive(Debug, Default)]
struct Swapchain {
    configured: bool,
    acquired: bool,
    discarded: usize,
    presented: usize,
    statuses: VecDeque<wgpu::SurfaceStatus>,
}

#[derive(Clone, Debug, Default)]
struct Backend(Arc<Mutex<Swapchain>>);

impl InstanceInterface for Backend {
    fn new(_: wgpu::InstanceDescriptor) -> Self {
        Self::default()
    }

    unsafe fn create_surface(
        &self,
        _: wgpu::SurfaceTargetUnsafe,
    ) -> Result<DispatchSurface, wgpu::CreateSurfaceError> {
        Ok(DispatchSurface::custom(self.clone()))
    }

    fn request_adapter(
        &self,
        _: &wgpu::RequestAdapterOptions<'_, '_>,
    ) -> Pin<Box<dyn RequestAdapterFuture>> {
        unreachable!("the test uses Device::noop")
    }

    fn poll_all_devices(&self, _: bool) -> bool {
        true
    }

    fn wgsl_language_features(&self) -> wgpu::WgslLanguageFeatures {
        wgpu::WgslLanguageFeatures::empty()
    }

    fn enumerate_adapters(&self, _: wgpu::Backends) -> Pin<Box<dyn EnumerateAdapterFuture>> {
        unreachable!("the test uses Device::noop")
    }
}

impl SurfaceInterface for Backend {
    fn get_capabilities(&self, _: &DispatchAdapter) -> wgpu::SurfaceCapabilities {
        unreachable!("the test supplies the surface configuration")
    }

    fn configure(&self, _: &DispatchDevice, _: &wgpu::SurfaceConfiguration) {
        let mut state = self.0.lock().unwrap();
        // Like wgpu-core, configuration retires the old swapchain before
        // checking for an outstanding surface texture. A late discard would
        // therefore target an unconfigured surface.
        state.configured = false;
        assert!(!state.acquired, "reconfigured with a live surface texture");
        state.configured = true;
    }

    fn get_current_texture(
        &self,
    ) -> (
        Option<DispatchTexture>,
        wgpu::SurfaceStatus,
        DispatchSurfaceOutputDetail,
    ) {
        let mut state = self.0.lock().unwrap();
        assert!(state.configured);
        assert!(!state.acquired);
        let status = state.statuses.pop_front().unwrap();
        let texture = matches!(
            status,
            wgpu::SurfaceStatus::Good | wgpu::SurfaceStatus::Suboptimal
        )
        .then(|| {
            state.acquired = true;
            DispatchTexture::custom(self.clone())
        });
        (
            texture,
            status,
            DispatchSurfaceOutputDetail::custom(self.clone()),
        )
    }
}

impl TextureInterface for Backend {
    fn create_view(&self, _: &wgpu::TextureViewDescriptor<'_>) -> DispatchTextureView {
        unreachable!("acquisition tests do not draw")
    }

    fn destroy(&self) {}
}

impl SurfaceOutputDetailInterface for Backend {
    fn present(&self) {
        let mut state = self.0.lock().unwrap();
        assert!(state.configured && state.acquired);
        state.acquired = false;
        state.presented += 1;
    }

    fn texture_discard(&self) {
        let mut state = self.0.lock().unwrap();
        assert!(state.configured, "discard after surface was unconfigured");
        assert!(state.acquired);
        state.acquired = false;
        state.discarded += 1;
    }
}

fn surface(backend: &Backend) -> wgpu::Surface<'static> {
    let instance = wgpu::Instance::from_custom(backend.clone());
    // SAFETY: this custom backend ignores the target and owns no native handles.
    // No window or display is created or accessed by the test.
    unsafe {
        instance.create_surface_unsafe(wgpu::SurfaceTargetUnsafe::RawHandle {
            raw_display_handle: None,
            raw_window_handle: wgpu::rwh::RawWindowHandle::Xlib(wgpu::rwh::XlibWindowHandle::new(
                0,
            )),
        })
    }
    .unwrap()
}

#[test]
fn suboptimal_texture_is_discarded_before_reconfigure_and_next_frame_presents() {
    let backend = Backend::default();
    backend.0.lock().unwrap().statuses.extend([
        wgpu::SurfaceStatus::Suboptimal,
        wgpu::SurfaceStatus::Occluded,
        wgpu::SurfaceStatus::Outdated,
        wgpu::SurfaceStatus::Timeout,
        wgpu::SurfaceStatus::Good,
    ]);
    let surface = surface(&backend);
    let (device, _queue) = wgpu::Device::noop(&wgpu::DeviceDescriptor::default());
    let mut config = wgpu::SurfaceConfiguration {
        usage: wgpu::TextureUsages::RENDER_ATTACHMENT,
        format: wgpu::TextureFormat::Bgra8Unorm,
        width: 800,
        height: 600,
        present_mode: wgpu::PresentMode::Fifo,
        desired_maximum_frame_latency: 2,
        alpha_mode: wgpu::CompositeAlphaMode::Opaque,
        view_formats: vec![],
    };
    surface.configure(&device, &config);

    config.width = 1024;
    assert!(matches!(
        acquire_surface_texture(&surface, || {
            surface.configure(&device, &config);
            FrameOutcome::Reconfigured
        }),
        Err(FrameOutcome::Reconfigured)
    ));
    // Also catch a leaked/forgotten texture, not just a late discard.
    assert_eq!(backend.0.lock().unwrap().discarded, 1);
    assert!(matches!(
        acquire_surface_texture(&surface, || panic!("occlusion must not reconfigure")),
        Err(FrameOutcome::Occluded)
    ));
    config.height = 768;
    assert!(matches!(
        acquire_surface_texture(&surface, || {
            surface.configure(&device, &config);
            FrameOutcome::Reconfigured
        }),
        Err(FrameOutcome::Reconfigured)
    ));
    assert!(matches!(
        acquire_surface_texture(&surface, || panic!("timeout must not reconfigure")),
        Err(FrameOutcome::Skipped)
    ));
    let texture =
        acquire_surface_texture(&surface, || panic!("success must not reconfigure")).unwrap();
    assert!(backend.0.lock().unwrap().acquired);
    texture.present();
    let state = backend.0.lock().unwrap();
    assert_eq!(state.discarded, 1);
    assert_eq!(state.presented, 1);
    assert!(!state.acquired);
    assert!(state.statuses.is_empty());
}

#[test]
fn lost_and_validation_acquisitions_still_stop_the_worker() {
    for status in [wgpu::SurfaceStatus::Lost, wgpu::SurfaceStatus::Validation] {
        let backend = Backend::default();
        backend.0.lock().unwrap().statuses.push_back(status);
        let surface = surface(&backend);
        // These statuses return before wgpu needs a configured texture descriptor.
        backend.0.lock().unwrap().configured = true;
        assert!(matches!(
            acquire_surface_texture(&surface, || panic!("lost surfaces need recreation")),
            Err(FrameOutcome::SurfaceGone)
        ));
        let state = backend.0.lock().unwrap();
        assert_eq!(state.discarded, 0);
        assert!(!state.acquired);
    }
}
