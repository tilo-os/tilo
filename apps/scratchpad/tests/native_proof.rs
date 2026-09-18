#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

use automerge::{ROOT, ReadDoc};
use ui::headless::Driver;

#[test]
fn native_fixture_reloads_production_storage_and_refuses_overwrite() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("note");
    let manifest = scratchpad::seed_native_fixture(&path).unwrap();
    let (state, root) = scratchpad::open(&path).unwrap();
    let object = state.doc.get(ROOT, "body").unwrap().unwrap().1;
    let body = state.doc.text(object).unwrap();
    assert_eq!(body.lines().count(), 1000);
    assert_eq!(manifest.body_bytes, body.len());
    assert_eq!(
        manifest.body_hash,
        iroh_blobs::Hash::new(body.as_bytes()).to_hex()
    );
    let snapshot = root.native_input_snapshot(&state.doc, &state.root).unwrap();
    assert_eq!(snapshot["body_hash"], manifest.body_hash);
    assert_eq!(snapshot["body_bytes"], manifest.body_bytes);
    assert_eq!(snapshot["editor_id"], "note-editor");
    assert_eq!(
        snapshot["ready"], false,
        "unlaid-out root must not report ready"
    );
    assert_eq!(snapshot["offset_x"], 0.0);
    assert_eq!(snapshot["offset_y"], 0.0);
    assert!(snapshot.get("body").is_none());
    assert_eq!(
        root.native_input_snapshot(&state.doc, &state.root).unwrap(),
        snapshot
    );
    drop((state, root));
    let before = std::fs::read(path.join("current")).unwrap();
    assert!(scratchpad::seed_native_fixture(&path).is_err());
    assert_eq!(std::fs::read(path.join("current")).unwrap(), before);
}

#[test]
fn native_editor_click_home_matches_fixture_hash_then_backspace_restores_it() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("note");
    let manifest = scratchpad::seed_native_fixture(&path).unwrap();
    {
        let (state, root) = scratchpad::open(&path).unwrap();
        let mut driver = Driver::new(state, root, (800, 600));
        scratchpad::register(driver.action_registry_mut());
        let editor = *driver.targets().get("note-editor").unwrap();
        eprintln!("native fixture editor bounds: {editor:?}");
        eprintln!("native-proof phase click");
        driver.click_at(editor.x0 + 8.0, editor.y0 + 8.0);
        eprintln!("native-proof phase home");
        driver.key("Home");
        eprintln!("native-proof phase type-x");
        driver.type_text("x");
        eprintln!("native-proof phase assert-x");
        let object = driver.state_mut().doc.get(ROOT, "body").unwrap().unwrap().1;
        let body = driver.state_mut().doc.text(object).unwrap();
        assert_eq!(body.len(), manifest.prefixed_x_bytes);
        assert_eq!(
            iroh_blobs::Hash::new(body.as_bytes()).to_hex(),
            manifest.prefixed_x_hash
        );
        eprintln!("native-proof phase backspace");
        driver.key("Backspace");
        eprintln!("native-proof phase save");
        driver.click_named("save-note").unwrap();
    }
    let (state, root) = scratchpad::open(&path).unwrap();
    let snapshot = root.native_input_snapshot(&state.doc, &state.root).unwrap();
    assert_eq!(snapshot["body_hash"], manifest.body_hash);
    assert_eq!(snapshot["body_bytes"], manifest.body_bytes);
}

// Read the production root's observer after real event dispatch and sync. The
// wrapper forwards the actual retained tree; it does not simulate input/state.
struct ObservedRoot {
    root: Box<dyn ui::widget::Widget>,
    observed: std::rc::Rc<std::cell::RefCell<serde_json::Value>>,
}
impl ui::widget::Widget for ObservedRoot {
    fn name(&self) -> &'static str {
        "ObservedScratchpad"
    }
    fn get_rect(&self) -> vello::kurbo::Rect {
        self.root.get_rect()
    }
    fn sync(&mut self, doc: &mut automerge::AutoCommit, path: &automerge::ObjId) {
        self.root.sync(doc, path);
        *self.observed.borrow_mut() = self.root.native_input_snapshot(doc, path).unwrap();
    }
    fn build_layout(
        &mut self,
        tree: &mut taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut std::collections::HashMap<taffy::NodeId, *mut dyn ui::widget::Widget>,
    ) -> taffy::NodeId {
        self.root.build_layout(tree, theme, nodes)
    }
    fn apply_layout(
        &mut self,
        tree: &taffy::TaffyTree<()>,
        node: taffy::NodeId,
        origin: vello::kurbo::Point,
    ) {
        self.root.apply_layout(tree, node, origin);
    }
    fn draw(
        &mut self,
        scene: &mut vello::Scene,
        transform: vello::kurbo::Affine,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.root.draw(scene, transform, text, theme, presence);
    }
    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        scale: f64,
        modifiers: &winit::event::Modifiers,
        doc: &mut automerge::AutoCommit,
        path: &automerge::ObjId,
        handled: &mut bool,
        focus: &mut Option<taffy::NodeId>,
        node: taffy::NodeId,
        tree: &taffy::TaffyTree<()>,
    ) {
        self.root.handle_event(
            event, scale, modifiers, doc, path, handled, focus, node, tree,
        );
        *self.observed.borrow_mut() = self.root.native_input_snapshot(doc, path).unwrap();
    }
    fn children(&self) -> Vec<&dyn ui::widget::Widget> {
        vec![self.root.as_ref()]
    }
    fn children_mut(&mut self) -> Vec<&mut (dyn ui::widget::Widget + '_)> {
        vec![self.root.as_mut()]
    }
}

