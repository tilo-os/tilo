use super::*;
use std::cell::Cell;

struct RecoveryProvider {
    attempts: Rc<Cell<usize>>,
    failing: Rc<Cell<bool>>,
}

impl crate::shell::search::SearchProvider for RecoveryProvider {
    fn id(&self) -> &'static str {
        "launcher-recovery-fixture"
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn query(
        &self,
        context: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        if !context.needle.is_empty() && !"recovery fixture".contains(context.needle) {
            return Vec::new();
        }
        let attempts = self.attempts.clone();
        let failing = self.failing.clone();
        vec![crate::shell::search::SearchResult::nullary_fallible(
            crate::shell::search::ResourceKind::Application,
            self.id(),
            "recovery",
            "Open Recovery Fixture",
            Some("A local fixture application".into()),
            100,
            move |_, _, _| {
                attempts.set(
                    attempts
                        .get()
                        .checked_add(1)
                        .expect("bounded test attempts"),
                );
                if failing.get() {
                    Err(crate::shell::search::action_api::ActionError::Explained(
                        "The application is temporarily unavailable. Try again.".into(),
                    ))
                } else {
                    Ok(())
                }
            },
        )]
    }
}

fn recovery_fixture() -> (AbsorbHarness, Rc<Cell<usize>>, Rc<Cell<bool>>) {
    let mut h = build_harness_sized(1280.0, 800.0);
    let attempts = Rc::new(Cell::new(0));
    let failing = Rc::new(Cell::new(true));
    h.host.search_registry.register(Rc::new(RecoveryProvider {
        attempts: attempts.clone(),
        failing: failing.clone(),
    }));
    h.host.set_palette_visible(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("recovery fixture");
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    let rows = h.host.search.visible_entries();
    let index = rows
        .iter()
        .position(|entry| entry.label == "Open Recovery Fixture")
        .expect("production registry must supply the fixture");
    h.host.search.set_cursor(index);
    (h, attempts, failing)
}

fn assert_recovery_context(h: &AbsorbHarness) {
    assert!(
        h.host.palette_visible(),
        "failed invocation must keep search open"
    );
    assert!(h.host.palette_visible, "search retains keyboard focus");
    assert_eq!(h.host.search.query(), "recovery fixture");
    let selected = h
        .host
        .search
        .visible_entries()
        .get(h.host.search.cursor())
        .copied()
        .unwrap();
    assert_eq!(selected.label, "Open Recovery Fixture");
    assert_eq!(selected.badge.as_deref(), Some("Couldn't run"));
    assert!(
        selected
            .description
            .as_deref()
            .unwrap()
            .contains("temporarily unavailable")
    );
}

/// Enter in the launcher: the selected row is chosen.
fn enter(h: &mut AbsorbHarness) {
    h.host.choose_selected_as_launcher(None);
}

#[test]
fn a_failed_run_keeps_its_context_for_retry_and_escape() {
    let (mut h, attempts, failing) = recovery_fixture();
    enter(&mut h);
    assert_eq!(attempts.get(), 1);
    assert_recovery_context(&h);
    enter(&mut h);
    assert_eq!(attempts.get(), 2);
    assert_recovery_context(&h);
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(
        h.host.palette_visible(),
        "the first Escape clears the query, keeping search open"
    );
    assert_eq!(h.host.palette_query(), "");
    assert!(pump(&mut h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(!h.host.palette_visible());
    assert_eq!(attempts.get(), 2, "cancel must not retry");
    h.host.set_palette_visible(true);
    h.host.set_palette_query("recovery fixture");
    h.host.last_search_query.clear();
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    failing.set(false);
    enter(&mut h);
    assert_eq!(attempts.get(), 3);
    assert!(!h.host.palette_visible(), "successful retry closes search");
}

#[test]
fn a_failed_run_keeps_its_context_across_resize_until_the_query_changes() {
    let (mut h, attempts, failing) = recovery_fixture();
    enter(&mut h);
    assert_eq!(attempts.get(), 1);
    assert_recovery_context(&h);
    settle(&mut h, 800.0, 600.0);
    assert_recovery_context(&h);
    // A query edit rebuilds provider rows, replacing old feedback rather
    // than carrying a failure to another result.
    h.host.ask_as_launcher(launcher_app::source::Ask::Search(
        launcher_app::source::Search {
            query: "recovery fixtur".into(),
            kinds: Vec::new(),
        },
    ));
    assert_eq!(h.host.search.query(), "recovery fixtur");
    assert!(
        h.host
            .search
            .visible_entries()
            .iter()
            .all(|entry| entry.badge.is_none())
    );
    failing.set(false);
    enter(&mut h);
    assert_eq!(attempts.get(), 2);
    assert!(!h.host.palette_visible());
    assert!(
        h.log.borrow().is_empty(),
        "search's input must not reach app content"
    );
}

struct OwnedNoticeProvider {
    progress: bool,
}

impl crate::shell::search::SearchProvider for OwnedNoticeProvider {
    fn id(&self) -> &'static str {
        "owned-notice-fixture"
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn query(
        &self,
        _: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        let progress = self.progress;
        vec![crate::shell::search::SearchResult::nullary_fallible(
            crate::shell::search::ResourceKind::Application,
            self.id(),
            "owned-notice",
            "Open Notice Fixture",
            None,
            100,
            move |_, _, chrome| {
                if progress {
                    chrome
                        .borrow_mut()
                        .push_notification("Opening Notice Fixture", "Preparing…");
                } else {
                    chrome.borrow_mut().push_error_notification(
                        "Couldn't open Notice Fixture",
                        "The application is unavailable. Open Applications and try again.",
                    );
                }
                Err(crate::shell::search::action_api::ActionError::Refused(
                    "The application is unavailable; backend detail is for diagnostics".into(),
                ))
            },
        )]
    }
}

#[test]
fn palette_reuses_callback_owned_recovery_notice_without_a_duplicate() {
    let mut h = build_harness_sized(1280.0, 800.0);
    h.host
        .search_registry
        .register(Rc::new(OwnedNoticeProvider { progress: false }));
    h.host.set_palette_visible(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("notice fixture");
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    h.host.choose_selected_as_launcher(None);
    assert!(h.host.palette_visible());
    let chrome = h.chrome.borrow();
    let notices = chrome.notifications();
    assert_eq!(
        notices.len(),
        1,
        "one refusal has one callback-owned recovery card"
    );
    let entry = h.host.search.visible_entries().first().copied().unwrap();
    assert_eq!(
        entry.description.as_deref(),
        Some(notices.first().unwrap().body.as_str())
    );
    assert!(
        !entry
            .description
            .as_deref()
            .unwrap()
            .contains("backend detail")
    );
}

#[test]
fn palette_progress_notice_does_not_mask_a_failed_activation() {
    let mut h = build_harness_sized(1280.0, 800.0);
    h.host
        .search_registry
        .register(Rc::new(OwnedNoticeProvider { progress: true }));
    h.host.set_palette_visible(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("notice fixture");
    h.host
        .refresh_search_results_for_shots(&mut AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    h.host.choose_selected_as_launcher(None);
    assert!(h.host.palette_visible());
    let entry = h.host.search.visible_entries().first().copied().unwrap();
    // A refusal's diagnostic detail is for logs and agents; the row says
    // what the person can do.
    assert_eq!(
        entry.description.as_deref(),
        Some(crate::shell::i18n::t("host-action-try-again").as_str())
    );
    assert!(!entry.description.as_deref().unwrap().contains("Preparing"));
    // The failure is told once, on the row that failed (above): the
    // progress card stays the only notice, and never stands in for it.
    assert_eq!(
        h.chrome.borrow().notifications().len(),
        1,
        "the palette posts no second notice for a failure its row tells"
    );
}
