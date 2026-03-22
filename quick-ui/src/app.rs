use winit::{
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::Window,
};
use std::sync::Arc;
use vello::{Scene, Renderer, RendererOptions, util::RenderContext, peniko::Color, kurbo::Affine};
use taffy::{TaffyTree, AvailableSpace, NodeId};
use automerge::ReadDoc;
use crate::state::State;
use crate::widget::Widget;
use crate::text_render::TextRenderer;

use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
enum Direction {
    Up,
    Down,
    Left,
    Right,
}

pub struct App {
    state: State,
    root_widget: Box<dyn Widget>,
    text_renderer: TextRenderer,
    taffy: TaffyTree<()>,
    root_node: Option<NodeId>,
    node_map: std::collections::HashMap<NodeId, *mut dyn Widget>,
    layout_dirty: bool,
    needs_sync: bool,
    pub theme: Theme,
    pub debug_mode: bool,
    pub focused_node: Option<NodeId>,
    pub focusable_order: Vec<NodeId>,
    modifiers: winit::event::Modifiers,
}

impl App {
    pub fn new(state: State, root_widget: Box<dyn Widget>) -> Self {
        Self {
            state,
            root_widget,
            text_renderer: TextRenderer::new(),
            taffy: TaffyTree::new(),
            root_node: None,
            node_map: std::collections::HashMap::new(),
            layout_dirty: true,
            needs_sync: true,
            theme: Theme::dark(),
            debug_mode: std::env::var("QUICK_UI_DEBUG").is_ok(),
            focused_node: None,
            focusable_order: Vec::new(),
            modifiers: winit::event::Modifiers::default(),
        }
    }

    fn update_focusable_order(&mut self) {
        self.focusable_order.clear();
        
        fn collect_focusable(node_id: NodeId, widget: &dyn Widget, taffy: &TaffyTree<()>, order: &mut Vec<NodeId>) {
            if widget.is_focusable() {
                order.push(node_id);
            }
            let children = widget.children();
            if let Ok(child_nodes) = taffy.children(node_id) {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    collect_focusable(child_node, *child_widget, taffy, order);
                }
            }
        }

