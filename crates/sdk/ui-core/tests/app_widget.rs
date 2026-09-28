#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::string_slice,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]

use tilo_ui::presence::PresenceCtx;
use tilo_ui::{AppKind, AppWidget, IconDescriptor, Theme, Widget};

struct DummyApp;

impl Widget for DummyApp {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }
    fn apply_layout(
        &mut self,
        _taffy: &taffy::TaffyTree,
        _node: taffy::NodeId,
        _origin: vello::kurbo::Point,
    ) {
    }
    fn draw(
        &mut self,
        _scene: &mut vello::Scene,
        _transform: vello::kurbo::Affine,
        _text_renderer: &mut tilo_ui::text_render::TextRenderer,
        _theme: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn name(&self) -> &'static str {
        "DummyApp"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        vello::kurbo::Rect::ZERO
    }
}

impl AppWidget for DummyApp {
    fn app_kind(&self) -> AppKind {
        AppKind::Terminal
    }
    fn app_title(&self) -> &'static str {
        "Dummy"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("X")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        Some(AppKind::Terminal)
    }
}

#[test]
fn app_widget_defaults_yield_empty_commands() {
    let app = DummyApp;
    assert_eq!(app.app_title(), "Dummy");
    assert_eq!(app.app_kind(), AppKind::Terminal);
    assert!(matches!(app.app_icon(), IconDescriptor::Glyph("X")));
    assert_eq!(app.permission_hint(), Some(AppKind::Terminal));
    assert!(app.commands().is_empty());
}
