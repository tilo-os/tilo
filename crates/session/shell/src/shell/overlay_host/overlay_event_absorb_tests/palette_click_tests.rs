//! A click on search's card goes to the row under the pointer, and a click
//! outside the card only puts search away: nothing on it runs, the
//! highlighted row included.

use super::*;

/// Three rows whose opening is recorded by title.
struct ClickFixture(Rc<RefCell<Vec<String>>>);

impl crate::shell::search::SearchProvider for ClickFixture {
    fn id(&self) -> &'static str {
        "click-fixture"
    }

    fn name(&self) -> &'static str {
        "Click fixture"
    }

    fn resource_kinds(&self) -> &[crate::shell::search::ResourceKind] {
        &[crate::shell::search::ResourceKind::Action]
    }

    fn query(
        &self,
        context: crate::shell::search::SearchContext<'_>,
    ) -> Vec<crate::shell::search::SearchResult> {
        if !"click fixture".contains(context.needle) {
            return Vec::new();
        }
        [("Alpha", 90), ("Beta", 80), ("Gamma", 70)]
            .into_iter()
            .map(|(title, score)| {
                let opened = Rc::clone(&self.0);
                crate::shell::search::SearchResult {
                    facts: Vec::new(),
                    shortcut: None,
                    kind: crate::shell::search::ResourceKind::Action,
                    operation_kind: tilo_actions::OperationKind::View,
                    provider_id: "click-fixture".into(),
                    id: format!("click-fixture:{title}"),
                    title: title.into(),
                    subtitle: None,
                    badge: None,
                    score,
                    params: Vec::new(),
                    contract: Some(tilo_actions::ActionContract::view()),
                    on_open: Rc::new(move |_, _, _, _| {
                        opened.borrow_mut().push(title.to_owned());
                        Ok(())
                    }),
                    on_invoke: None,
                    on_begin_invoke: None,
                }
            })
            .collect()
    }
}

fn search_open() -> (AbsorbHarness, Rc<RefCell<Vec<String>>>) {
    let opened = Rc::new(RefCell::new(Vec::new()));
    let mut h = build_harness();
    h.host
        .search_registry
        .register(Rc::new(ClickFixture(Rc::clone(&opened))));
    h.host.set_palette_visible(true);
    h.host.set_palette_focus(true);
    h.host.force_palette_settled();
    h.host.palette.set_query("click fixture");
    let mut doc = AutoCommit::new();
    h.host
        .refresh_search_results_for_shots(&mut doc, &automerge::ROOT);
    settle(&mut h, 1600.0, 900.0);
    (h, opened)
}

fn row_of(h: &AbsorbHarness, title: &str) -> Rect {
    let index = h
        .host
        .palette
        .visible_entries()
        .iter()
        .position(|entry| entry.label == title)
        .unwrap_or_else(|| panic!("{title} is listed"));
    h.host
        .palette
        .row_rect(index)
        .unwrap_or_else(|| panic!("{title} is painted"))
}

fn click(h: &mut AbsorbHarness, at: Point) {
    pump(h, &cursor_moved_at(at));
    pump(h, &mouse_press());
    pump(h, &mouse_release());
}

#[test]
fn a_click_outside_search_only_puts_it_away() {
    let (mut h, opened) = search_open();
    let card = h.host.palette.get_rect();
    assert!(card.area() > 0.0);
    assert!(
        h.host.palette.selected_id().is_some(),
        "a row is highlighted, which Enter would run"
    );
    // Beside the card, over the desktop.
    let outside = Point::new(card.x0 - 40.0, card.center().y);
    assert!(!card.contains(outside) && outside.x > 0.0);
    click(&mut h, outside);
    assert!(!h.host.palette_visible(), "the click put search away");
    assert!(
        opened.borrow().is_empty(),
        "and ran nothing: {:?}",
        opened.borrow()
    );
}

#[test]
fn a_click_on_a_row_runs_that_row_not_the_highlighted_one() {
    let (mut h, opened) = search_open();
    let highlighted = h.host.palette.selected_id().map(str::to_owned);
    assert_eq!(
        h.host
            .palette
            .visible_entries()
            .iter()
            .find(|entry| Some(entry.id.as_str()) == highlighted.as_deref())
            .map(|entry| entry.label.clone())
            .as_deref(),
        Some("Alpha"),
        "the first row is highlighted"
    );
    let gamma = row_of(&h, "Gamma");
    click(&mut h, gamma.center());
    assert_eq!(*opened.borrow(), ["Gamma"]);
}
