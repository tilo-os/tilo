//! Deferred input: the stamp an edit is checked against, and the embedded
//! app it follows.

use super::*;

#[test]
fn deferred_input_stamp_checks_active_custody_and_exhausted_admission() {
    let mut fixture = ActionFixture::new();
    let owner = fixture.bindings[0].clone();
    let node = *fixture
        .app
        .layout
        .node_map
        .keys()
        .find(|node| {
            fixture
                .app
                .bindings
                .binding_for(**node)
                .is_some_and(|binding| binding.same_component(&owner))
        })
        .unwrap();
    owner
        .with_settlement(&mut fixture.app.state.doc, |doc, _| {
            assert!(
                fixture.app.bindings.document_edit_stamp(node, doc).is_ok(),
                "an active read needs the explicit canonical document witness"
            );
            assert!(
                fixture
                    .app
                    .bindings
                    .document_edit_stamp(node, &automerge::AutoCommit::new())
                    .is_err(),
                "a host or unrelated document cannot stand in for an active canonical loan"
            );
        })
        .unwrap();
    fixture
        .records
        .with_history(&owner.checkout.id, |_, _| {
            assert!(
                fixture
                    .app
                    .bindings
                    .document_edit_stamp(node, &fixture.app.state.doc)
                    .is_err(),
                "a borrowed parked record cannot issue a stamp"
            );
        })
        .unwrap();
    fixture
        .records
        .with_action_state(&owner.checkout.id, |_, _, state| {
            state.generation = Some(u64::MAX);
        })
        .unwrap();
    assert!(
        fixture
            .app
            .bindings
            .document_edit_stamp(node, &fixture.app.state.doc)
            .is_err(),
        "an exhausted admission counter must fail closed even though its value is not hashed"
    );
}

#[test]
fn deferred_input_stamp_is_read_only_and_invalidates_an_edit_and_its_inverse() {
    let mut fixture = ActionFixture::new();
    let owner = fixture.bindings[0].clone();
    let node = *fixture
        .app
        .layout
        .node_map
        .keys()
        .find(|node| {
            fixture
                .app
                .bindings
                .binding_for(**node)
                .is_some_and(|binding| binding.same_component(&owner))
        })
        .unwrap();
    let pending = fixture.app.state.doc.pending_ops();
    let before = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    let repeated = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    assert!(before == repeated);
    assert_eq!(
        fixture.app.state.doc.pending_ops(),
        pending,
        "reading a stamp must not commit or mutate the host document"
    );
    owner
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.put(ROOT, "dictation-guard", "edit").unwrap();
        })
        .unwrap();
    let edited = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    assert!(edited != before);
    owner
        .with_document(&mut fixture.app.state.doc, |doc, _| {
            doc.delete(ROOT, "dictation-guard").unwrap();
        })
        .unwrap();
    let reverted = fixture
        .app
        .bindings
        .document_edit_stamp(node, &fixture.app.state.doc)
        .unwrap();
    assert!(
        reverted != edited && reverted != before,
        "restoring the same text must not revive a deferred insertion"
    );
    owner.closing.set(true);
    assert!(
        fixture
            .app
            .bindings
            .document_edit_stamp(node, &fixture.app.state.doc)
            .is_err(),
        "a retired owner cannot issue a live stamp"
    );
}

#[test]
fn deferred_input_embedded_app_follows_its_actual_editable_child() {
    use crate::widget::{Column, TextInput, Widget};
    let mut app = App::from_scoped_parts_with_controlled_inputs(
        crate::wake::WakeScope::new(),
        State::new(),
        Box::new(
            Column::new()
                .push(Box::new(TextInput::local().with_initial_value("same")))
                .push(Box::new(TextInput::local().with_initial_value("same"))),
        ),
    );
    app.init_layout();
    app.refresh_focus_order();
    let first = app.focusable_order[0];
    let second = app.focusable_order[1];
    app.set_focused_node(Some(first));
    let mut embedded = crate::app::EmbeddedApp::new(app, |_| {}).unwrap();
    assert!(crate::ime::ImeWindowState::from_widget(&embedded).is_some());
    let stamp = embedded.ime_edit_fingerprint(&[3; 32]).unwrap();
    embedded.app_mut().set_focused_node(Some(second));
    assert_ne!(
        embedded.ime_edit_fingerprint(&[3; 32]).unwrap(),
        stamp,
        "identical embedded fields must remain distinct insertion targets"
    );
    let stamp = embedded.ime_edit_fingerprint(&[3; 32]).unwrap();
    embedded
        .app_mut()
        .state
        .doc
        .put(ROOT, "guard", "edit")
        .unwrap();
    embedded.app_mut().state.doc.delete(ROOT, "guard").unwrap();
    assert_ne!(
        embedded.ime_edit_fingerprint(&[3; 32]).unwrap(),
        stamp,
        "an edit and inverse must invalidate deferred embedded input"
    );
    embedded.app_mut().set_focused_node(None);
    assert!(crate::ime::ImeWindowState::from_widget(&embedded).is_none());
    assert!(embedded.ime_edit_fingerprint(&[3; 32]).is_none());
}
