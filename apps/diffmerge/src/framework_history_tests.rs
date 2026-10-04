use ui::widget::{AppModel, ModelPath};
include!("../../../crates/sdk/ui-core/tests/support/history_adapter_conformance.rs");
fn malformed_fixture(doc: &mut AutoCommit) {
    doc.put(ROOT, "merged", 7).unwrap();
}
fn fixture() -> (AutoCommit, automerge::ChangeHash, String) {
    let mut doc = AutoCommit::new();
    crate::doc::MergeDoc::set_merged(&mut doc, "Selected output\nlet value = 42;\n");
    crate::doc::MergeDoc::set_resolution(&mut doc, 0, "ours");
    crate::doc::MergeDoc::set_session(&mut doc, "recorded session");
    let selected = doc.get_heads()[0];
    crate::doc::MergeDoc::set_merged(&mut doc, "Live output\n");
    crate::doc::MergeDoc::set_resolution(&mut doc, 0, "theirs");
    doc.put(ROOT, "view_only", "current").unwrap();
    doc.get_heads();
    (doc, selected, "Selected output".into())
}
fn verify_restored(doc: &AutoCommit) {
    assert_eq!(
        crate::doc::MergeDoc::merged_text(doc),
        "Selected output\nlet value = 42;\n"
    );
    assert_eq!(
        crate::doc::MergeDoc::resolutions(doc),
        vec![(0, "ours".into())]
    );
    assert_eq!(
        crate::doc::MergeDoc::session(doc).as_deref(),
        Some("recorded session")
    );
}
/// History adapter restore: production startup preserves a saved merge, and
/// restoring fences a stale resource request while successive admitted hunks
/// stay usable. Fresh fixture per phase.
#[test]
fn history_restore_preserves_saved_merge_and_fences_stale_requests() {
    production_startup_preserves_saved_merge();
    successive_admitted_resource_hunks_remain_usable_then_restore_fences_stale_request();
}

fn production_startup_preserves_saved_merge() {
    use ui::widget::Widget;
    let mut doc = automerge::AutoCommit::new();
    crate::doc::MergeDoc::set_merged(&mut doc, "saved authored output\n");
    crate::doc::MergeDoc::set_resolution(&mut doc, 0, "ours");
    crate::doc::MergeDoc::set_session(&mut doc, "saved session");
    let heads = doc.get_heads();
    crate::app::DiffMergeApp::new().sync(&mut doc, &automerge::ROOT);
    assert_eq!(
        crate::doc::MergeDoc::merged_text(&doc),
        "saved authored output\n"
    );
    assert_eq!(
        crate::doc::MergeDoc::resolutions(&doc),
        vec![(0, "ours".into())]
    );
    assert_eq!(doc.get_heads(), heads);
}

fn test_root() -> Box<dyn ui::widget::Widget> {
    Box::new(crate::DiffMergeApp::new())
}

// Routes actual resource descriptors through Driver's admitted document-action
// door while retaining the real DiffMerge owner as the callback destination.
struct ResourceHarness {
    app: crate::DiffMergeApp,
    queue: std::rc::Rc<std::cell::RefCell<Vec<tilo_actions::ControlInvocation>>>,
}
impl ui::widget::Widget for ResourceHarness {
    ui::provides!(
        dyn ui::history_mode::capability::DocumentHistory,
        dyn ui::widget::controls::ActionControls,
        dyn ui::resources::PublishesResources
    );

    fn sync(&mut self, d: &mut AppModel, p: &ModelPath) {
        self.app.sync(d, p);
    }
    fn build_layout(
        &mut self,
        t: &mut taffy::TaffyTree<()>,
        th: &ui::Theme,
        m: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        self.app.build_layout(t, th, m)
    }
    fn apply_layout(&mut self, t: &taffy::TaffyTree<()>, n: taffy::NodeId, p: vello::kurbo::Point) {
        self.app.apply_layout(t, n, p);
    }
    fn draw(
        &mut self,
        s: &mut vello::Scene,
        t: vello::kurbo::Affine,
        tr: &mut ui::TextRenderer,
        th: &ui::Theme,
        p: &ui::presence::PresenceCtx,
    ) {
        self.app.draw(s, t, tr, th, p);
    }
    #[allow(
        clippy::many_single_char_names,
        reason = "test adapter forwards the existing Widget event signature"
    )]
    fn handle_event(&mut self, e: &winit::event::WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        self.app.handle_event(e, ctx);
    }
    fn name(&self) -> &'static str {
        "DiffMerge resource test"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.app.get_rect()
    }
}

impl ui::resources::PublishesResources for ResourceHarness {
    fn resources(&self, d: &AutoCommit) -> Vec<tilo_actions::Resource> {
        ui::resources::resources(&self.app, d)
    }
}

impl ui::widget::controls::ActionControls for ResourceHarness {
    fn take_control_invocation(&mut self) -> Option<tilo_actions::ControlInvocation> {
        self.queue.borrow_mut().pop()
    }
}

impl ui::history_mode::capability::DocumentHistory for ResourceHarness {
    fn document_action_applied(
        &mut self,
        a: &tilo_actions::EncodedAction,
        b: &[automerge::ChangeHash],
        d: &AutoCommit,
        p: &automerge::ObjId,
    ) {
        self.app.document_action_applied(a, b, d, p);
    }
}
fn successive_admitted_resource_hunks_remain_usable_then_restore_fences_stale_request() {
    let queue = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
    let root = ResourceHarness {
        app: crate::DiffMergeApp::new_demo(),
        queue: queue.clone(),
    };
    let mut driver = ui::headless::Driver::new(ui::State::new(), Box::new(root), (1000, 750));
    crate::actions::register(driver.action_registry_mut());
    for choice in ["take-theirs", "take-ours"] {
        let resources = driver.resources();
        let hunk = resources
            .iter()
            .flat_map(|r| &r.children)
            .find(|r| r.actions.iter().any(|a| a.id == choice))
            .expect("current hunk remains available");
        queue.borrow_mut().push(tilo_actions::ControlInvocation {
            address: tilo_actions::ActionAddress::new(hunk.id.as_str(), choice),
            arguments: serde_json::Map::default(),
        });
        driver.key("Escape");
        assert!(
            crate::doc::MergeDoc::resolutions(&driver.state_mut().doc)
                .iter()
                .any(|(_, c)| c == choice.strip_prefix("take-").unwrap())
        );
    }
    let resources = driver.resources();
    let hunk = resources
        .iter()
        .flat_map(|r| &r.children)
        .find(|r| r.actions.iter().any(|a| a.id == "take-theirs"))
        .unwrap();
    let stale = tilo_actions::ActionAddress::new(hunk.id.as_str(), "take-theirs");
    crate::doc::MergeDoc::set_merged(
        &mut driver.state_mut().doc,
        "Historical output independent of current hunks\n",
    );
    let heads = driver.state_mut().doc.get_heads();
    queue.borrow_mut().push(tilo_actions::ControlInvocation {
        address: stale,
        arguments: serde_json::Map::default(),
    });
    driver.key("Escape");
    assert_eq!(driver.state_mut().doc.base_heads(), heads);
    assert_eq!(
        crate::doc::MergeDoc::merged_text(&driver.state_mut().doc),
        "Historical output independent of current hunks\n"
    );
}