#[test]
fn native_snapshot_observes_real_scroll_and_focused_edit_without_body_text() {
    let parent = tempfile::tempdir().unwrap();
    let path = parent.path().join("note");
    let manifest = scratchpad::seed_native_fixture(&path).unwrap();
    let (state, root) = scratchpad::open(&path).unwrap();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(serde_json::Value::Null));
    let root = ObservedRoot {
        root,
        observed: std::rc::Rc::clone(&observed),
    };
    let mut driver = Driver::new(state, Box::new(root), (800, 600));
    scratchpad::register(driver.action_registry_mut());
    driver.profile_frame();
    let rect = observed.borrow()["scroll_rect"].as_array().unwrap().clone();
    let coord = |index: usize| rect.get(index).unwrap().as_f64().unwrap();
    driver.send(&winit::event::WindowEvent::CursorMoved {
        device_id: winit::event::DeviceId::dummy(),
        position: winit::dpi::PhysicalPosition::new(
            (coord(0) + coord(2)) / 2.0,
            (coord(1) + coord(3)) / 2.0,
        ),
    });
    let before = observed.borrow()["offset_y"].as_f64().unwrap();
    driver.scroll(0.0, -3.0);
    let after = observed.borrow()["offset_y"].as_f64().unwrap();
    assert!(
        after > before,
        "real content must scroll: {before} -> {after}"
    );
    assert_eq!(observed.borrow()["body_hash"], manifest.body_hash);
    driver.scroll(0.0, 3.0);
    assert!(observed.borrow()["offset_y"].as_f64().unwrap() < after);
    eprintln!(
        "native-proof real scroll observed: {before} -> {after} -> {}",
        observed.borrow()["offset_y"]
    );
    let editor = *driver.targets().get("note-editor").unwrap();
    driver.click_at(editor.x0 + 8.0, editor.y0 + 8.0);
    driver.key("Home");
    driver.type_text("x");
    {
        let snapshot = observed.borrow();
        assert_eq!(snapshot["body_hash"], manifest.prefixed_x_hash);
        assert_eq!(snapshot["editor_focused"], true);
        assert_eq!(snapshot["ready"], true);
        assert!(snapshot.get("body").is_none());
    }
    driver.key("Backspace");
    assert_eq!(observed.borrow()["body_hash"], manifest.body_hash);
}

#[test]
fn native_focus_setup_reports_initial_and_tab_focus() {
    let directory = tempfile::tempdir().unwrap();
    let (state, root) = scratchpad::open(directory.path()).unwrap();
    let observed = std::rc::Rc::new(std::cell::RefCell::new(serde_json::Value::Null));
    let root = ObservedRoot {
        root,
        observed: std::rc::Rc::clone(&observed),
    };
    let mut driver = Driver::new(state, Box::new(root), (800, 600));
    scratchpad::register(driver.action_registry_mut());
    driver.profile_frame();
    let initial = observed.borrow()["editor_focused"].clone();
    driver.key("Tab");
    driver.profile_frame();
    let after_tab = observed.borrow()["editor_focused"].clone();
    eprintln!(
        "scratchpad focus initial={initial} after_tab={after_tab}, focused={:?}",
        driver.focused_access_node()
    );
    let editor = *driver.targets().get("note-editor").unwrap();
    driver.click_at(editor.x0 + 8.0, editor.y0 + 8.0);
    driver.profile_frame();
    assert_eq!(observed.borrow()["editor_focused"], true);
}
