use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::{Affine, Point};
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId, Style, Size};
use crate::widget::Widget;
use crate::text_render::TextRenderer;
use crate::theme::Theme;

pub struct Text {
    label: String,
    size: f32,
    rect: vello::kurbo::Rect,
}

impl Text {
    pub fn new(label: &str) -> Self {
        Self {
            label: label.to_string(),
            size: 16.0,
            rect: vello::kurbo::Rect::ZERO,
        }
    }

    pub fn with_size(mut self, size: f32) -> Self {
        self.size = size;
        self
    }
}

impl Widget for Text {
    fn sync(&mut self, _state: &AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::auto(),
            },
            flex_shrink: 0.0, // Prevent wrapping unless window is truly too small
            flex_grow: 0.0,
            ..Default::default()
        }).unwrap();
        // In Taffy 0.9.2, to make a node measurable via compute_layout_with_measure,
        // it seems it just needs to be a leaf node. 
        // Let's ensure node_map is correct and use a different approach if it fails.
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(&mut self, _known_dimensions: taffy::Size<Option<f32>>, available_space: taffy::Size<taffy::AvailableSpace>, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        let label = crate::i18n!(&self.label);
        
        // 1. Get natural size (no wrapping)
        let (natural_w, natural_h) = text_renderer.measure(&label, self.size, None);
        
        // 2. Check if we are forced to wrap
        let max_available_w = match available_space.width {
            taffy::AvailableSpace::Definite(w) => Some(w),
            _ => None,
        };

        if let Some(max_w) = max_available_w {
            if max_w < natural_w {
                // Wrap to available space
                let (w, h) = text_renderer.measure(&label, self.size, Some(max_w));
                return taffy::Size { width: w, height: h };
            }
        }
        
        taffy::Size { width: natural_w, height: natural_h }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).unwrap();
        self.rect = vello::kurbo::Rect::new(
            origin.x + layout.location.x as f64,
            origin.y + layout.location.y as f64,
            origin.x + layout.location.x as f64 + layout.size.width as f64,
            origin.y + layout.location.y as f64 + layout.size.height as f64,
        );
    }

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme) {
        let label = crate::i18n!(&self.label);
        let max_width = self.rect.width() as f32;
        
        text_renderer.draw_text(
            scene,
            &label,
            self.size,
            theme.text_primary,
            transform * Affine::translate((self.rect.x0, self.rect.y0)),
            Some(max_width),
        );
    }

    fn handle_event(&mut self, _event: &WindowEvent, _scale_factor: f64, _state: &mut AutoCommit, _path: &ObjId, _handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {}

    fn name(&self) -> &str { "Text" }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }
}
