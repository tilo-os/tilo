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
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, _theme: &Theme, node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>) -> NodeId {
        let node = taffy.new_leaf(Style {
            size: Size {
                width: taffy::Dimension::auto(),
                height: taffy::Dimension::auto(),
            },
            // flex_shrink: 1.0 is CRITICAL for wrapping. 
            // It allows Taffy to propose a smaller width than natural_w.
            flex_shrink: 1.0,
            flex_grow: 0.0,
            ..Default::default()
        }).unwrap();
        node_map.insert(node, self as *mut dyn Widget);
        node
    }

    fn measure(&mut self, known_dimensions: taffy::Size<Option<f32>>, available_space: taffy::Size<taffy::AvailableSpace>, text_renderer: &mut TextRenderer) -> taffy::Size<f32> {
        let label = crate::i18n!(&self.label);
        
        // 1. Determine the maximum width we are allowed to have
        let max_w = if let Some(w) = known_dimensions.width {
            Some(w)
        } else {
            match available_space.width {
                taffy::AvailableSpace::Definite(w) => Some(w),
                taffy::AvailableSpace::MinContent => Some(0.0), // Force wrap to min-content
                taffy::AvailableSpace::MaxContent => None,      // Allow infinite width
            }
        };

        // 2. Measure with that constraint
        let (w, h) = text_renderer.measure(&label, self.size, max_w);
        
        // 3. Return the measured size. 
        // We add a tiny 0.1 epsilon to width to prevent rounding errors in Row/Column
        // from thinking the text is "just a bit too big" and wrapping it unnecessarily.
        taffy::Size { width: w + 0.1, height: h }
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

    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, _presence: &crate::presence::PresenceCtx) {
        let label = crate::i18n!(&self.label);
        // Use the actual width from layout. 
        // We add 1.0px of "slop" to the draw call to ensure cosmic-text doesn't 
        // wrap a word that technically fits but is off by a sub-pixel.
        let max_width = self.rect.width() as f32;
        
        text_renderer.draw_text(
            scene,
            &label,
            self.size,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((self.rect.x0, self.rect.y0)),
            Some(max_width + 1.0),
        );
    }

    fn handle_event(&mut self, _event: &WindowEvent, _scale_factor: f64, _modifiers: &winit::event::Modifiers, _state: &mut AutoCommit, _path: &ObjId, _handled: &mut bool, _focus_target: &mut Option<NodeId>, _node_id: NodeId, _taffy: &TaffyTree<()>) {}

    fn name(&self) -> &str { "Text" }

    fn get_rect(&self) -> vello::kurbo::Rect {
        self.rect
    }

    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { false }
}
