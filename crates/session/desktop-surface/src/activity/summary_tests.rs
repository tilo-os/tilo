//! Generated summaries exercise the production history projection and evidence door.
use super::*;
use crate::activity::testing::{TestHost, TestTree};

fn fixture() -> (TestTree, RoomActivityPane) {
    let mut tree = TestTree::bootstrap_demo();
    let mut pane = RoomActivityPane::new();
    pane.set_host(Rc::new(TestHost::default()));
    pane.set_view(ActivityView::Explore);
    pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
    pane.refresh(&mut tree);
    let reader = tree.doc.get_actor().clone();
    tree.doc
        .set_actor(automerge::ActorId::from(b"summary-peer".as_slice()));
    tree.desktop.workspaces[0].name = "Recorded rename".into();
    tree.seed_doc_from_cache();
    tree.doc.set_actor(reader);
    pane.refresh(&mut tree);
    (tree, pane)
}

#[test]
fn generated_summary_reads_as_static_text_and_sources_open_existing_explorer() {
    let (mut tree, mut pane) = fixture();
    let heads = tree.doc.get_heads();
    let request = pane.narrative_request().expect("recorded activity");
    let id = *request.supplied_ids().iter().next().unwrap();
    assert!(pane.apply_generated_narrative(ui::GroundedNarrative {
        text: "Generated wording, not recorded evidence.".into(),
        source_ids: vec![id],
        generator: "configured-model".into(),
        request: Some(Box::new(request)),
    }));
    pane.set_view(ActivityView::CatchUp);
    // Switching views does not authorize a different request.
    if pane.narrative.is_none() {
        let request = pane.narrative_request().unwrap();
        assert!(pane.apply_generated_narrative(ui::GroundedNarrative {
            text: "Generated wording, not recorded evidence.".into(),
            source_ids: vec![id],
            generator: "configured-model".into(),
            request: Some(Box::new(request)),
        }));
    }
    let mut scene = Scene::new();
    pane.draw(
        &mut scene,
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
    let snapshot = pane.accessibility_snapshot();
    assert!(
        snapshot
            .children
            .iter()
            .any(|node| node.role == ui::a11y::Role::StaticText
                && node.name.as_deref() == Some("Generated wording, not recorded evidence."))
    );
    assert!(pane.summary_label().contains("configured-model"));
    assert!(pane.summary_label().contains("not fact checks"));
    assert_eq!(pane.summary_source_rects.len(), 1);
    assert!(
        pane.access_control(&ActivityFocus::SummarySource(0))
            .unwrap()
            .description
            .contains(&ui::widget::history_group::hex16(&id))
    );
    assert!(
        pane.selection_history_actions()
            .iter()
            .any(|(id, _)| *id == "history.summary.read")
    );
    let outcome = pane.apply_selection_history_action(SUMMARY_SOURCE_ACTIONS[0], &mut tree, None);
    assert!(outcome.handled);
    assert_eq!(pane.view, ActivityView::Explore);
    assert!(pane.selected_ids().contains(&id));
    assert_eq!(
        tree.doc.get_heads(),
        heads,
        "reading generated text and opening evidence never writes history"
    );
}

#[test]
fn same_evidence_with_changed_scope_rejects_stale_generated_output() {
    let (mut tree, mut pane) = fixture();
    let old = pane.narrative_request().unwrap();
    let id = *old.supplied_ids().iter().next().unwrap();
    let heads = tree.doc.get_heads();
    pane.set_scope(RoomScope::AllWorkspaces, &tree);
    let current = pane.narrative_request().unwrap();
    assert_ne!(old.projection, current.projection);
    assert!(!pane.apply_generated_narrative(ui::GroundedNarrative {
        text: "Stale generated summary".into(),
        source_ids: vec![id],
        generator: "model".into(),
        request: Some(Box::new(old)),
    }));
    assert!(pane.narrative.is_none());
    let scoped = pane.narrative_request().unwrap();
    pane.mark_desktop_history_capped();
    assert_ne!(
        pane.narrative_request().unwrap().projection,
        scoped.projection
    );
    pane.set_view(ActivityView::CatchUp);
    pane.set_narrative_feedback("activity-summary-failed");
    assert!(
        pane.summary_accessibility_node()
            .unwrap()
            .name
            .unwrap()
            .contains("failed")
    );
    pane.set_narrative_feedback("activity-summary-unavailable");
    assert!(
        pane.summary_accessibility_node()
            .unwrap()
            .name
            .unwrap()
            .contains("No summary model")
    );
    assert_eq!(tree.doc.get_heads(), heads);
    assert!(
        !pane.entries().is_empty(),
        "model failure cannot remove deterministic history"
    );
}
