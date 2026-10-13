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

fn summary_actions(
    chrome: &shell::shell::chrome_state::Chrome,
) -> Vec<shell::shell::search::SearchResult> {
    use shell::shell::search::{ResourceKind, SearchContext, SearchProvider as _};
    let doc = automerge::AutoCommit::new();
    shell::shell::search::providers::actions::ActionsProvider::new().query(SearchContext {
        query_scope: None,
        include_programmatic: false,
        needle: "",
        wanted: &[ResourceKind::Action],
        chrome: &chrome.borrow(),
        doc: &doc,
        doc_root: &automerge::ROOT,
    })
}

fn invoke_summary(
    actions: &[shell::shell::search::SearchResult],
    id: &str,
    chrome: &shell::shell::chrome_state::Chrome,
) -> Result<(), shell::shell::search::action_api::ActionError> {
    let action = actions
        .iter()
        .find(|action| action.id == id)
        .expect("published summary action");
    let mut doc = automerge::AutoCommit::new();
    (action.on_open)(
        &mut ui::widget::Edits::lend(&mut doc),
        &automerge::ROOT,
        chrome,
        &shell::shell::search::action_api::ActionArgs::none(),
    )
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

fn assert_summary_text_exported(
    host: &shell::shell::overlay_host::ShellOverlayHost,
    text: &str,
    expected: bool,
) {
    let root = host.accessibility().expect("shell accessibility tree");
    let exported = root.children.iter().any(|window| {
        window.children.iter().any(|node| {
            node.role == ui::a11y::Role::StaticText && node.name.as_deref() == Some(text)
        })
    });
    assert_eq!(exported, expected);
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
    assert_summary_text_exported(&host, &narrative.text, true);
    let path = citation_path(&host, &ids[0]).expect("citation exported by shell host");
    let initial_other_path = citation_path(&host, &ids[1]).expect("second citation exported");
    assert_ne!(path, initial_other_path);
    let heads = chrome.borrow().tree_mut().doc.get_heads();

    let mut replacement = narrative.clone();
    replacement.source_ids.reverse();
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(replacement.clone())
    );
    host.force_room_activity_settled();
    paint(&mut host);
    let reordered_path = citation_path(&host, &ids[0]).expect("reordered citation exported");
    let other_path = citation_path(&host, &ids[1]).expect("second reordered citation exported");
    assert_ne!(reordered_path, path);
    assert!(!host.focus_synthetic_accessibility(&path));
    assert!(host.focus_synthetic_accessibility(&reordered_path));
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
    let surviving_path = citation_path(&host, &ids[1]).expect("surviving citation exported");
    assert_ne!(surviving_path, other_path);
    assert!(!host.focus_synthetic_accessibility(&other_path));
    let other_path = surviving_path;
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

    let (mut host, chrome, narrative) = accessibility_summary_fixture();
    let ids = narrative.source_ids.clone();
    assert_eq!(ids.len(), 2);
    host.room_activity_mut().set_view(ActivityView::CatchUp);
    assert!(
        host.room_activity_mut()
            .apply_generated_narrative(narrative)
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

    // Explore is not a summary presentation. Returning to CatchUp publishes
    // a fresh read capability without rewriting history.
    host.force_room_activity_settled();
    assert!(chrome.borrow().history_summary_binding().is_none());
    host.room_activity_mut().set_view(ActivityView::CatchUp);
    host.force_room_activity_settled();
    let binding = chrome.borrow().history_summary_binding().unwrap().clone();
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
    assert_ne!(binding.action_id(Some(0)), reordered.action_id(Some(1)));
    assert!(reordered.epoch > binding.epoch);
    assert_eq!(reordered.native_owner, binding.native_owner);
    assert!(reordered.native_generation > binding.native_generation);
    chrome
        .borrow_mut()
        .request_history_summary_action(&reordered, Some(1))
        .unwrap();
    host.force_room_activity_settled();
    assert!(host.room_activity().selected_ids().contains(&ids[0]));

    host.room_activity_mut().set_view(ActivityView::CatchUp);
    host.force_room_activity_settled();
    let current = chrome.borrow().history_summary_binding().unwrap().clone();
    chrome
        .borrow_mut()
        .request_history_summary_action(&current, None)
        .unwrap();
    {
        let state = chrome.borrow();
        host.room_activity_mut()
            .set_scope(RoomScope::AllWorkspaces, &*state.tree());
    }
    host.force_room_activity_settled();
    assert!(chrome.borrow().history_summary_binding().is_none());
    // The queued read is revoked: it must not change the current pane mode.
    assert_eq!(host.room_activity().view(), ActivityView::CatchUp);
    assert_eq!(chrome.borrow().tree_mut().doc.get_heads(), heads);
}

#[test]
fn history_summary_cached_native_addresses_and_actions_reject_view_and_window_reentry() {
    use shell::shell::room_activity::ActivityView;
    use shell::shell::search::action_api::ActionError;

    for close_window in [false, true] {
        let (mut host, chrome, narrative) = accessibility_summary_fixture();
        host.room_activity_mut().set_view(ActivityView::CatchUp);
        assert!(
            host.room_activity_mut()
                .apply_generated_narrative(narrative.clone())
        );
        host.force_room_activity_settled();
        // Native accessibility and action discovery do not require painting
        // brief cards. Keep this presentation unviewed so closing it cannot
        // change the evidence through acknowledgement.
        let binding = chrome.borrow().history_summary_binding().unwrap().clone();
        let cached = summary_actions(&chrome);
        let old_path = citation_path(&host, &narrative.source_ids[0]).unwrap();
        let heads = chrome.borrow().tree_mut().doc.get_heads();
        let selection = host.room_activity().selected_ids();

        if close_window {
            let tab =
                shell::shell::tab_binding::active_workspace_activity_tab(&chrome.borrow().tree())
                    .unwrap()
                    .2;
            shell::shell::window_actions::close_tab(&mut chrome.borrow_mut(), tab).unwrap();
            assert_eq!(
                chrome.borrow().tree_mut().doc.get_heads(),
                heads,
                "closing personal Activity changes no evidence"
            );
            // Refuse even before the host gets its next paint: a read cannot
            // reopen the closed window through its cached publication.
            assert!(chrome.borrow().history_summary_binding().is_none());
            for slot in [None, Some(0)] {
                assert!(matches!(
                    invoke_summary(&cached, &binding.action_id(slot), &chrome),
                    Err(ActionError::Refused(_))
                ));
            }
            host.take_structure_dirty();
            layout_host(&mut host);
        } else {
            host.room_activity_mut().set_view(ActivityView::Explore);
            host.force_room_activity_settled();
        }
        assert!(chrome.borrow().history_summary_binding().is_none());
        assert!(!host.focus_synthetic_accessibility(&old_path));
        for slot in [None, Some(0)] {
            assert!(matches!(
                invoke_summary(&cached, &binding.action_id(slot), &chrome),
                Err(ActionError::Refused(_))
            ));
        }
        assert!(
            chrome
                .borrow_mut()
                .take_history_summary_action_request()
                .is_none()
        );
        assert_eq!(host.room_activity().selected_ids(), selection);

        if close_window {
            chrome.borrow_mut().open_room_activity(None);
            host.take_structure_dirty();
            layout_host(&mut host);
        } else {
            host.room_activity_mut().set_view(ActivityView::CatchUp);
        }
        // Retain the pane and exact evidence; no replacement model result
        // and still no painted (therefore viewed) brief cards.
        host.force_room_activity_settled();
        let restored = chrome.borrow().history_summary_binding().unwrap().clone();
        assert_eq!(restored.request, binding.request);
        assert_eq!(restored.narrative, binding.narrative);
        assert_eq!(restored.native_owner, binding.native_owner);
        assert!(restored.native_generation > binding.native_generation);
        assert!(restored.epoch > binding.epoch);
        let new_path = citation_path(&host, &narrative.source_ids[0]).unwrap();
        assert_ne!(new_path, old_path);
        assert!(!host.focus_synthetic_accessibility(&old_path));
        assert!(host.focus_synthetic_accessibility(&new_path));
        let fresh = summary_actions(&chrome);
        for slot in [None, Some(0)] {
            assert!(matches!(
                invoke_summary(&cached, &binding.action_id(slot), &chrome),
                Err(ActionError::Refused(_))
            ));
            invoke_summary(&fresh, &restored.action_id(slot), &chrome).unwrap();
            host.force_room_activity_settled();
            if slot.is_some() {
                assert_eq!(host.room_activity().view(), ActivityView::Explore);
                assert!(
                    host.room_activity()
                        .selected_ids()
                        .contains(&narrative.source_ids[0])
                );
            } else {
                assert_eq!(host.room_activity().view(), ActivityView::CatchUp);
            }
        }
        assert_eq!(chrome.borrow().tree_mut().doc.get_heads(), heads);
    }
}

#[test]
fn history_summary_cached_actions_reject_native_aba_absence_and_pane_replacement() {
    use shell::shell::room_activity::ActivityView;
    use shell::shell::search::action_api::ActionError;

    for transition in ["changed", "absent", "replacement"] {
        let (mut host, chrome, narrative) = accessibility_summary_fixture();
        host.room_activity_mut().set_view(ActivityView::CatchUp);
        assert!(
            host.room_activity_mut()
                .apply_generated_narrative(narrative.clone())
        );
        host.force_room_activity_settled();
        let binding = chrome.borrow().history_summary_binding().unwrap().clone();
        let cached = summary_actions(&chrome);
        let heads = chrome.borrow().tree_mut().doc.get_heads();
        match transition {
            "changed" => {
                let mut other = narrative.clone();
                other.source_ids.reverse();
                assert!(host.room_activity_mut().apply_generated_narrative(other));
                host.force_room_activity_settled();
            }
            "absent" => {
                host.room_activity_mut()
                    .set_narrative_feedback("activity-summary-unavailable");
                host.room_activity_mut()
                    .set_narrative_feedback("activity-summary-pending");
                host.force_room_activity_settled();
                assert!(chrome.borrow().history_summary_binding().is_none());
            }
            "replacement" => {
                *host.room_activity_mut() = RoomActivityPane::new();
                host.room_activity_mut().set_view(ActivityView::Explore);
                host.force_room_activity_settled();
                assert_eq!(
                    host.room_activity().narrative_request(),
                    Some(binding.request.clone())
                );
                host.room_activity_mut().set_view(ActivityView::CatchUp);
            }
            _ => unreachable!(),
        }
        assert!(
            host.room_activity_mut()
                .apply_generated_narrative(narrative.clone())
        );
        host.force_room_activity_settled();
        let restored = chrome.borrow().history_summary_binding().unwrap().clone();
        assert_eq!(restored.request, binding.request);
        assert_eq!(restored.narrative, binding.narrative);
        assert!(restored.epoch > binding.epoch);
        if transition == "replacement" {
            assert_ne!(restored.native_owner, binding.native_owner);
        } else {
            assert_eq!(restored.native_owner, binding.native_owner);
            assert!(restored.native_generation > binding.native_generation);
        }
        let fresh = summary_actions(&chrome);
        for slot in [None, Some(0)] {
            let selection = host.room_activity().selected_ids();
            let view = host.room_activity().view();
            assert!(matches!(
                invoke_summary(&cached, &binding.action_id(slot), &chrome),
                Err(ActionError::Refused(_))
            ));
            assert!(
                chrome
                    .borrow_mut()
                    .take_history_summary_action_request()
                    .is_none()
            );
            host.force_room_activity_settled();
            assert_eq!(host.room_activity().view(), view);
            assert_eq!(host.room_activity().selected_ids(), selection);
            invoke_summary(&fresh, &restored.action_id(slot), &chrome).unwrap();
            host.force_room_activity_settled();
            match slot {
                None => assert_eq!(host.room_activity().view(), ActivityView::CatchUp),
                Some(slot) => {
                    assert_eq!(host.room_activity().view(), ActivityView::Explore);
                    assert!(
                        host.room_activity()
                            .selected_ids()
                            .contains(&narrative.source_ids[slot])
                    );
                }
            }
        }
        assert_eq!(chrome.borrow().tree_mut().doc.get_heads(), heads);
    }
}
