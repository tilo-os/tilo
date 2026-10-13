//! Held accessibility nodes authorize only their original summary presentation.
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
fn held_native_summary_nodes_authorize_only_the_original_pane_lifetime() {
    let mut tree = TestTree::bootstrap_demo();
    let mut original = RoomActivityPane::new();
    let mut replacement = RoomActivityPane::with_local_user(TestHost::default().local_user());
    // The constructors share one allocation sequence, independent of host services.
    assert!(replacement.narrative_owner() > original.narrative_owner());
    let owner = original.narrative_owner();
    for pane in [&mut original, &mut replacement] {
        pane.set_host(Rc::new(TestHost::default()));
        pane.set_view(ActivityView::Explore);
        pane.layout(Rect::new(0.0, 0.0, 1600.0, 900.0));
        pane.refresh(&mut tree);
    }
    let reader = tree.doc.get_actor().clone();
    tree.doc
        .set_actor(automerge::ActorId::from(b"citation-peer".as_slice()));
    tree.desktop.workspaces[0].name = "Recorded rename".into();
    tree.seed_doc_from_cache();
    tree.doc.set_actor(reader);
    for pane in [&mut original, &mut replacement] {
        pane.refresh(&mut tree);
    }
    assert_eq!(
        original.narrative_request(),
        replacement.narrative_request()
    );
    let id = *original
        .narrative_request()
        .unwrap()
        .supplied_ids()
        .iter()
        .next()
        .unwrap();
    for pane in [&mut original, &mut replacement] {
        install(pane, vec![id]);
    }
    assert_eq!(original.narrative(), replacement.narrative());
    assert_eq!(
        original.narrative_generation(),
        replacement.narrative_generation()
    );
    assert_eq!(original.narrative_owner(), owner);
    let heads = tree.doc.get_heads();
    let held_text = original.summary_accessibility_node().unwrap();
    let held_citation = original.summary_source_key(&id);
    let key = usize::try_from(
        ui::a11y::AccessNode::new(ui::a11y::Role::Button)
            .with_hashed_key(&held_citation)
            .key
            .unwrap(),
    )
    .unwrap();
    assert!(
        original
            .accessibility_snapshot()
            .children
            .iter()
            .any(|node| node.key == Some(key as u64))
    );
    assert!(original.focus_accessibility_path(&[0, key]));
    assert!(!replacement.focus_accessibility_path(&[0, key]));
    assert_ne!(
        replacement.summary_accessibility_node().unwrap().key,
        held_text.key
    );
    assert!(replacement.summary_source_id(&held_citation).is_none());
    let selection = replacement.selected_ids();
    replacement.open_summary_citation(&held_citation);
    assert_eq!(replacement.selected_ids(), selection);
    original.open_summary_citation(&held_citation);
    assert!(original.selected_ids().contains(&id));
    let current = replacement.summary_source_key(&id);
    replacement.open_summary_citation(&current);
    assert!(replacement.selected_ids().contains(&id));
    drop(original);
    assert!(RoomActivityPane::default().narrative_owner() > replacement.narrative_owner());
    assert_eq!(tree.doc.get_heads(), heads);
}

