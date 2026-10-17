//! Human job rows read the same authorized invocation state as status actions.
use super::{LiveAppActionsProvider, job_status_id};
use crate::shell::chrome_state::ChromeState;
use crate::shell::search::{ResourceKind, SearchBudget, SearchContext, SearchProvider};
use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};
use automerge::AutoCommit;
use tilo_actions::{InvocationId, InvocationUpdate};

fn chrome() -> crate::shell::chrome_state::Chrome {
    let mut tree = WorkspaceTree::default();
    tree.desktop
        .workspaces
        .push(Workspace::new(WorkspaceId(1), "Job progress fixture"));
    tree.desktop.active = WorkspaceId(1);
    ChromeState::new(tree)
}

fn context<'a>(chrome: &'a ChromeState, doc: &'a AutoCommit) -> SearchContext<'a> {
    SearchContext {
        query_scope: None,
        include_programmatic: false,
        needle: "jobs",
        wanted: &[ResourceKind::Action],
        chrome,
        doc,
        doc_root: &automerge::ROOT,
    }
}

fn progress(id: u64, phase: &str, completed: Option<u64>, total: Option<u64>) -> InvocationUpdate {
    InvocationUpdate::Progress {
        invocation: InvocationId(uuid::Uuid::from_u128(u128::from(id))),
        sequence: 1,
        phase: phase.into(),
        completed,
        total,
        message: Some("private command diagnostic must not appear in the row".into()),
    }
}

#[test]
fn job_rows_show_reported_phase_counts_through_all_search_paths() {
    let chrome = chrome();
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
    let doc = AutoCommit::new();
    for (completed, total, expected) in [
        (Some(4), Some(10), "Reported: Routing (4 / 10)"),
        (Some(0), Some(0), "Reported: Routing (0 / 0)"),
        (
            Some(u64::MAX),
            Some(u64::MAX),
            "Reported: Routing (18446744073709551615 / 18446744073709551615)",
        ),
        (Some(4), None, "Reported: Routing (4 completed)"),
        (None, Some(10), "Reported: Routing (total 10)"),
        (None, None, "Reported: Routing"),
    ] {
        let update = progress(17, "Routing", completed, total);
        broker.record_completion_for_test(17, &actor, update.clone());
        let state = chrome.borrow();
        let row = provider
            .resolve(&job_status_id(17), context(&state, &doc))
            .unwrap();
        assert_eq!(row.subtitle.as_deref(), Some(expected));
        assert_eq!(row.id, job_status_id(17));
        let queried = provider.query(context(&state, &doc));
        assert_eq!(
            queried
                .iter()
                .find(|row| row.id == job_status_id(17))
                .unwrap()
                .subtitle
                .as_deref(),
            Some(expected)
        );
        let page = provider.query_page(context(&state, &doc), None, SearchBudget::PALETTE);
        assert_eq!(
            page.results
                .iter()
                .find(|row| row.id == job_status_id(17))
                .unwrap()
                .subtitle
                .as_deref(),
            Some(expected)
        );
        assert_eq!(broker.status_for_actor(17, &actor), Some(update));
    }
    drop(provider);
    drop(broker);
}

#[test]
fn job_rows_bound_reported_phase_without_displaying_command_messages() {
    let chrome = chrome();
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
    let doc = AutoCommit::new();
    let phase = format!("{}\u{7}", "é".repeat(200));
    broker.record_completion_for_test(17, &actor, progress(17, &phase, None, None));
    let state = chrome.borrow();
    let row = provider
        .resolve(&job_status_id(17), context(&state, &doc))
        .unwrap();
    assert_eq!(row.subtitle, Some(format!("Reported: {}", "é".repeat(128))));
    broker.record_completion_for_test(17, &actor, progress(17, "Rout\n\t\u{7}ing", None, None));
    let row = provider
        .resolve(&job_status_id(17), context(&state, &doc))
        .unwrap();
    assert_eq!(row.subtitle.as_deref(), Some("Reported: Routing"));
    broker.record_completion_for_test(
        17,
        &actor,
        progress(17, &format!("{}Routing", "\0".repeat(128)), None, None),
    );
    let row = provider
        .resolve(&job_status_id(17), context(&state, &doc))
        .unwrap();
    assert_eq!(
        row.subtitle.as_deref(),
        Some("running"),
        "only the bounded prefix is inspected"
    );
    broker.record_completion_for_test(17, &actor, progress(17, "\n\t\u{7} ", None, None));
    drop(broker);
    let row = provider
        .resolve(&job_status_id(17), context(&state, &doc))
        .unwrap();
    drop(provider);
    assert_eq!(row.subtitle.as_deref(), Some("running"));
}

