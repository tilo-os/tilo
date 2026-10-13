//! Deferred summary actions are revalidated against the actual overlay host.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    reason = "test fixtures and assertions"
)]

use shell::shell::build_tilo_overlay_host_with_inputs;
use shell::shell::hosted_inputs::HostedTileInputs;
use shell::shell::room_activity::RoomActivityPane;
use shell::shell::state::WorkspaceTree;
use ui::widget::Widget as _;

use crate::support::host::{layout_host, paint, press_key};

/// Find the exported synthetic address, not the pane's internal focus order.
fn citation_path(
    host: &shell::shell::overlay_host::ShellOverlayHost,
    id: &ui::widget::HistoryEntryId,
) -> Option<Vec<usize>> {
    let root = host.accessibility().expect("shell accessibility tree");
    let evidence = ui::widget::history_group::hex16(id);
    root.children.iter().enumerate().find_map(|(window, node)| {
        node.children.iter().find_map(|child| {
            (child.role == ui::a11y::Role::Button
                && child
                    .description
                    .as_deref()
                    .is_some_and(|description| description.contains(&evidence)))
            .then(|| {
                vec![
                    window,
                    usize::try_from(child.key.expect("citation has a stable key"))
                        .expect("accessibility key fits usize"),
                ]
            })
        })
    })
}

/// Record real workspace changes and ground a summary in the host's history.
fn accessibility_summary_fixture() -> (
    shell::shell::overlay_host::ShellOverlayHost,
    shell::shell::chrome_state::Chrome,
    ui::GroundedNarrative,
) {
    use shell::shell::room_activity::ActivityView;

    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
    shell::shell::tab_binding::open_workspace_activity(&mut tree);
    let (mut host, chrome) = build_tilo_overlay_host_with_inputs(
        tree.shared(),
        false,
        ui::record::RecordStore::in_memory(),
        HostedTileInputs::fixture().expect("fixture inputs"),
    );
    *host.room_activity_mut() = RoomActivityPane::new();
    host.room_activity_mut().set_view(ActivityView::Explore);
    layout_host(&mut host);
    host.force_room_activity_settled();
    for name in ["First recorded rename", "Second recorded rename"] {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let reader = tree.doc.get_actor().clone();
        tree.doc
            .set_actor(automerge::ActorId::from(name.as_bytes()));
        tree.desktop.workspaces[0].name = name.into();
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        drop(tree);
        drop(state);
        host.force_room_activity_settled();
    }
    let request = host.room_activity().narrative_request().unwrap();
    let ids: Vec<_> = request.supplied_ids().into_iter().take(2).collect();
    assert_eq!(ids.len(), 2);
    let narrative = ui::GroundedNarrative {
        text: "Recorded workspace renames.".into(),
        source_ids: ids,
        generator: "test-model".into(),
        request: Some(Box::new(request)),
    };
    (host, chrome, narrative)
}

#[test]
fn history_summary_host_accessibility_keeps_exact_citations_and_revokes_stale_ones() {
    use shell::shell::room_activity::ActivityView;
    use winit::keyboard::{Key, ModifiersState, NamedKey};

    // Locale is thread-local; restore it even if an assertion fails.
    struct RestoreLocale(String);
    impl Drop for RestoreLocale {
        fn drop(&mut self) {
            ui::i18n::I18nManager::set_locale(&self.0);
        }
    }
    let locale = RestoreLocale(ui::i18n::I18nManager::current_locale());
    let (mut host, chrome, narrative) = accessibility_summary_fixture();
    let ids = narrative.source_ids.clone();
    host.room_activity_mut().set_view(ActivityView::CatchUp);
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(narrative.clone())
    );
    host.force_room_activity_settled();
    paint(&mut host);
    let root = host.accessibility().unwrap();
    assert!(
        root.children
            .iter()
            .any(|window| window.children.iter().any(|node| {
                node.role == ui::a11y::Role::StaticText
                    && node.name.as_deref() == Some(narrative.text.as_str())
            }))
    );
    let path = citation_path(&host, &ids[0]).expect("citation exported by shell host");
    let other_path = citation_path(&host, &ids[1]).expect("second citation exported");
    assert_ne!(path, other_path);
    let heads = chrome.borrow().tree_mut().doc.get_heads();

    let mut replacement = narrative.clone();
    replacement.source_ids.reverse();
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(replacement.clone())
    );
    host.force_room_activity_settled();
    paint(&mut host);
    assert_eq!(citation_path(&host, &ids[0]), Some(path.clone()));
    assert_eq!(citation_path(&host, &ids[1]), Some(other_path.clone()));
    assert!(host.focus_synthetic_accessibility(&path));
    let (taffy, node) = layout_host(&mut host);
    assert!(press_key(
        &mut host,
        &taffy,
        node,
        Key::Named(NamedKey::Enter),
        ModifiersState::empty(),
    ));
    assert_eq!(host.room_activity().view(), ActivityView::Explore);
    assert!(host.room_activity().selected_ids().contains(&ids[0]));
    assert!(!host.room_activity().selected_ids().contains(&ids[1]));

    // A removed citation must not alias the surviving button's ordinal.
    host.room_activity_mut().set_view(ActivityView::CatchUp);
    replacement.source_ids = vec![ids[1]];
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(replacement)
    );
    host.force_room_activity_settled();
    paint(&mut host);
    assert!(citation_path(&host, &ids[0]).is_none());
    assert_eq!(citation_path(&host, &ids[1]), Some(other_path.clone()));
    let selection = host.room_activity().selected_ids();
    assert!(!host.focus_synthetic_accessibility(&path));
    assert_eq!(host.room_activity().selected_ids(), selection);
    assert_eq!(host.room_activity().view(), ActivityView::CatchUp);

    // The same history in another language invalidates both exported prose
    // and held citation addresses through the host's ordinary refresh path.
    let next_locale = if locale.0 == "de-DE" {
        "en-US"
    } else {
        "de-DE"
    };
    ui::i18n::I18nManager::set_locale(next_locale);
    host.force_room_activity_settled();
    paint(&mut host);
    assert!(chrome.borrow().history_summary_binding().is_none());
    assert!(host.room_activity().narrative().is_none());
    assert_eq!(
        host.room_activity().narrative_request().unwrap().locale,
        next_locale
    );
    assert!(!host.focus_synthetic_accessibility(&other_path));
    assert!(
        !host
            .room_activity_mut()
            .apply_generated_narrative(narrative)
    );
    assert!(!host.room_activity().entries().is_empty());
    assert_eq!(host.room_activity().selected_ids(), selection);
    let root = host.accessibility().unwrap();
    assert!(!root.children.iter().any(|window| {
        window
            .children
            .iter()
            .any(|node| node.name.as_deref() == Some("Recorded workspace renames."))
    }));
    assert_eq!(chrome.borrow().tree_mut().doc.get_heads(), heads);
}

