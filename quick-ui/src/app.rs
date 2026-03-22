use winit::{
    event::{Event, WindowEvent},
    event_loop::EventLoop,
    window::Window,
};
use std::sync::Arc;
use vello::{Scene, Renderer, RendererOptions, util::RenderContext, peniko::Color, kurbo::Affine, kurbo::Rect as KurboRect};
use taffy::{TaffyTree, AvailableSpace, NodeId};
use crate::state::State;
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

#[derive(Debug, Clone, Copy)]
enum Direction { Up, Down, Left, Right }

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
    
    // Stable state for Undo/Redo tracking
    last_stable_focus_idx: Option<usize>,
    last_stable_metadata: Option<String>,
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
            last_stable_focus_idx: None,
            last_stable_metadata: None,
        }
    }

    fn update_focusable_order(&mut self) {
        self.focusable_order.clear();
        fn collect_focusable(node_id: NodeId, widget: &dyn Widget, taffy: &TaffyTree<()>, order: &mut Vec<NodeId>) {
            if widget.is_focusable() { order.push(node_id); }
            let children = widget.children();
            if let Ok(child_nodes) = taffy.children(node_id) {
                for (child_widget, &child_node) in children.iter().zip(child_nodes.iter()) {
                    collect_focusable(child_node, *child_widget, taffy, order);
                }
            }
        }
        if let Some(root) = self.root_node {
            collect_focusable(root, self.root_widget.as_ref(), &self.taffy, &mut self.focusable_order);
        }
    }

    fn apply_undo_entry(&mut self, entry: crate::state::UndoEntry) {
        if let Some(idx) = entry.focused_node_idx {
            if idx < self.focusable_order.len() {
                let id = self.focusable_order[idx];
                self.set_focused_node(Some(id));
                if let Some(metadata) = entry.widget_metadata {
                    if let Some(&w_ptr) = self.node_map.get(&id) {
                        unsafe { (*w_ptr).set_state(&metadata); }
                    }
                    self.last_stable_metadata = Some(metadata);
                }
                self.last_stable_focus_idx = Some(idx);
            }
        } else { 
            self.set_focused_node(None); 
            self.last_stable_focus_idx = None;
            self.last_stable_metadata = None;
        }
        self.layout_dirty = true;
        self.needs_sync = true;
    }

    fn commit_action(&mut self) {
        // We push the focus/metadata that were active BEFORE the current changes were committed.
        self.state.commit_action(self.last_stable_focus_idx, self.last_stable_metadata.clone());
        
        // Now update stable state to current
        self.last_stable_focus_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map.get(&id).and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });
    }

    fn set_focused_node(&mut self, new_id: Option<NodeId>) {
        if self.focused_node != new_id {
            if let Some(old_id) = self.focused_node {
                if let Some(&w_ptr) = self.node_map.get(&old_id) {
                    unsafe { (*w_ptr).set_focus(false); }
                }
            }
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

    fn navigate_focus(&mut self, direction: Direction) {
        let current_id = match self.focused_node {
            Some(id) => id,
            None => { if !self.focusable_order.is_empty() { self.set_focused_node(Some(self.focusable_order[0])); } return; }
        };
        let current_rect = if let Some(&w_ptr) = self.node_map.get(&current_id) { unsafe { (*w_ptr).get_rect() } } else { return; };
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
                    let score = primary_dist + (secondary_dist * 2.0);
                    if score < best_score { best_score = score; best_candidate = Some(candidate_id); }
                }
            }
        }
        if let Some(new_id) = best_candidate { self.set_focused_node(Some(new_id)); }
    }

    pub fn run(mut self) {
        let event_loop = EventLoop::new().unwrap();
        let window_attributes = Window::default_attributes()
            .with_title("QuickUI App")
            .with_inner_size(winit::dpi::LogicalSize::new(800.0, 600.0));
        let window = Arc::new(event_loop.create_window(window_attributes).unwrap());
        let mut render_cx = RenderContext::new();
        let mut surface = pollster::block_on(render_cx.create_surface(window.clone(), window.inner_size().width, window.inner_size().height, vello::wgpu::PresentMode::AutoVsync)).unwrap();
        let device_handle = &render_cx.devices[surface.dev_id];
        surface.config.format = surface.config.format.add_srgb_suffix();
        surface.surface.configure(&device_handle.device, &surface.config);
        let mut renderer = Renderer::new(&device_handle.device, RendererOptions { surface_format: Some(surface.config.format), use_cpu: false, antialiasing_support: vello::AaSupport::all(), num_init_threads: None }).unwrap();
        let mut scene = Scene::new();

        self.node_map.clear();
        self.root_node = Some(self.root_widget.build_layout(&mut self.taffy, &self.theme, &mut self.node_map));
        self.update_focusable_order();
        
        // Capture initial stable state
        self.last_stable_focus_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
        self.last_stable_metadata = self.focused_node.and_then(|id| {
            self.node_map.get(&id).and_then(|&w_ptr| unsafe { (*w_ptr).get_state() })
        });

        let themes = [Theme::dark(), Theme::dracula(), Theme::nord(), Theme::gruvbox(), Theme::solarized(), Theme::light()];
        let mut _theme_idx = 0;

        event_loop.run(move |event, elwt| {
            match event {
                Event::WindowEvent { event, window_id } if window_id == window.id() => {
                    match event {
                        WindowEvent::CloseRequested => elwt.exit(),
                        WindowEvent::Resized(size) => { render_cx.resize_surface(&mut surface, size.width, size.height); self.layout_dirty = true; window.request_redraw(); }
                        WindowEvent::RedrawRequested => {
                            let width = surface.config.width;
                            let height = surface.config.height;
                            scene.reset();
                            let scale_factor = window.scale_factor() as f32;

                            if self.needs_sync { self.root_widget.sync(&self.state.doc, &self.state.root); self.needs_sync = false; }
                            
                            let root_node = self.root_node.unwrap();
                            if self.layout_dirty {
                                let node_map = &self.node_map;
                                let text_renderer = &mut self.text_renderer;
                                self.taffy.compute_layout_with_measure(root_node, taffy::Size { width: AvailableSpace::Definite((width as f32) / scale_factor), height: AvailableSpace::Definite((height as f32) / scale_factor) },
                                    |known, available, node_id, _, _| {
                                        if let Some(&widget_ptr) = node_map.get(&node_id) { unsafe { (*widget_ptr).measure(known, available, text_renderer) } }
                                        else { taffy::Size::ZERO }
                                    }).unwrap();
                                self.root_widget.apply_layout(&self.taffy, root_node, vello::kurbo::Point::new(0.0, 0.0));
                                self.layout_dirty = false;
                                self.update_focusable_order();
                            }
                            
                            let transform = Affine::scale(scale_factor as f64);
                            self.root_widget.draw(&mut scene, transform, &mut self.text_renderer, &self.theme);
                            
                            if self.debug_mode {
                                fn draw_debug_recursive(widget: &dyn Widget, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, depth: usize) {
                                    let rect = widget.get_rect();
                                    let color = match depth % 3 { 0 => Color::rgb8(255, 0, 255), 1 => Color::rgb8(0, 255, 255), _ => Color::rgb8(255, 255, 0) };
                                    scene.stroke(&vello::kurbo::Stroke::new(1.5), transform, color, None, &rect);
                                    let label = format!("{}: {}", widget.name(), depth);
                                    let text_y = (rect.y0 + 12.0).min(rect.y1);
                                    let bg_rect = KurboRect::new(rect.x0, rect.y0, rect.x0 + 100.0, rect.y0 + 14.0);
                                    scene.fill(vello::peniko::Fill::NonZero, transform, Color::rgba8(0,0,0,180), None, &bg_rect);
                                    text_renderer.draw_text(scene, &label, 10.0, color, transform * Affine::translate((rect.x0 + 2.0, text_y)), None);
                                    for child in widget.children() { draw_debug_recursive(child, scene, transform, text_renderer, depth + 1); }
                                }
                                draw_debug_recursive(self.root_widget.as_ref(), &mut scene, transform, &mut self.text_renderer, 0);
                            }

                            let surface_texture = surface.surface.get_current_texture().expect("failed to get surface texture");
                            let device_handle = &render_cx.devices[surface.dev_id];
                            renderer.render_to_surface(&device_handle.device, &device_handle.queue, &scene, &surface_texture, &vello::RenderParams { base_color: self.theme.bg_app, width, height, antialiasing_method: vello::AaConfig::Area }).unwrap();
                            surface_texture.present();
                            device_handle.device.poll(vello::wgpu::Maintain::Poll);
                        }
                        WindowEvent::ModifiersChanged(modifiers) => { self.modifiers = modifiers; }
                        _ => {
                            let mut handled = false;
                            let mut focus_target = None;
                            let mut is_undo_redo = false;

                            if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::F12), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                self.debug_mode = !self.debug_mode; handled = true;
                            }

                            if !handled && self.modifiers.state().control_key() {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Character(ref c), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    match c.as_str() {
                                        "z" | "Z" => { if let Some(e) = self.state.undo() { self.apply_undo_entry(e); } is_undo_redo = true; handled = true; }
                                        "y" | "Y" => { if let Some(e) = self.state.redo() { self.apply_undo_entry(e); } is_undo_redo = true; handled = true; }
                                        _ => {}
                                    }
                                }
                            }

                            if !handled {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(winit::keyboard::NamedKey::Tab), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let shift = self.modifiers.state().shift_key();
                                    if !self.focusable_order.is_empty() {
                                        let current_idx = self.focused_node.and_then(|id| self.focusable_order.iter().position(|&x| x == id));
                                        let next_idx = match current_idx {
                                            Some(idx) => if shift { if idx == 0 { self.focusable_order.len() - 1 } else { idx - 1 } } else { (idx + 1) % self.focusable_order.len() },
                                            None => 0,
                                        };
                                        self.set_focused_node(Some(self.focusable_order[next_idx])); handled = true;
                                    }
                                }
                            }

                            if !handled {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(logical_key), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let dir = match logical_key {
                                        winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up),
                                        winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                                        winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left),
                                        winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                                        _ => None,
                                    };
                                    if let (Some(d), true) = (dir, self.modifiers.state().alt_key()) { self.navigate_focus(d); handled = true; }
                                }
                            }

                            if !handled {
                                let scale_factor = window.scale_factor();
                                let root_node = self.root_node.unwrap();
                                self.root_widget.handle_event(&event, scale_factor, &mut self.state.doc, &self.state.root, &mut handled, &mut focus_target, root_node, &self.taffy);
                            }

                            if !handled {
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Named(logical_key), state: winit::event::ElementState::Pressed, .. }, .. } = event {
                                    let dir = match logical_key {
                                        winit::keyboard::NamedKey::ArrowUp => Some(Direction::Up), winit::keyboard::NamedKey::ArrowDown => Some(Direction::Down),
                                        winit::keyboard::NamedKey::ArrowLeft => Some(Direction::Left), winit::keyboard::NamedKey::ArrowRight => Some(Direction::Right),
                                        _ => None,
                                    };
                                    if let Some(d) = dir { self.navigate_focus(d); handled = true; }
                                }
                            }

                            if let Some(id) = focus_target { self.set_focused_node(Some(id)); handled = true; }

                            if handled {
                                let mut should_commit = !is_undo_redo;
                                if let WindowEvent::KeyboardInput { event: winit::event::KeyEvent { logical_key: winit::keyboard::Key::Character(_), .. }, .. } = event { should_commit = false; }
                                if should_commit { self.commit_action(); }
                                self.needs_sync = true; self.layout_dirty = true; window.request_redraw();
                            }
                        }
                    }
                }
                Event::AboutToWait => {
                    if self.root_widget.needs_commit() {
                        self.commit_action();
                        self.root_widget.on_commit();
                        self.needs_sync = true;
                        window.request_redraw();
                    }
                }
                _ => {}
            }
        }).unwrap();
    }
}