        let root_node = self.root_node.expect("Root node must exist");
        collect_focusable(root_node, self.root_widget.as_ref(), &self.taffy, &mut self.focusable_order);
    }

    fn navigate_focus(&mut self, direction: Direction) {
        let current_id = match self.focused_node {
            Some(id) => id,
            None => {
                if !self.focusable_order.is_empty() {
                    let first = self.focusable_order[0];
                    self.set_focused_node(Some(first));
                }
                return;
            }
        };

        let current_rect = if let Some(&w_ptr) = self.node_map.get(&current_id) {
            unsafe { (*w_ptr).get_rect() }
        } else {
            return;
        };
        let current_center = current_rect.center();

        let mut best_candidate = None;
        let mut best_score = f64::MAX;

        for &candidate_id in &self.focusable_order {
            if candidate_id == current_id { continue; }

            if let Some(&candidate_ptr) = self.node_map.get(&candidate_id) {
                let candidate_rect = unsafe { (*candidate_ptr).get_rect() };
                let candidate_center = candidate_rect.center();
                let vector = vello::kurbo::Vec2::new(candidate_center.x - current_center.x, candidate_center.y - current_center.y);

                let (primary_dist, secondary_dist, in_direction) = match direction {
                    Direction::Up => (-vector.y, vector.x.abs(), vector.y < -1.0),
                    Direction::Down => (vector.y, vector.x.abs(), vector.y > 1.0),
                    Direction::Left => (-vector.x, vector.y.abs(), vector.x < -1.0),
                    Direction::Right => (vector.x, vector.y.abs(), vector.x > 1.0),
                };

                if in_direction {
                    // Score: primary distance + weighted secondary distance to favor alignment
                    let score = primary_dist + (secondary_dist * 2.0);
                    if score < best_score {
                        best_score = score;
                        best_candidate = Some(candidate_id);
                    }
                }
            }
        }

        if let Some(new_id) = best_candidate {
            self.set_focused_node(Some(new_id));
        }
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        if self.focused_node != new_id {
            // Blur old
            if let Some(old_id) = self.focused_node {
                if let Some(&w_ptr) = self.node_map.get(&old_id) {
                    unsafe { (*w_ptr).set_focus(false); }
                }
            }
            // Focus new
            self.focused_node = new_id;
            if let Some(id) = new_id {
                if let Some(&w_ptr) = self.node_map.get(&id) {
                    unsafe { (*w_ptr).set_focus(true); }
                }
            }
            self.layout_dirty = true;
            self.needs_sync = true;
        }
    }

    pub fn run(mut self) {
        let event_loop = EventLoop::new().unwrap();
        let window_attributes = Window::default_attributes()
            .with_title("QuickUI App")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0))
            .with_min_inner_size(winit::dpi::LogicalSize::new(400.0, 300.0));
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
        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(&mut self.taffy, &self.theme, &mut self.node_map));
        self.update_focusable_order();

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

                            // DX Stunt: Save/Load session
                            if let Ok(Some((automerge::Value::Scalar(s), _))) = self.state.doc.get(&self.state.root, "request_save") {
                                if let automerge::ScalarValue::Boolean(true) = s.as_ref() {
                                    let data = self.state.save();
                                    let _ = std::fs::write("session.crdt", data);
                                    let _ = self.state.doc.put(&self.state.root, "request_save", false);
                                    println!("Session saved to session.crdt");
                                }
                            }
                            if let Ok(Some((automerge::Value::Scalar(s), _))) = self.state.doc.get(&self.state.root, "request_load") {
                                if let automerge::ScalarValue::Boolean(true) = s.as_ref() {
                                    if let Ok(data) = std::fs::read("session.crdt") {
                                        if let Ok(new_state) = State::load(&data) {
                                            self.state = new_state;
                                            self.layout_dirty = true;
                                            self.needs_sync = true;
                                            println!("Session loaded from session.crdt");
                                        }
                                    }
                                    let _ = self.state.doc.put(&self.state.root, "request_load", false);
                                }
                            }

                            // Only keep the current theme name synced in state if it changed
                            use automerge::transaction::Transactable;
                            let theme_changed = match self.state.doc.get(&self.state.root, "current_theme_name") {
                                Ok(Some((automerge::Value::Scalar(s), _))) => s.to_string() != self.theme.name,
                                _ => true,
                            };
                            if theme_changed {
                                let _ = self.state.doc.put(&self.state.root, "current_theme_name", self.theme.name.as_str());
                            }

                            // 2. Sync Widget Tree
                            if self.needs_sync {
                                self.root_widget.sync(&self.state.doc, &self.state.root);
                                self.needs_sync = false;
                            }
                            
                            // 3. Layout
                            let root_node = self.root_node.unwrap();
                            if self.layout_dirty {
                                // Set root constraints
                                let node_map = &self.node_map;
                                let text_renderer = &mut self.text_renderer;
                                
                                self.taffy.compute_layout_with_measure(
                                    root_node,
                                    taffy::Size {
                                        width: AvailableSpace::Definite((width as f32) / scale_factor),
                                        height: AvailableSpace::Definite((height as f32) / scale_factor),
                                    },
                                    |known, available, node_id, _context, _tree| {
                                        if let Some(&widget_ptr) = node_map.get(&node_id) {
                                            unsafe {
                                                (*widget_ptr).measure(known, available, text_renderer)
                                            }
                                        } else {
                                            taffy::Size::ZERO
                                        }
                                    }
                                ).unwrap();
                                
                                self.root_widget.apply_layout(&self.taffy, root_node, vello::kurbo::Point::new(0.0, 0.0));
                                self.layout_dirty = false;
                            }
                            
                            // 4. Draw
                            let transform = Affine::scale(scale_factor as f64);
                            self.root_widget.draw(&mut scene, transform, &mut self.text_renderer, &self.theme);
                            
                            // Debug Borders
                            #[cfg(debug_assertions)]
                            if self.debug_mode {
                                fn draw_debug_recursive(widget: &dyn Widget, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, depth: usize) {
                                    let rect = widget.get_rect();
                                    let color = match depth % 3 {
                                        0 => Color::rgb8(255, 0, 255), // Magenta
                                        1 => Color::rgb8(0, 255, 255), // Cyan
                                        _ => Color::rgb8(255, 255, 0), // Yellow
                                    };
                                    
                                    scene.stroke(
                                        &vello::kurbo::Stroke::new(1.0),
                                        transform,
                                        color,
                                        None,
                                        &rect,
                                    );
                                    
                                    // Draw widget name
                                    let name = widget.name();
                                    text_renderer.draw_text(
                                        scene,
                                        name,
                                        10.0,
                                        color,
                                        transform * Affine::translate((rect.x0 + 2.0, rect.y0 + 10.0)),
                                        None,
                                    );

                                    for child in widget.children() {
                                        draw_debug_recursive(child, scene, transform, text_renderer, theme, depth + 1);
                                    }
                                }
                                draw_debug_recursive(self.root_widget.as_ref(), &mut scene, transform, &mut self.text_renderer, &self.theme, 0);
                            }
                            
                            // 5. Draw Remote Cursors
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
                        WindowEvent::ModifiersChanged(modifiers) => {
                            self.modifiers = modifiers;
                        }
                        _ => {
                            // Forward events to widget
                            let mut handled = false;
                            let mut focus_target = None;
                            
                            // Toggle Debug Mode with F12
                            if let winit::event::WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                self.debug_mode = !self.debug_mode;
                                println!("Debug mode: {}", self.debug_mode);
                                handled = true;
                            }

                            // Handle Tab navigation
                            if !handled {
                                if let winit::event::WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let shift = self.modifiers.state().shift_key();
                                    
                                    if !self.focusable_order.is_empty() {
                                        let current_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
                                        let next_idx = match current_idx {
                                            Some(idx) => {
                                                if shift {
                                                    if idx == 0 { self.focusable_order.len() - 1 } else { idx - 1 }
                                                } else {
                                                    (idx + 1) % self.focusable_order.len()
                                                }
                                            }
                                            None => 0,
                                        };
                                        self.set_focused_node(Some(self.focusable_order[next_idx]));
                                        handled = true;
                                    }
                                }
                            }

                            // Handle Arrow navigation
                            if !handled {
                                if let winit::event::WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(logical_key), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let direction = match logical_key {
                                        winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                                        winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                                        winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                                        winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                                        _ => None,
                                    };

                                    if let Some(dir) = direction {
                                        let alt = self.modifiers.state().alt_key();
                                        // Always navigate on Alt+Arrow, or if nothing handled it below
                                        if alt {
                                            self.navigate_focus(dir);
                                            handled = true;
                                        }
                                    }
                                }
                            }

                            if !handled {
                                let scale_factor = window.scale_factor();
                                let root_node = self.root_node.unwrap();
                                self.root_widget.handle_event(&event, scale_factor, &mut self.state.doc, &self.state.root, &mut handled, &mut focus_target, root_node, &self.taffy);
                            }

                            // Post-widget navigation check (if widget didn't use arrow key)
                            if !handled {
                                if let winit::event::WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(logical_key), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let direction = match logical_key {
                                        winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                                        winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                                        winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                                        winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                                        _ => None,
                                    };
                                    if let Some(dir) = direction {
                                        self.navigate_focus(dir);
                                        handled = true;
                                    }
                                }
                            }

                            // Update Focus if requested (e.g. by mouse click)
                            if let Some(new_focus_id) = focus_target {
                                self.set_focused_node(Some(new_focus_id));
                                handled = true;
                            }

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