#[test]
fn job_rows_replace_progress_on_terminal_and_refuse_foreign_actor() {
    let chrome = chrome();
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
    let doc = AutoCommit::new();
    broker.record_completion_for_test(17, &actor, progress(17, "Routing", Some(4), Some(10)));
    broker.record_completion_for_test(
        18,
        "someone-else",
        progress(18, "Foreign private phase", None, None),
    );
    let state = chrome.borrow();
    assert!(
        provider
            .resolve(&job_status_id(18), context(&state, &doc))
            .is_none()
    );
    for row in provider.query(context(&state, &doc)) {
        assert_ne!(row.id, job_status_id(18));
    }
    let page = provider.query_page(context(&state, &doc), None, SearchBudget::PALETTE);
    assert!(page.results.iter().all(|row| row.id != job_status_id(18)));
    let before = provider.search_revision(&context(&state, &doc));
    let invocation = InvocationId(uuid::Uuid::from_u128(17));
    for (update, expected) in [
        (
            InvocationUpdate::Succeeded {
                invocation,
                output: tilo_actions::ActionOutput::default(),
            },
            "succeeded",
        ),
        (
            InvocationUpdate::Cancelled {
                invocation,
                message: Some("private cancellation".into()),
            },
            "cancelled",
        ),
        (
            InvocationUpdate::Failed {
                invocation,
                error: tilo_actions::ActionFailure {
                    code: "failed".into(),
                    message: "private failure".into(),
                    details: None,
                    retryable: false,
                },
            },
            "failed",
        ),
    ] {
        broker.record_completion_for_test(17, &actor, update);
        assert_ne!(provider.search_revision(&context(&state, &doc)), before);
        let row = provider
            .resolve(&job_status_id(17), context(&state, &doc))
            .unwrap();
        assert_eq!(row.subtitle.as_deref(), Some(expected));
        let page = provider.query_page(context(&state, &doc), None, SearchBudget::PALETTE);
        assert_eq!(
            page.results
                .iter()
                .find(|row| row.id == job_status_id(17))
                .unwrap()
                .subtitle
                .as_deref(),
            Some(expected)
        );
        assert!(
            provider
                .resolve(&super::job_cancel_id(17), context(&state, &doc))
                .is_none()
        );
    }
    drop(provider);
    drop(broker);
}

#[test]
fn job_rows_keep_progress_across_bounded_pages_and_account_for_utf8_bytes() {
    let chrome = chrome();
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
    let doc = AutoCommit::new();
    for id in [16, 17] {
        broker.record_completion_for_test(id, &actor, progress(id, &"é".repeat(200), None, None));
    }
    drop(broker);
    let state = chrome.borrow();
    let mut ids = Vec::new();
    let mut cursor = None;
    let budget = SearchBudget {
        max_results: 1,
        ..SearchBudget::PALETTE
    };
    for _ in 0..4 {
        let page = provider.query_page(context(&state, &doc), cursor.as_ref(), budget);
        assert!(page.results.len() <= 1);
        for row in page.results {
            assert_eq!(row.subtitle, Some(format!("Reported: {}", "é".repeat(128))));
            ids.push(row.id);
        }
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert!(cursor.is_none(), "the bounded job pages must terminate");
    assert_eq!(ids, vec![job_status_id(16), job_status_id(17)]);
    let narrow = SearchBudget {
        max_bytes: 128,
        ..SearchBudget::PALETTE
    };
    let page = provider.query_page(context(&state, &doc), None, narrow);
    drop(provider);
    assert!(
        page.results.is_empty(),
        "UTF-8 phase bytes count against the existing row budget"
    );
}

#[test]
fn uncertified_job_rows_remain_available_without_a_certified_session() {
    let chrome = chrome();
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new(broker.clone());
    broker.record_completion_for_test(17, &actor, progress(17, "Routing", None, None));
    let doc = AutoCommit::new();
    let state = chrome.borrow();
    let row = provider
        .resolve(&job_status_id(17), context(&state, &doc))
        .unwrap();
    assert_eq!(row.id, job_status_id(17));
    assert_eq!(row.subtitle.as_deref(), Some("Reported: Routing"));
}

#[test]
fn job_rows_remain_unique_with_multiple_workspace_sessions() {
    let chrome = chrome();
    chrome
        .borrow()
        .tree_mut()
        .desktop
        .workspaces
        .push(Workspace::new(WorkspaceId(2), "Another job workspace"));
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
    for id in [17, 16] {
        broker.record_completion_for_test(id, &actor, progress(id, "Routing", None, None));
    }
    let doc = AutoCommit::new();
    let state = chrome.borrow();
    let expected = vec![job_status_id(16), job_status_id(17)];
    let queried = provider.query(context(&state, &doc));
    assert_eq!(
        queried.into_iter().map(|row| row.id).collect::<Vec<_>>(),
        expected
    );
    let page = provider.query_page(context(&state, &doc), None, SearchBudget::PALETTE);
    assert_eq!(
        page.results
            .into_iter()
            .map(|row| row.id)
            .collect::<Vec<_>>(),
        expected
    );
}

#[test]
fn job_rows_refresh_reported_progress_when_the_desktop_language_changes() {
    let chrome = chrome();
    let actor = chrome.borrow().active_local_user();
    let broker =
        crate::shell::collab_broker_capnp::CapnpBrokerState::test_broker_with_idle_directory_queue(
        );
    let provider = LiveAppActionsProvider::new_with_fixture_identity(broker.clone());
    let doc = AutoCommit::new();
    broker.record_completion_for_test(17, &actor, progress(17, "Routing", Some(4), Some(10)));
    drop(broker);
    let state = chrome.borrow();
    let budget = SearchBudget {
        max_results: 1,
        ..SearchBudget::PALETTE
    };
    let english = provider.query_page(context(&state, &doc), None, budget);
    assert_eq!(
        english.results[0].subtitle.as_deref(),
        Some("Reported: Routing (4 / 10)")
    );
    let cursor = english
        .next_cursor
        .expect("the one-row page retains a continuation");
    ui::i18n::I18nManager::set_locale("de-DE");
    let previous_language_page = provider.query_page(context(&state, &doc), Some(&cursor), budget);
    let german = provider.query_page(context(&state, &doc), None, budget);
    drop(provider);
    ui::i18n::I18nManager::set_locale("en-US");
    assert!(
        previous_language_page.stale_cursor,
        "a cursor from the previous desktop language is stale"
    );
    assert_eq!(
        german.results[0].subtitle.as_deref(),
        Some("Gemeldet: Routing (4 / 10)")
    );
    assert_eq!(german.results[0].id, english.results[0].id);
}
