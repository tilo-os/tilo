//! Asserts `TileContent::App` holds an `AppWidget`, not a bare `Widget`.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use automerge::{AutoCommit, ObjId};
use ui::presence::PresenceCtx;
use ui::widget::{TileContent, TileHost};
use ui::{AppKind, AppWidget, IconDescriptor, Theme, Widget};

struct DummyApp;

impl Widget for DummyApp {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        let n = taffy.new_leaf(taffy::Style::default()).unwrap();
        node_map.insert(n, std::ptr::from_mut::<dyn Widget>(self));
        n
    }
    fn apply_layout(&mut self, _t: &taffy::TaffyTree, _n: taffy::NodeId, _o: vello::kurbo::Point) {}
    fn draw(
        &mut self,
        _s: &mut vello::Scene,
        _t: vello::kurbo::Affine,
        _tr: &mut ui::text_render::TextRenderer,
        _th: &Theme,
        _presence: &PresenceCtx,
    ) {
    }
    fn handle_event(
        &mut self,
        _event: &winit::event::WindowEvent,
        _scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        _handled: &mut bool,
        _focus_target: &mut Option<taffy::NodeId>,
        _node_id: taffy::NodeId,
        _taffy: &taffy::TaffyTree<()>,
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
        "test"
    }
    fn app_icon(&self) -> IconDescriptor {
        IconDescriptor::Glyph("T")
    }
    fn permission_hint(&self) -> Option<AppKind> {
        None
    }
}

#[test]
fn tile_content_app_holds_app_widget() {
    let tc = TileContent::App(Box::new(DummyApp));
    match tc {
        TileContent::App(a) => {
            assert_eq!(a.app_title(), "test");
            assert_eq!(a.app_kind(), AppKind::Terminal);
        }
        _ => panic!("expected App variant"),
    }
    // Ensure TileHost accepts an AppWidget-bearing TileContent.
    let host = TileHost::new(TileContent::App(Box::new(DummyApp)));
    assert_eq!(host.kind_string(), "App");
}
