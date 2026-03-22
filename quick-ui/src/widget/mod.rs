use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId};
use crate::text_render::TextRenderer;
use crate::theme::Theme;

use std::collections::HashMap;

pub struct LayoutConstraints {
    pub max_width: f32,
    pub max_height: f32,
}

pub struct WidgetContext<'a> {
    pub doc: &'a AutoCommit,
    pub path: &'a ObjId,
    pub theme: &'a Theme,
}

/// The core Widget trait for Retained Mode CRDT UI.
pub trait Widget {
    /// Sync visual state from the CRDT document
    fn sync(&mut self, state: &AutoCommit, path: &ObjId);

    /// Pass 1: Build the Taffy layout tree
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId;

    /// Optional: Measure widget size based on constraints
    fn measure(&mut self, _known_dimensions: taffy::Size<Option<f32>>, _available_space: taffy::Size<taffy::AvailableSpace>, _text_renderer: &mut TextRenderer) -> taffy::Size<f32> {

        taffy::Size::ZERO
    }

    /// Pass 2: Save the calculated layout rect

    fn apply_layout(&mut self, taffy: &TaffyTree, node: NodeId, origin: vello::kurbo::Point);
    
    /// Draw the widget using Vello
    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme);
    
    /// Handle winit events, mutating the CRDT document directly if necessary.
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>);

    /// Get children widgets (for layout/event propagation)
    fn children(&self) -> Vec<&dyn Widget> { vec![] }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> { vec![] }

    /// Get the current layout rect
    fn get_rect(&self) -> vello::kurbo::Rect { vello::kurbo::Rect::ZERO }

    /// Get widget name for debugging
    fn name(&self) -> &str { "Widget" }

    /// Focus management
    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }
}

pub type Element = Box<dyn Widget>;

mod button;
pub use button::Button;

mod column;
pub use column::Column;

mod toggle;
pub use toggle::Toggle;

mod checkbox;
pub use checkbox::Checkbox;

mod row;
pub use row::Row;

mod stack;
pub use stack::Stack;

mod container;
pub use container::Container;

mod text;
pub use text::Text;

mod spacer;
pub use spacer::Spacer;

mod slider;
pub use slider::Slider;

mod text_input;
pub use text_input::TextInput;
