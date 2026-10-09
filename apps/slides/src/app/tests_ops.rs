//! Slide and element edits from the editor's controls and keys: each lands
//! in the deck through its dispatcher, and the editor follows the deck.

use super::bench::{Bench, deck_of};
use super::*;

fn key(bench: &mut Bench, key: NamedKey) -> bool {
    bench.with(|app| {
        let mut handled = false;
        app.on_key(&Key::Named(key), None, Modifiers::default(), &mut handled);
        handled
    })
}

/// New, duplicate, move and delete slide land in the deck, and the editor
/// shows the slide each one leaves current.
#[test]
fn slide_ops_update_the_deck_and_the_current_slide() {
    let mut bench = Bench::sample();
    bench.with(|app| app.cur_slide = 1);
    let n0 = bench.deck().slides.len();

    bench.with(|app| app.apply_hit(Hit::NewSlide));
    assert_eq!(bench.deck().slides.len(), n0 + 1);
    assert_eq!(
        bench.read(|app| app.cur_slide),
        2,
        "the new slide becomes current"
    );

    bench.with(|app| app.apply_hit(Hit::DuplicateSlide));
    assert_eq!(bench.deck().slides.len(), n0 + 2);
    assert_eq!(bench.read(|app| app.cur_slide), 3);

    // Moving up and down (the rail menu's reorder) reorders by rank.
    let moved = bench.deck().slides[3].id.clone();
    bench.with(|app| app.apply_rail_event(PageSidebarEvent::Reorder { from: 3, to: 2 }));
    assert_eq!(bench.read(|app| app.cur_slide), 2);
    assert_eq!(
        bench.deck().slides[2].id,
        moved,
        "move up reordered the deck"
    );
    bench.with(|app| app.apply_rail_event(PageSidebarEvent::Reorder { from: 2, to: 3 }));
    assert_eq!(bench.read(|app| app.cur_slide), 3);
    assert_eq!(
        bench.deck().slides[3].id,
        moved,
        "move down reordered it back"
    );

    // Deleting is destructive: it waits for the person's confirmation, as a
    // declared delete does, and changes nothing until then.
    bench.with(|app| app.apply_hit(Hit::DeleteSlide));
    assert_eq!(bench.deck().slides.len(), n0 + 2, "held for confirmation");
    let current = bench.deck().slides[2].id.clone();
    bench
        .app
        .invoke_confirmed(
            ui::tilo_resource::Invoke::apply(&actions::delete_slide_action(&current)).unwrap(),
        )
        .unwrap();
    assert_eq!(bench.deck().slides.len(), n0 + 1);
    assert!(bench.undo(), "a delete is an undo step");
    assert_eq!(bench.deck().slides.len(), n0 + 2);
}

/// The last slide stays.
#[test]
fn the_last_slide_cannot_be_deleted() {
    let mut bench = Bench::one_slide(Slide::new(Layout::Blank));
    bench.with(|app| app.apply_hit(Hit::DeleteSlide));
    assert_eq!(bench.deck().slides.len(), 1);
}

/// An arrow nudges the selection, Tab cycles it and Delete removes it.
#[test]
fn arrow_nudge_tab_cycle_and_delete_edit_the_deck() {
    let mut slide = Slide::new(Layout::Blank);
    let a = slide.add(Element::shape(
        ElementKind::Rect,
        200.0,
        200.0,
        100.0,
        100.0,
    ));
    let b = slide.add(Element::shape(
        ElementKind::Ellipse,
        500.0,
        400.0,
        100.0,
        100.0,
    ));
    let mut bench = Bench::open(&deck_of(vec![slide]), super::bench::WINDOW);
    bench.with(|app| app.sel = Some(a.clone()));

    let x0 = bench.deck().slides[0].element(&a).unwrap().x;
    assert!(key(&mut bench, NamedKey::ArrowRight));
    let x1 = bench.deck().slides[0].element(&a).unwrap().x;
    assert!(x1 > x0 + 1.0, "ArrowRight nudged x ({x0} → {x1})");

    assert!(key(&mut bench, NamedKey::Tab));
    assert_eq!(bench.read(|app| app.sel.clone()), Some(b), "Tab cycles");

    // Delete removes the selection: destructive, so held for the person's
    // confirmation.
    let n = bench.deck().slides[0].elements.len();
    assert!(key(&mut bench, NamedKey::Delete));
    assert_eq!(
        bench.deck().slides[0].elements.len(),
        n,
        "held for confirmation"
    );
    assert!(
        bench.read(|app| app.sel.is_none()),
        "selection clears after delete"
    );
}

/// Clicking an overview card opens that slide in the editor.
#[test]
fn clicking_an_overview_card_opens_that_slide() {
    let mut bench = Bench::sample();
    bench.with(|app| app.mode = Mode::Overview);
    let (index, card) = bench.read(|app| app.compute_overview().1[2]);
    bench.click(card.center());
    assert_eq!(bench.read(|app| app.mode), Mode::Edit);
    assert_eq!(bench.read(|app| app.cur_slide), index);
}

/// A viewer's edits change nothing.
#[test]
fn a_viewer_cannot_edit_the_deck() {
    let mut bench = Bench::open_as(
        &crate::model::seed_deck(),
        super::bench::WINDOW,
        ui::permissions::Role::Viewer,
    );
    let before = bench.deck();
    bench.with(|app| app.apply_hit(Hit::NewSlide));
    bench.with(|app| app.apply_hit(Hit::DeleteSlide));
    assert_eq!(bench.deck(), before);
    assert_eq!(bench.read(|app| app.deck.slides.len()), before.slides.len());
}

/// The selected element's delete and restack are published on the deck for
/// the palette and agents, bound to the selection, and gone without one.
#[test]
fn the_selection_publishes_its_bound_edits() {
    let mut slide = Slide::new(Layout::Blank);
    let a = slide.add(Element::shape(
        ElementKind::Rect,
        200.0,
        200.0,
        100.0,
        100.0,
    ));
    let b = slide.add(Element::shape(
        ElementKind::Ellipse,
        500.0,
        400.0,
        100.0,
        100.0,
    ));
    let mut bench = Bench::open(&deck_of(vec![slide]), super::bench::WINDOW);
    let published = |bench: &mut Bench| -> Vec<String> {
        bench
            .app
            .agent_surface()
            .as_array()
            .unwrap()
            .iter()
            .filter(|row| row["resource"] == "slides.deck")
            .filter_map(|row| row["action"].as_str().map(str::to_owned))
            .collect()
    };
    assert!(
        !published(&mut bench)
            .iter()
            .any(|id| id.contains("selection")),
        "nothing selected, nothing bound"
    );

    bench.with(|app| app.sel = Some(a.clone()));
    let ids = published(&mut bench);
    for id in [
        "delete-selection",
        "bring-selection-front",
        "send-selection-back",
    ] {
        assert!(
            ids.iter().any(|published| published == id),
            "`{id}` in {ids:?}"
        );
    }
    let instance_id = bench.app.harness().component_instance_id().unwrap();
    bench
        .app
        .harness()
        .invoke_bound_action(
            &ui::bound_dispatch::BoundActionAddress {
                instance_id,
                address: tilo_actions::ActionAddress::new("slides.deck", "bring-selection-front"),
            },
            &serde_json::Map::new(),
        )
        .expect("bring to front applies");
    bench.frame();
    let deck = bench.deck();
    let (za, zb) = (
        deck.slides[0].element(&a).unwrap().z,
        deck.slides[0].element(&b).unwrap().z,
    );
    assert!(
        za > zb,
        "the selected element came to the front ({za} vs {zb})"
    );
}