#[test]
fn history_summary_host_revalidates_deferred_citations() {
    use shell::shell::room_activity::{ActivityView, RoomScope};

    let mut tree = WorkspaceTree::bootstrap_demo();
    tree.adopt_ownerless_workspaces(&shell::shell::state::local_user_id());
    let (mut host, chrome) = build_tilo_overlay_host_with_inputs(
        tree.shared(),
        false,
        ui::record::RecordStore::in_memory(),
        HostedTileInputs::fixture().expect("fixture inputs"),
    );
    *host.room_activity_mut() = RoomActivityPane::new();
    host.room_activity_mut().set_view(ActivityView::Explore);
    host.force_room_activity_settled();
    for name in ["First recorded rename", "Second recorded rename"] {
        let state = chrome.borrow();
        let mut tree = state.tree_mut();
        let reader = tree.doc.get_actor().clone();
        tree.doc
            .set_actor(automerge::ActorId::from(b"summary-peer".as_slice()));
        tree.desktop.workspaces[0].name = name.into();
        tree.seed_doc_from_cache();
        tree.doc.set_actor(reader);
        drop(tree);
        drop(state);
        host.force_room_activity_settled();
    }
    let request = host
        .room_activity()
        .narrative_request()
        .expect("recorded briefs");
    let ids: Vec<_> = request.supplied_ids().into_iter().take(2).collect();
    assert_eq!(ids.len(), 2);
    host.room_activity_mut().set_view(ActivityView::CatchUp);
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(ui::GroundedNarrative {
                text: "Recorded workspace renames.".into(),
                source_ids: ids.clone(),
                generator: "test-model".into(),
                request: Some(Box::new(request)),
            })
    );
    host.force_room_activity_settled();
    let binding = chrome
        .borrow()
        .history_summary_binding()
        .expect("host published prose")
        .clone();
    let heads = chrome.borrow().tree_mut().doc.get_heads();
    chrome
        .borrow_mut()
        .request_history_summary_action(&binding, Some(0))
        .unwrap();
    host.force_room_activity_settled();
    assert_eq!(host.room_activity().view(), ActivityView::Explore);
    assert!(host.room_activity().selected_ids().contains(&ids[0]));

    // Read reveals the same prose without rewriting history.
    chrome
        .borrow_mut()
        .request_history_summary_action(&binding, None)
        .unwrap();
    host.force_room_activity_settled();
    assert_eq!(host.room_activity().view(), ActivityView::CatchUp);

    // Reordering model citations preserves addresses, but invalidates the
    // original full presentation queued before that replacement.
    chrome
        .borrow_mut()
        .request_history_summary_action(&binding, Some(0))
        .unwrap();
    let mut replacement = binding.narrative.clone();
    replacement.source_ids.reverse();
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(replacement)
    );
    host.force_room_activity_settled();
    assert_eq!(host.room_activity().view(), ActivityView::CatchUp);
    let reordered = chrome.borrow().history_summary_binding().unwrap().clone();
    assert_eq!(binding.action_id(Some(0)), reordered.action_id(Some(1)));
    chrome
        .borrow_mut()
        .request_history_summary_action(&reordered, Some(1))
        .unwrap();
    host.force_room_activity_settled();
    assert!(host.room_activity().selected_ids().contains(&ids[0]));

    chrome
        .borrow_mut()
        .request_history_summary_action(&reordered, None)
        .unwrap();
    {
        let state = chrome.borrow();
        host.room_activity_mut()
            .set_scope(RoomScope::AllWorkspaces, &*state.tree());
    }
    host.force_room_activity_settled();
    assert!(chrome.borrow().history_summary_binding().is_none());
    assert_eq!(host.room_activity().view(), ActivityView::Explore);
    assert_eq!(chrome.borrow().tree_mut().doc.get_heads(), heads);
}