#[test]
fn held_citation_keys_refuse_replacements_and_citation_count_changes() {
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
    // Repeated publication of the exact same presentation preserves its keys.
    install(&mut pane, ids.clone());
    assert!(pane.focus_accessibility_path(&[0, key]));
    assert_eq!(
        pane.keyboard_focus,
        Some(ActivityFocus::SummarySource(identity.clone()))
    );
    pane.open_summary_citation(&identity);
    assert!(pane.selected_ids().contains(&ids[0]));

    pane.set_view(ActivityView::CatchUp);
    let mut replacement = pane.narrative().unwrap();
    replacement.text = "A different generated account of the same evidence.".into();
    assert!(pane.apply_generated_narrative(replacement));
    assert!(!pane.focus_accessibility_path(&[0, key]));
    assert!(pane.summary_source_id(&identity).is_none());
    let generation = pane.narrative_generation();
    install(&mut pane, vec![ids[1], ids[0]]);
    assert!(pane.narrative_generation() > generation);
    assert!(!pane.focus_accessibility_path(&[0, key]));
    let selection = pane.selected_ids();
    pane.open_summary_citation(&identity);
    assert_eq!(pane.selected_ids(), selection);
    // Even returning to the original prose and evidence cannot revive a held node.
    install(&mut pane, ids.clone());
    assert!(!pane.focus_accessibility_path(&[0, key]));
    let replacement_key = pane.summary_source_key(&ids[0]);
    install(&mut pane, vec![ids[0]]);
    assert!(pane.summary_source_id(&replacement_key).is_none());
    assert_eq!(pane.summary_source_rects.len(), 1);
    install(&mut pane, ids.clone());
    assert_eq!(pane.summary_source_rects.len(), 2);
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
fn held_text_feedback_and_citations_never_revive_after_absence_or_scope_aba() {
    let (tree, mut pane) = fixture();
    let id = *pane
        .narrative_request()
        .unwrap()
        .supplied_ids()
        .iter()
        .next()
        .unwrap();
    install(&mut pane, vec![id]);
    let held_text = pane.summary_accessibility_node().unwrap();
    let held_citation = pane.summary_source_key(&id);
    assert!(held_text.key.is_some());
    assert_eq!(
        pane.summary_accessibility_node().unwrap().key,
        held_text.key
    );

    pane.set_scope(RoomScope::AllWorkspaces, &tree);
    pane.set_scope(RoomScope::ThisRoom, &tree);
    install(&mut pane, vec![id]);
    assert_ne!(
        pane.summary_accessibility_node().unwrap().key,
        held_text.key
    );
    assert!(pane.summary_source_id(&held_citation).is_none());

    let digest = pane.digest.clone();
    let generation = pane.narrative_generation();
    pane.digest = ActivityDigest::default();
    pane.reset_narrative();
    assert!(pane.narrative_request().is_none());
    assert!(pane.summary_accessibility_node().is_none());
    assert!(pane.narrative_generation() > generation);
    let absent_generation = pane.narrative_generation();
    pane.reset_narrative();
    assert_eq!(pane.narrative_generation(), absent_generation);
    // Locale transitions are tracked even while there is no request or prose.
    let original_locale = ui::i18n::I18nManager::current_locale();
    let next = if original_locale == "de-DE" {
        "en-US"
    } else {
        "de-DE"
    };
    ui::i18n::I18nManager::set_locale(next);
    pane.refresh_narrative_locale();
    assert!(pane.narrative_generation() > absent_generation);
    let localized_absence = pane.narrative_generation();
    ui::i18n::I18nManager::set_locale(&original_locale);
    pane.refresh_narrative_locale();
    assert!(pane.narrative_generation() > localized_absence);
    pane.digest = digest;
    pane.reset_narrative();
    let held_feedback = pane.summary_accessibility_node().unwrap();
    assert!(held_feedback.key.is_some());
    assert_eq!(
        pane.summary_accessibility_node().unwrap().key,
        held_feedback.key
    );
    pane.set_narrative_feedback("activity-summary-pending");
    pane.set_narrative_feedback("activity-summary-unavailable");
    assert_ne!(
        pane.summary_accessibility_node().unwrap().key,
        held_feedback.key
    );
    install(&mut pane, vec![id]);
    assert_ne!(
        pane.summary_accessibility_node().unwrap().key,
        held_text.key
    );
    assert!(pane.summary_source_id(&held_citation).is_none());
    let current = pane.summary_source_key(&id);
    pane.set_narrative_feedback("activity-summary-pending");
    assert!(pane.narrative().is_none());
    install(&mut pane, vec![id]);
    assert!(pane.summary_source_id(&current).is_none());
}

#[test]
fn language_change_invalidates_summary_on_tick_and_rebuilds_unchanged_history() {
    let (mut tree, mut pane) = fixture();
    let original_locale = ui::i18n::I18nManager::current_locale();
    let old = pane.narrative_request().unwrap();
    let id = *old.supplied_ids().iter().next().unwrap();
    install(&mut pane, vec![id]);
    let held = pane.summary_source_key(&id);
    let held_text = pane.summary_accessibility_node().unwrap().key;
    let generation = pane.narrative_generation();
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
    assert!(pane.narrative_generation() > generation);
    assert!(pane.summary_source_id(&held).is_none());
    assert_ne!(pane.summary_accessibility_node().unwrap().key, held_text);
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
    pane.refresh_narrative_locale();
    install(&mut pane, vec![id]);
    assert!(pane.summary_source_id(&held).is_none());
    assert_ne!(pane.summary_accessibility_node().unwrap().key, held_text);
}

#[test]
fn identical_summary_and_feedback_reentry_revoke_held_native_keys_without_viewed_changes() {
    for close in [false, true] {
        let (mut tree, mut pane) = fixture();
        pane.set_view(ActivityView::CatchUp);
        let request = pane.narrative_request().unwrap();
        let heads = tree.doc.get_heads();
        let workspace = tree.active.raw();
        let held_feedback = pane.summary_accessibility_node().unwrap().key;
        if close {
            pane.finish_view(workspace);
        } else {
            pane.set_view(ActivityView::Explore);
        }
        assert!(pane.summary_accessibility_node().is_none());
        if close {
            pane.begin_view(workspace);
        } else {
            pane.set_view(ActivityView::CatchUp);
        }
        assert_eq!(pane.narrative_request(), Some(request.clone()));
        assert_ne!(
            pane.summary_accessibility_node().unwrap().key,
            held_feedback
        );
        let id = *request.supplied_ids().iter().next().unwrap();
        // Install without drawing: there are no acknowledged brief cards.
        assert!(pane.apply_generated_narrative(ui::GroundedNarrative {
            text: "Retained account.".into(),
            source_ids: vec![id],
            generator: "test-model".into(),
            request: Some(Box::new(request.clone())),
        }));
        let retained = pane.narrative();
        let held_text = pane.summary_accessibility_node().unwrap().key;
        let held = pane.summary_source_key(&id);
        let held_key = usize::try_from(
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_hashed_key(&held)
                .key
                .unwrap(),
        )
        .unwrap();
        assert!(pane.focus_accessibility_path(&[0, held_key]));
        if close {
            pane.finish_view(workspace);
        } else {
            pane.set_view(ActivityView::Explore);
        }
        assert!(!pane.focus_accessibility_path(&[0, held_key]));
        if close {
            pane.begin_view(workspace);
        } else {
            pane.set_view(ActivityView::CatchUp);
        }
        assert_eq!(pane.narrative_request(), Some(request));
        assert_eq!(pane.narrative(), retained);
        assert_ne!(pane.summary_accessibility_node().unwrap().key, held_text);
        assert!(pane.summary_source_id(&held).is_none());
        assert!(!pane.focus_accessibility_path(&[0, held_key]));
        let fresh = pane.summary_source_key(&id);
        let fresh_key = usize::try_from(
            ui::a11y::AccessNode::new(ui::a11y::Role::Button)
                .with_hashed_key(&fresh)
                .key
                .unwrap(),
        )
        .unwrap();
        assert!(pane.focus_accessibility_path(&[0, fresh_key]));
        pane.open_summary_citation(&fresh);
        assert!(pane.selected_ids().contains(&id));
        assert_eq!(tree.doc.get_heads(), heads);
    }
}
