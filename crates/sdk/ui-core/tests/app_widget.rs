use tilo_ui::widget::TaffyTreeExt;
use tilo_ui::{AppKind, AppWidget, IconDescriptor, Theme, Widget};

struct DummyApp;

impl Widget for DummyApp {
    fn build_layout(
        &mut self,
        taffy: &mut taffy::TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut std::collections::HashMap<taffy::NodeId, *mut dyn Widget>,
    ) -> taffy::NodeId {
        taffy.widget_leaf(taffy::Style::default(), node_map, self)
    }
    fn name(&self) -> &'static str {
        "DummyApp"
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
