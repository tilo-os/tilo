use winit::{event_loop::EventLoop, window::Window};
use vello::util::RenderContext;
use std::sync::Arc;

fn main() {
    let event_loop = EventLoop::new().unwrap();
    let window_attributes = Window::default_attributes()
        .with_title("Format Test")
        .with_visible(false);
    
    #[allow(deprecated)]
    let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
    
    let mut render_cx = RenderContext::new();
    let surface = pollster::block_on(render_cx.create_surface(
        window.clone(),
        800,
        600,
        vello::wgpu::PresentMode::AutoVsync
    )).unwrap();
    
    println!("Surface format: {:?}", surface.config.format);
}
