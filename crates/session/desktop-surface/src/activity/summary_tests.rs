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
        pane.access_control(&ActivityFocus::SummarySource(pane.summary_source_key(&id)))
            .unwrap()
            .description
            .contains(&ui::widget::history_group::hex16(&id))
    );
    assert_eq!(
        pane.summary_accessibility_node().unwrap().description,
        Some(pane.summary_label())
    );
    assert!(
        pane.selection_history_actions()
            .iter()
            .all(|(id, _)| !id.starts_with("history.summary."))
    );
    assert!(pane.reveal_narrative_source(id));
    assert_eq!(pane.view, ActivityView::Explore);
    assert!(pane.selected_ids().contains(&id));
    assert_eq!(
        tree.doc.get_heads(),
        heads,
        "reading generated text and opening evidence never writes history"
    );
}

#[test]
fn narrow_generated_summary_preserves_paragraph_and_scrolls_to_evidence() {
    let (mut tree, mut pane) = fixture();
    pane.set_view(ActivityView::CatchUp);
    pane.layout_window(Rect::new(0.0, 0.0, 360.0, 400.0));
    let request = pane.narrative_request().unwrap();
    let id = *request.supplied_ids().iter().next().unwrap();
    let prose = "A complete generated paragraph with evidence and qualifications. ".repeat(4);
    assert!(pane.apply_generated_narrative(ui::GroundedNarrative {
        text: prose.clone(),
        source_ids: vec![id],
        generator: "remote configured-model".into(),
        request: Some(Box::new(request)),
    }));
    let mut text = TextRenderer::new();
    let theme = Theme::dark();
    pane.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::empty(),
    );
    assert!(pane.catchup_scroll.overflows_vertically());
    assert_eq!(
        pane.summary_accessibility_node().unwrap().name.as_deref(),
        Some(prose.as_str())
    );
    let details = pane.summary_details_rect.unwrap();
    let (_, paragraph_h) = text.measure_style(
        &prose,
        Some(text_width(
            pane.sheet.width() - 48.0 - f64::from(theme.tokens.metrics.scrollbar_gutter),
        )),
        &theme.text(TextRole::Caption),
    );
    assert!(details.y0 > f64::from(paragraph_h));
    let citation = ActivityFocus::SummarySource(pane.summary_source_key(&id));
    assert!(pane.focus_order().contains(&citation));
    let index = pane
        .focus_order()
        .iter()
        .position(|focus| focus == &citation)
        .unwrap();
    let key = usize::try_from(
        pane.accessibility_control_node(&citation, index, pane.focus_order().len())
            .unwrap()
            .key
            .unwrap(),
    )
    .unwrap();
    assert!(pane.focus_accessibility_path(&[0, key]));
    let rect = pane.rect_for_focus(&citation).unwrap();
    assert_eq!(pane.catchup_scroll.content_rect().intersect(rect), rect);
    let details_index = pane
        .focus_order()
        .iter()
        .position(|focus| focus == &ActivityFocus::SummaryDetails)
        .unwrap();
    assert!(pane.focus_accessibility_path(&[0, details_index]));
    assert!(pane.activate_focus(&mut tree, None).handled);
    pane.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut text,
        &theme,
        &PresenceCtx::empty(),
    );
    assert_eq!(
        pane.access_control(&ActivityFocus::SummaryDetails)
            .unwrap()
            .expanded,
        Some(true)
    );
    assert!(
        pane.summary_label()
            .contains("citations are not fact checks")
    );
}

#[test]
fn unbound_summary_actions_never_handle_or_change_selection() {
    let (mut tree, mut pane) = fixture();
    let heads = tree.doc.get_heads();
    for generated in [false, true] {
        if generated {
            let request = pane.narrative_request().unwrap();
            let id = *request.supplied_ids().iter().next().unwrap();
            assert!(pane.apply_generated_narrative(ui::GroundedNarrative {
                text: "Generated wording.".into(),
                source_ids: vec![id],
                generator: "configured-model".into(),
                request: Some(Box::new(request)),
            }));
        }
        let view = pane.view;
        let selection = pane.selected_ids();
        for action in std::iter::once("history.summary.read".to_owned())
            .chain((0..6).map(|slot| format!("history.summary.source.{slot}")))
        {
            assert!(
                !pane
                    .apply_selection_history_action(&action, &mut tree, None)
                    .handled
            );
            assert_eq!(pane.view, view);
            assert_eq!(pane.selected_ids(), selection);
        }
        assert!(
            pane.selection_history_actions()
                .iter()
                .all(|(id, _)| !id.starts_with("history.summary."))
        );
    }
    assert_eq!(tree.doc.get_heads(), heads);
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
