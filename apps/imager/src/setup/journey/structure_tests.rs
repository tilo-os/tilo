//! Retained layout maps must be retired when a nested surface replaces controls.
use super::*;

struct ReplacingChild {
    child: Box<dyn Widget>,
    replace: Rc<Cell<bool>>,
    replaced: bool,
}

impl Widget for ReplacingChild {
    fn name(&self) -> &'static str {
        "ReplacingChild"
    }

    fn sync(&mut self, _state: &AppModel, _path: &ModelPath) {
        if self.replace.replace(false) {
            self.child = Box::new(ui::widget::Text::new("Replacement"));
            self.replaced = true;
        }
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        nodes: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.child.build_layout(taffy, theme, nodes)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![self.child.as_ref()]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![self.child.as_mut()]
    }

    fn take_structure_dirty(&mut self) -> bool {
        std::mem::take(&mut self.replaced)
    }
}

#[test]
fn nested_replacement_retires_installer_layout_even_without_a_page_change() {
    let replace = Rc::new(Cell::new(false));
    let mut preview = InstallerPreview {
        composed: Some(Box::new(ReplacingChild {
            child: Box::new(ui::widget::Text::new("Original")),
            replace: replace.clone(),
            replaced: false,
        })),
        dirty: false,
        ..InstallerPreview::default()
    };
    preview.direct.active = true;
    let theme = Theme::default();
    preview.layout_metrics = Some((
        theme.tokens.metrics.readable_width,
        theme.tokens.metrics.content_min_width,
        theme.text(TextRole::LargeTitle).size,
    ));
    let mut taffy = TaffyTree::new();
    let mut nodes = HashMap::new();
    preview.build_layout(&mut taffy, &theme, &mut nodes);
    assert!(
        !nodes.is_empty(),
        "there is a retained layout to invalidate"
    );
    assert!(!preview.take_structure_dirty());
    let state = ui::state::State::new();
    replace.set(true);
    preview.sync(&state.doc, &state.root);
    assert!(
        preview.take_structure_dirty(),
        "the host must discard pointers to the child freed by sync"
    );
    assert!(
        !preview.take_structure_dirty(),
        "the child report is drained"
    );
}

#[test]
fn page_rebuild_also_drains_nested_replacement_reports() {
    let mut preview = InstallerPreview {
        composed: Some(Box::new(ReplacingChild {
            child: Box::new(ui::widget::Text::new("Replacement")),
            replace: Rc::default(),
            replaced: true,
        })),
        ..InstallerPreview::default()
    };
    assert!(preview.take_structure_dirty());
    assert!(
        !preview.composed.as_mut().unwrap().take_structure_dirty(),
        "a page rebuild must drain the child's report too"
    );
    preview.dirty = false;
    assert!(!preview.take_structure_dirty());
}

#[test]
fn native_handoff_report_is_independent_of_page_composition() {
    use ui::framework::testing::TestApp;
    let mut app =
        TestApp::<crate::resource::GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview
            .show_desktop(crate::setup::view::SetupApp::for_full_os_check(
                ui::LocalState::new(None),
            ))
            .unwrap();
        assert!(!preview.dirty, "the native surface owns its composition");
        assert!(preview.take_structure_dirty(), "the old page was replaced");
    })
    .unwrap();
}
