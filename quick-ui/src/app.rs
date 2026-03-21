use winit::{
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::Window,
};
use std::sync::Arc;
use vello::{Scene, Renderer, RendererOptions, util::RenderContext, peniko::Color, kurbo::Affine};
use taffy::{TaffyTree, AvailableSpace};
use automerge::ReadDoc;
use crate::state::State;
use crate::widget::Widget;
use crate::text_render::TextRenderer;

use crate::theme::Theme;

pub struct App {
    state: State,
    root_widget: Box<dyn Widget>,
    text_renderer: TextRenderer,
    taffy: TaffyTree<()>,
    root_node: Option<taffy::NodeId>,
    layout_dirty: bool,
    needs_sync: bool,
    pub theme: Theme,
}

impl App {
    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
        Self {
            state,
            root_widget,
            text_renderer: TextRenderer::new(),
            taffy: TaffyTree::new(),
            root_node: None,
            layout_dirty: true,
            needs_sync: true,
            theme: Theme::dark(),
        }
    }

    pub fn run(mut self) {
        let event_loop = EventLoop::new().unwrap();
        let window_attributes = Window::default_attributes()
            .with_title("QuickUI App")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        #[allow(deprecated)]
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());

        let mut render_cx = RenderContext::new();
        
        let mut surface = pollster::block_on(render_cx.create_surface(window.clone(), window.inner_size().width, window.inner_size().height, vello::wgpu::PresentMode::AutoVsync)).unwrap();
        
        let device_handle = &render_cx.devices[surface.dev_id];
        
        // Force sRGB format to prevent washed out colors
        surface.config.format = surface.config.format.add_srgb_suffix();
        // Set alpha mode to Auto (typically opaque on Wayland unless transparent window is requested)
        surface.config.alpha_mode = vello::wgpu::CompositeAlphaMode::Auto;
        surface.surface.configure(&device_handle.device, &surface.config);
        
        let mut renderer = Renderer::new(
            &device_handle.device,
            RendererOptions {
                surface_format: Some(surface.config.format),
                use_cpu: false,
                antialiasing_support: vello::AaSupport::all(),
                num_init_threads: None,
            },
        ).expect("Failed to create Vello renderer");

        let mut scene = Scene::new();
        
        // Initial build of the layout tree
        self.root_node = Some(self.root_widget.build_layout(&mut self.taffy, &self.theme));

        let mut theme_idx = 0;
        let themes = [
            Theme::dark(),
            Theme::dracula(),
            Theme::nord(),
            Theme::gruvbox(),
            Theme::solarized(),
            Theme::light(),
        ];

        #[allow(deprecated)]
        event_loop.run(move |event, elwt| {
            match event {
                Event::WindowEvent { event, window_id } if window_id == window.id() => {
                    match event {
                        WindowEvent::CloseRequested => elwt.exit(),
                        WindowEvent::Resized(size) => {
                            render_cx.resize_surface(&mut surface, size.width, size.height);
                            self.layout_dirty = true;
                            window.request_redraw();
                        }
                        WindowEvent::RedrawRequested => {
                            let width = surface.config.width;
                            let height = surface.config.height;

                            scene.reset();
                            
                            let scale_factor = window.scale_factor() as f32;
                            
                            // 1. Handle State-based Logic (Theme cycling, etc.)
                            // Check if a theme cycle was requested via button
                            if let Ok(Some((automerge::Value::Scalar(s), _))) = self.state.doc.get(&self.state.root, "request_theme_cycle") {
                                if let automerge::ScalarValue::Boolean(true) = s.as_ref() {
                                    theme_idx = (theme_idx + 1) % themes.len();
                                    self.theme = themes[theme_idx].clone();
                                    self.layout_dirty = true;
                                    self.needs_sync = true;
                                    
                                    use automerge::transaction::Transactable;
                                    let _ = self.state.doc.put(&self.state.root, "request_theme_cycle", false);
                                }
                            }

                            // Always keep the current theme name synced in state for labels to read
                            use automerge::transaction::Transactable;
                            let _ = self.state.doc.put(&self.state.root, "current_theme_name", self.theme.name.as_str());

                            // 2. Sync Widget Tree
                            if self.needs_sync {
                                self.root_widget.sync(&self.state.doc, &self.state.root);
                                self.needs_sync = false;
                            }
                            
                            // 3. Layout
                            let root_node = self.root_node.unwrap();
                            if self.layout_dirty {
                                // Set root constraints
                                self.taffy.compute_layout_with_measure(
                                    root_node,
                                    taffy::Size {
                                        width: AvailableSpace::Definite((width as f32) / scale_factor),
                                        height: AvailableSpace::Definite((height as f32) / scale_factor),
                                    },
                                    |_, _, _, _, _| taffy::Size::ZERO
                                ).unwrap();
                                
                                self.root_widget.apply_layout(&self.taffy, root_node, vello::kurbo::Point::new(0.0, 0.0));
                                self.layout_dirty = false;
                            }
                            
                            // 4. Draw
                            let transform = Affine::scale(scale_factor as f64);
                            self.root_widget.draw(&mut scene, transform, &mut self.text_renderer, &self.theme);
                            
                            // 4. Draw Remote Cursors
                            for (_, cursor) in &self.state.collab.cursors {
                                let mut path = vello::kurbo::BezPath::new();
                                path.move_to(vello::kurbo::Point::new(0.0, 0.0));
                                path.line_to(vello::kurbo::Point::new(14.0, 14.0));
                                path.line_to(vello::kurbo::Point::new(6.0, 16.0));
                                path.line_to(vello::kurbo::Point::new(0.0, 22.0));
                                path.close_path();

                                let cursor_transform = transform * Affine::translate((cursor.x as f64, cursor.y as f64));
                                scene.fill(
                                    vello::peniko::Fill::NonZero,
                                    cursor_transform,
                                    cursor.color.clone(),
                                    None,
                                    &path,
                                );
                                // Outline
                                scene.stroke(
                                    &vello::kurbo::Stroke::new(1.5),
                                    cursor_transform,
                                    Color::WHITE,
                                    None,
                                    &path,
                                );
                            }

                            let surface_texture = surface.surface.get_current_texture().expect("failed to get surface texture");
                            let device_handle = &render_cx.devices[surface.dev_id];

                            renderer.render_to_surface(
                                &device_handle.device,
                                &device_handle.queue,
                                &scene,
                                &surface_texture,
                                &vello::RenderParams {
                                    base_color: self.theme.bg_app,
                                    width,
                                    height,
                                    antialiasing_method: vello::AaConfig::Area,
                                }
                            ).expect("Failed to render to surface");

                            surface_texture.present();
                            device_handle.device.poll(vello::wgpu::Maintain::Poll);
                        }
                        _ => {
                            // Forward events to widget
                            let mut handled = false;
                            let scale_factor = window.scale_factor();
                            self.root_widget.handle_event(&event, scale_factor, &mut self.state.doc, &self.state.root, &mut handled);
                            if handled {
                                self.needs_sync = true;
                                self.layout_dirty = true;
                                window.request_redraw();
                            }
                        }
                    }
                }
                Event::AboutToWait => {
                    // We could sync remote CRDT changes here
                }
                _ => {}
            }
        }).unwrap();
    }
}
