use automerge::{AutoCommit, ObjId};
use vello::Scene;
use vello::kurbo::Affine;
use winit::event::WindowEvent;
use taffy::{TaffyTree, NodeId};
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::presence::{PresenceCtx, PresenceKey};
use crate::permissions::PermissionScope;
use crate::dnd::DragPayload;

use std::collections::HashMap;

pub trait Widget {
    fn sync(&mut self, state: &mut AutoCommit, path: &ObjId);
    fn update(&mut self, _state: &mut AutoCommit, _path: &ObjId) -> bool { false }
    fn build_layout(&mut self, taffy: &mut TaffyTree<()>, theme: &Theme, node_map: &mut HashMap<NodeId, *mut dyn Widget>) -> NodeId;
    fn measure(&mut self, _known: taffy::Size<Option<f32>>, _available: taffy::Size<taffy::AvailableSpace>, _text_renderer: &mut TextRenderer) -> taffy::Size<f32> { taffy::Size::ZERO }
    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: vello::kurbo::Point);
    fn draw(&mut self, scene: &mut Scene, transform: Affine, text_renderer: &mut TextRenderer, theme: &Theme, presence: &PresenceCtx);
    fn handle_event(&mut self, event: &WindowEvent, scale_factor: f64, modifiers: &winit::event::Modifiers, state: &mut AutoCommit, path: &ObjId, handled: &mut bool, focus_target: &mut Option<NodeId>, node_id: NodeId, taffy: &TaffyTree<()>);
    fn children(&self) -> Vec<&dyn Widget> { vec![] }
    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> { vec![] }
    fn name(&self) -> &str;
    fn get_rect(&self) -> vello::kurbo::Rect;
    fn is_focusable(&self) -> bool { false }
    fn set_focus(&mut self, _focused: bool) {}
    fn is_focused(&self) -> bool { false }
    fn needs_commit(&self) -> bool { false }
    fn on_commit(&mut self) {}
    fn get_state(&self) -> Option<String> { None }
    fn set_state(&mut self, _state: &str) {}

    // --- Collab / DnD contract extensions ---
    fn presence_key(&self) -> Option<PresenceKey> { None }
    fn permission_scope(&self) -> Option<PermissionScope> { None }
    fn accepts_drop(&self, _payload: &DragPayload) -> bool { false }
    fn on_drop(&mut self, _payload: DragPayload, _state: &mut AutoCommit, _path: &ObjId) {}
    fn wants_pointer_capture(&self) -> bool { false }
}

pub type Element = Box<dyn Widget>;

mod button;      pub use button::Button;
mod checkbox;    pub use checkbox::Checkbox;
mod column;      pub use column::Column;
mod row;         pub use row::Row;
mod toggle;      pub use toggle::Toggle;
mod stack;       pub use stack::Stack;
mod container;   pub use container::Container;
mod text;        pub use text::Text;
mod spacer;      pub use spacer::Spacer;
mod slider;      pub use slider::Slider;
mod text_input;  pub use text_input::TextInput;
mod tabs;        pub use tabs::{TabBar, TabDescriptor};
mod scrollable;  pub use scrollable::{Scrollable, ScrollAxis};
mod tile_host;   pub use tile_host::{TileHost, TileContent, AppKind};
mod app_widget;  pub use app_widget::{AppCommand, AppWidget, IconDescriptor};
pub mod command_palette;
mod window_group;
pub use window_group::{WindowGroup, WindowWidth, WIDTH_MIN_PX, WIDTH_MAX_PX, RESIZE_HANDLE_PX};
mod strip_view;
pub use strip_view::{StripView, STRIP_COOLDOWN_MS};
mod workspace_view;
pub use workspace_view::{WorkspaceView, WorkspaceSwitchDirection};
