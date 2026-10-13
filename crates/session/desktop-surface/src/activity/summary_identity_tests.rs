//! Citation identity survives model ordering but never a different request.
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
    for name in ["First recorded rename", "Second recorded rename"] {
        tree.doc
            .set_actor(automerge::ActorId::from(b"citation-peer".as_slice()));
        tree.desktop.workspaces[0].name = name.into();
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader.clone());
        pane.refresh(&mut tree);
    }
    (tree, pane)
}

fn install(pane: &mut RoomActivityPane, ids: Vec<HistoryEntryId>) {
    pane.set_view(ActivityView::CatchUp);
    assert!(pane.apply_generated_narrative(ui::GroundedNarrative {
        text: "Generated account of recorded activity.".into(),
        source_ids: ids,
        generator: "test-model".into(),
        request: Some(Box::new(pane.narrative_request().unwrap())),
    }));
    pane.draw(
        &mut Scene::new(),
        Affine::IDENTITY,
        &mut TextRenderer::new(),
        &Theme::dark(),
        &PresenceCtx::empty(),
    );
}

#[test]
fn citation_keys_follow_exact_evidence_not_model_order_or_labels() {
    let (mut tree, mut pane) = fixture();
    let ids: Vec<_> = pane
        .narrative_request()
        .unwrap()
        .supplied_ids()
        .into_iter()
        .take(2)
        .collect();
    assert_eq!(ids.len(), 2);
    let heads = tree.doc.get_heads();
    install(&mut pane, ids.clone());
    let identity = pane.summary_source_key(&ids[0]);
    let key = usize::try_from(
        pane.accessibility_snapshot()
            .children
            .iter()
            .find(|node| {
                node.key
                    == Some(
                        ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                            .with_hashed_key(&identity)
                            .key
                            .unwrap(),
                    )
            })
            .expect("citation has an explicit stable key")
            .key
            .unwrap(),
    )
    .expect("accessibility key fits usize");
    install(&mut pane, vec![ids[1], ids[0]]);
    assert!(pane.focus_accessibility_path(&[0, key]));
    assert_eq!(
        pane.keyboard_focus,
        Some(ActivityFocus::SummarySource(identity.clone()))
    );
    pane.open_summary_citation(&identity);
    assert!(pane.selected_ids().contains(&ids[0]));

    pane.set_view(ActivityView::CatchUp);
    install(&mut pane, vec![ids[1]]);
    assert!(!pane.focus_accessibility_path(&[0, key]));
    pane.set_scope(RoomScope::AllWorkspaces, &tree);
    assert_ne!(pane.summary_source_key(&ids[0]), identity);
    let selection = pane.selected_ids();
    assert!(!pane.focus_accessibility_path(&[0, key]));
    pane.open_summary_citation(&identity);
    assert_eq!(pane.selected_ids(), selection);
    assert_eq!(tree.doc.get_heads(), heads);
}

#[test]
fn language_change_invalidates_summary_on_tick_and_rebuilds_unchanged_history() {
    let (mut tree, mut pane) = fixture();
    let original_locale = ui::i18n::I18nManager::current_locale();
    let old = pane.narrative_request().unwrap();
    let id = *old.supplied_ids().iter().next().unwrap();
    install(&mut pane, vec![id]);
    let heads = tree.doc.get_heads();
    let next = if original_locale == "de-DE" {
        "en-US"
    } else {
        "de-DE"
    };
    ui::i18n::I18nManager::set_locale(next);
    pane.refresh_narrative_locale();
    assert!(pane.narrative.is_none());
    assert!(pane.summary_source_rects.is_empty());
    assert_eq!(pane.narrative_request().unwrap().locale, next);
    assert!(!pane.apply_generated_narrative(ui::GroundedNarrative {
        text: "Stale language.".into(),
        source_ids: vec![id],
        generator: "test-model".into(),
        request: Some(Box::new(old)),
    }));
    // A no-op history refresh still updates the full localized projection.
    pane.refresh(&mut tree);
    assert_eq!(pane.projection_locale, next);
    assert_eq!(tree.doc.get_heads(), heads);
    assert!(!pane.entries().is_empty());
    ui::i18n::I18nManager::set_locale(&original_locale);
}
