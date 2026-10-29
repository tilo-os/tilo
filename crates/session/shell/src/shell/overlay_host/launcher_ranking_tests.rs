use super::*;
use std::cell::Cell;

struct RankingProvider {
    revision: Rc<Cell<u64>>,
    invoked: Rc<RefCell<Vec<String>>>,
}

impl crate::shell::search::SearchProvider for RankingProvider {
    fn id(&self) -> &'static str {
        "launcher-ranking-fixture"
    }
    fn name(&self) -> &'static str {
        "Applications"
    }
    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Application]
    }
    fn search_revision(&self, _: &crate::shell::search::SearchContext<'_>) -> String {
        self.revision.get().to_string()
    }
    fn query(
        &self,
        _: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        let revision = self.revision.get();
        let rows = if revision == 0 {
            vec![("alpha", 90), ("beta", 80)]
        } else if revision == 1 {
            vec![("gamma", 100), ("alpha", 90), ("beta", 80)]
        } else {
            vec![("gamma", 100), ("alpha", 90)]
        };
        rows.into_iter()
            .map(|(id, score)| {
                let invoked = self.invoked.clone();
                crate::shell::search::SearchResult::nullary(
                    crate::shell::search::ResourceKind::Application,
                    self.id(),
                    id,
                    format!("Ranking fixture {id}"),
                    Some(format!("Available application · revision {revision}")),
                    score,
                    move |_, _, _| {
                        invoked.borrow_mut().push(format!("{id}:{revision}"));
                    },
                )
            })
            .collect()
    }
}

struct RankingHarness {
    h: AbsorbHarness,
    revision: Rc<Cell<u64>>,
    invoked: Rc<RefCell<Vec<String>>>,
}

/// `ArrowDown` in the launcher: the next row is the person's choice, and the
/// launcher says so.
fn select_next(h: &mut AbsorbHarness) {
    let next = h.host.search.cursor().saturating_add(1);
    let row = h.host.search.visible_entries()[next].id.clone();
    h.host
        .ask_as_launcher(launcher_app::source::Ask::Select { row });
}

/// Enter in the launcher.
fn enter(h: &mut AbsorbHarness) {
    h.host.choose_selected_as_launcher(None);
}

fn ranking_fixture() -> RankingHarness {
    let mut f = untouched_ranking_fixture();
    select_next(&mut f.h);
    assert_eq!(selected_label(&f.h), "Ranking fixture beta");
    f
}

/// The palette after typing, with the selection where the search put it.
fn untouched_ranking_fixture() -> RankingHarness {
    let mut h = build_harness_sized(1280.0, 800.0);
    let revision = Rc::new(Cell::new(0));
    let invoked = Rc::new(RefCell::new(Vec::new()));
    h.host.search_registry.register(Rc::new(RankingProvider {
        revision: revision.clone(),
        invoked: invoked.clone(),
    }));
    h.host.set_palette_visible(true);
    h.host.force_palette_settled();
    h.host.set_palette_query("ranking fixture");
    h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut h, 1280.0, 800.0);
    assert_eq!(selected_label(&h), "Ranking fixture alpha");
    RankingHarness {
        h,
        revision,
        invoked,
    }
}

fn selected_label(h: &AbsorbHarness) -> String {
    let selected = h.host.search.selected_id().expect("a selected result");
    h.host
        .search
        .visible_entries()
        .into_iter()
        .find(|entry| entry.id == selected)
        .expect("selected entry must exist")
        .label
        .clone()
}

#[test]
fn provider_revision_keeps_keyboard_choice_and_invokes_the_fresh_callback() {
    let mut f = ranking_fixture();
    f.revision.set(1);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut f.h, 1280.0, 800.0);
    assert_eq!(
        selected_label(&f.h),
        "Ranking fixture beta",
        "a new top result must not steal the user's choice"
    );
    assert_eq!(f.h.host.search.query(), "ranking fixture");
    assert!(f.h.host.palette_visible);
    enter(&mut f.h);
    assert_eq!(
        &*f.invoked.borrow(),
        &["beta:1"],
        "identity survives, but the callback must come from the current index"
    );
    assert!(!f.h.host.palette_visible());
}

#[test]
fn a_slower_providers_better_result_takes_a_selection_nobody_moved() {
    let mut f = untouched_ranking_fixture();
    // A slower provider answers after typing with a better match.
    f.revision.set(1);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    settle(&mut f.h, 1280.0, 800.0);
    assert_eq!(
        selected_label(&f.h),
        "Ranking fixture gamma",
        "Enter runs the best result, not the one that was best before it arrived"
    );
    enter(&mut f.h);
    assert_eq!(&*f.invoked.borrow(), &["gamma:1"]);
}

#[test]
fn query_correction_and_removed_results_use_the_current_visible_list() {
    let mut f = ranking_fixture();
    f.revision.set(1);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    assert_eq!(selected_label(&f.h), "Ranking fixture beta");
    f.h.host.ask_as_launcher(launcher_app::source::Ask::Search(
        launcher_app::source::Search {
            query: "ranking fixtur".into(),
            kinds: Vec::new(),
        },
    ));
    assert_eq!(f.h.host.search.query(), "ranking fixtur");
    assert_eq!(
        selected_label(&f.h),
        "Ranking fixture gamma",
        "a new query starts at its own best match"
    );
    assert!(pump(&mut f.h, &key_pressed(Key::Named(NamedKey::Escape))));
    assert!(f.invoked.borrow().is_empty());

    f.h.host.set_palette_visible(true);
    f.h.host.set_palette_query("ranking fixture");
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    let beta =
        f.h.host
            .search
            .visible_entries()
            .iter()
            .position(|entry| entry.label == "Ranking fixture beta")
            .unwrap();
    let row = f.h.host.search.visible_entries()[beta].id.clone();
    f.h.host
        .ask_as_launcher(launcher_app::source::Ask::Select { row });
    f.revision.set(2);
    f.h.host
        .refresh_search_results(&AutoCommit::new(), &automerge::ROOT);
    assert!(
        !f.h.host
            .search
            .visible_entries()
            .iter()
            .any(|entry| entry.label == "Ranking fixture beta")
    );
    assert_eq!(selected_label(&f.h), "Ranking fixture gamma");
    enter(&mut f.h);
    assert_eq!(
        &*f.invoked.borrow(),
        &["gamma:2"],
        "a vanished result's old callback must never run"
    );
}
