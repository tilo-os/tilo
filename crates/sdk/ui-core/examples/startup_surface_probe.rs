//! Exercise actual SDK surface selection and presentation in an owned display.
//! This is a GPU diagnostic, not a normal application or a sandbox qualification.

use std::sync::Arc;

use tilo_ui::gpu::RenderContext;
use tilo_ui::vello::wgpu;
use tilo_ui::winit::{
    application::ApplicationHandler,
    event::WindowEvent,
    event_loop::{ActiveEventLoop, EventLoop},
    window::{Window, WindowId},
};

#[derive(Default)]
struct Probe {
    error: Option<String>,
    window: Option<Arc<Window>>,
    reuse_device: bool,
}

impl Probe {
    fn run(&mut self, event_loop: &ActiveEventLoop) -> Result<(), Box<dyn std::error::Error>> {
        let mut context = RenderContext::new();
        if self.reuse_device {
            pollster::block_on(context.device(None)).ok_or("could not prepare a device")?;
        }
        let window = Arc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title("Tilo surface startup probe")
                    .with_inner_size(tilo_ui::winit::dpi::PhysicalSize::new(320, 240)),
            )?,
        );
        let size = window.inner_size();
        let started = tilo_ui::time::now();
        let surface = pollster::block_on(context.create_surface_unconfigured(
            window.clone(),
            size.width.max(1),
            size.height.max(1),
            wgpu::PresentMode::AutoVsync,
        ))?;
        let handle = context
            .devices
            .get(surface.dev_id)
            .ok_or("missing selected device")?;
        tilo_startup_metrics::mark("probe-surface-checked-configure-start");
        let configure_scope = handle
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        surface.surface.configure(&handle.device, &surface.config);
        if let Some(error) = pollster::block_on(configure_scope.pop()) {
            return Err(error.to_string().into());
        }
        tilo_startup_metrics::mark("probe-surface-checked-configure-done");
        let surface_ms = started.elapsed().as_secs_f64() * 1000.0;
        let scope = handle
            .device
            .push_error_scope(wgpu::ErrorFilter::Validation);
        let frame = match surface.surface.get_current_texture() {
            wgpu::CurrentSurfaceTexture::Success(frame) => frame,
            other => return Err(format!("surface acquisition failed: {other:?}").into()),
        };
        let view = frame
            .texture
            .create_view(&wgpu::TextureViewDescriptor::default());
        let mut encoder = handle
            .device
            .create_command_encoder(&wgpu::CommandEncoderDescriptor::default());
        {
            let _pass = encoder.begin_render_pass(&wgpu::RenderPassDescriptor {
                color_attachments: &[Some(wgpu::RenderPassColorAttachment {
                    view: &view,
                    depth_slice: None,
                    resolve_target: None,
                    ops: wgpu::Operations {
                        load: wgpu::LoadOp::Clear(wgpu::Color::BLACK),
                        store: wgpu::StoreOp::Store,
                    },
                })],
                ..Default::default()
            });
        }
        handle.queue.submit([encoder.finish()]);
        frame.present();
        handle.device.poll(wgpu::PollType::wait_indefinitely())?;
        if let Some(error) = pollster::block_on(scope.pop()) {
            return Err(error.to_string().into());
        }
        let adapter = handle.adapter.get_info();
        println!(
            "{}",
            serde_json::json!({
                "scope": "SDK surface creation and validated clear submission; not display/input latency",
                "reuse_device": self.reuse_device,
                "surface_ms": surface_ms,
                "submit_and_poll_ms": started.elapsed().as_secs_f64() * 1000.0,
                "adapter": adapter.name,
                "backend": format!("{:?}", adapter.backend),
                "format": format!("{:?}", surface.format),
                "device_count": context.devices.len(),
            })
        );
        self.window = Some(window);
        Ok(())
    }
}

impl ApplicationHandler for Probe {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        self.error = self.run(event_loop).err().map(|error| error.to_string());
        tilo_startup_metrics::dump();
        event_loop.exit();
    }

    fn window_event(&mut self, _: &ActiveEventLoop, _: WindowId, _: WindowEvent) {}
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let reuse_device = match std::env::args().nth(1).as_deref() {
        None => false,
        Some("--reuse-device") => true,
        Some(_) => return Err("expected no argument or --reuse-device".into()),
    };
    tilo_startup_metrics::init();
    let mut probe = Probe {
        reuse_device,
        ..Probe::default()
    };
    EventLoop::new()?.run_app(&mut probe)?;
    probe.error.map_or(Ok(()), |error| Err(error.into()))
}
