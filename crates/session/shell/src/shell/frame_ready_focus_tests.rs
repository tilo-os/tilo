//! The onboarding ready screen as the compositor drives it: where keyboard
//! focus lands when the screen appears, where Tab takes it, and what the
//! accessibility tree reports as focused, which is what a screen reader and
//! the release gates read over AT-SPI.

use super::*;
use winit::keyboard::{Key, NamedKey};

fn name_of(fb: &FrameBuilder, focus: Option<NodeId>) -> Option<String> {
    let ptr = fb.node_map.get(&focus?)?;
    // SAFETY: the map belongs to the frame's current, live layout tree.
    unsafe { (**ptr).accessibility()?.name }
}

/// The label of the node the exported accessibility tree marks focused.
fn exported_focus(fb: &FrameBuilder, focus: Option<NodeId>) -> Option<String> {
    let update = fb.build_a11y_tree_update(focus, 1.0);
    update
        .nodes
        .iter()
        .find(|(id, _)| *id == update.focus)
        .and_then(|(_, node)| node.label().map(str::to_owned))
}

#[test]
fn the_ready_screen_holds_focus_on_its_action_and_tab_keeps_a_named_focus() {
    let identity_dir = tempfile::tempdir().expect("temporary identity directory");
    let mut fb = FrameBuilder::new((1280, 800));
    let identity_path = identity_dir.path().join("primary.json");
    let view = crate::onboarding_view::OnboardingView::with_minter(Box::new(move || {
        Ok(crate::onboarding::MintedIdentity {
            fingerprint: "ab".repeat(32),
            path: identity_path.clone(),
        })
    }));
    let press = view.presser();
    fb.enable_test_onboarding(view);
    fb.relayout();
    let mut doc = AutoCommit::new();
    let mut focus = None;
    // Setup is an app: it lays out its page in the frame that draws it.
    let settle = |fb: &mut FrameBuilder, doc: &mut AutoCommit| {
        fb.pump_apps(doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
        fb.pump_apps(doc, &automerge::ROOT);
        fb.relayout();
        let _ = fb.paint();
    };
    press(crate::onboarding_view::KEY_BEGIN);
    settle(&mut fb, &mut doc);
    doc.put(
        &automerge::ROOT,
        crate::onboarding_view::KEY_UNDERSTOOD,
        true,
    )
    .expect("acknowledge identity protection");
    settle(&mut fb, &mut doc);
    press(crate::onboarding_view::KEY_CREATE);
    settle(&mut fb, &mut doc);
    doc.put(
        &automerge::ROOT,
        crate::onboarding_view::KEY_BACKUP_SKIP_ACKNOWLEDGED,
        true,
    )
    .expect("acknowledge going without a backup");
    settle(&mut fb, &mut doc);
    press(crate::onboarding_view::KEY_BACKUP_CONTINUE);
    for _ in 0..50 {
        settle(&mut fb, &mut doc);
        if fb
            .widget_registry()
            .contains_key("onboarding.finish.button")
        {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(
        fb.widget_registry()
            .contains_key("onboarding.finish.button"),
        "the walk reaches the ready screen"
    );
    fb.reconcile_native_focus(&mut focus);
    // The app moves its own focus in the frame after it is given the keyboard.
    let _ = fb.paint();
    assert_eq!(
        name_of(&fb, focus).as_deref(),
        Some("Setup"),
        "the shell's keyboard is with the setup app"
    );
    assert_eq!(
        exported_focus(&fb, focus).as_deref(),
        Some("Continue to desktop"),
        "which puts focus on the ready screen's one action and says so over AT-SPI"
    );

    let tab = ui::synthetic_input::key_press(Key::Named(NamedKey::Tab), None);
    assert!(fb.dispatch_event(
        &tab,
        &winit::event::Modifiers::default(),
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        &mut focus,
    ));
    fb.relayout();
    fb.reconcile_native_focus(&mut focus);
    let after_tab = exported_focus(&fb, focus);
    assert!(
        after_tab.as_deref().is_some_and(|name| !name.is_empty()),
        "Tab leaves focus on a named node, not the window: {after_tab:?} (widget {:?})",
        name_of(&fb, focus)
    );
}
