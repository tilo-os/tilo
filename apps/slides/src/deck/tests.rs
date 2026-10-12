//! The deck on its own: its verbs and field edits through the same door the
//! host uses, and merging.

#![allow(
    clippy::float_cmp,
    reason = "every compared number is a literal the test itself passed in, stored unchanged"
)]

use super::*;
use tilo_document::action::RegionKind;
use ui::tilo_resource::{Edit, Invoke, Replica};

fn apply(deck: &mut Replica<Deck>, invoke: Invoke<Deck>) -> Result<(), String> {
    deck.apply(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

/// A destructive verb, confirmed.
fn confirmed(deck: &mut Replica<Deck>, invoke: Invoke<Deck>) -> Result<(), String> {
    deck.apply_confirmed(invoke)
        .map(drop)
        .map_err(|error| error.to_string())
}

fn edit(deck: &mut Replica<Deck>, edit: &Edit<Deck>) -> Result<(), String> {
    apply(deck, Invoke::edit(edit).map_err(|error| error.to_string())?)
}

fn state(deck: &Replica<Deck>) -> Deck {
    deck.state().unwrap()
}

fn slide_ids(deck: &Replica<Deck>) -> Vec<Id> {
    state(deck).slides.ids().cloned().collect()
}

fn element_ids(deck: &Replica<Deck>, slide: &Id) -> Vec<Id> {
    state(deck)
        .slides
        .get(slide)
        .unwrap()
        .elements
        .ids()
        .cloned()
        .collect()
}

/// The words of every text box of `slide`, back to front.
fn words(deck: &Replica<Deck>, slide: &Id) -> Vec<String> {
    state(deck)
        .slides
        .get(slide)
        .unwrap()
        .elements
        .values()
        .filter(|element| element.kind == ElementKind::Text)
        .map(|element| {
            blocks_of(&element.body)
                .into_iter()
                .map(|block| block.text)
                .collect::<Vec<_>>()
                .join("\n")
        })
        .collect()
}

fn add_slide(deck: &mut Replica<Deck>, title: &str, body: &str) -> Id {
    let before = slide_ids(deck);
    apply(
        deck,
        Deck::ADD_SLIDE.with(deck::AddSlide {
            title: Some(title.into()),
            body: Some(body.into()),
            layout: None,
            after: None,
        }),
    )
    .unwrap();
    slide_ids(deck)
        .into_iter()
        .find(|id| !before.contains(id))
        .expect("a new slide")
}

/// Every device begins with the same starter deck, so two people opening a
/// new deck before they sync have one first slide, not two.
#[test]
fn every_new_deck_begins_the_same() {
    let (left, right) = (Replica::<Deck>::new(), Replica::<Deck>::new());
    let shown = |deck: &Replica<Deck>| {
        let state = state(deck);
        let ids: Vec<Vec<Id>> = state
            .slides
            .values()
            .map(|slide| slide.elements.ids().cloned().collect())
            .collect();
        (slide_ids(deck), ids, words(deck, &slide_ids(deck)[0]))
    };
    assert_eq!(shown(&left), shown(&right));
    let starter = crate::model::starter_deck();
    assert_eq!(state(&left).slides.len(), starter.slides.len());
    assert_eq!(state(&left).title, starter.title);
    assert_eq!(
        words(&left, &slide_ids(&left)[0]),
        starter.slides[0]
            .elements
            .iter()
            .filter(|element| element.kind == ElementKind::Text)
            .map(|element| element.text.clone())
            .collect::<Vec<_>>()
    );
}

/// A slide is added with its words, copied with them under new ids, moved
/// and deleted; the deck keeps one slide at least.
#[test]
fn slides_are_added_copied_moved_and_deleted() {
    let mut deck = Replica::<Deck>::new();
    let first = slide_ids(&deck)[0].clone();
    let agenda = add_slide(&mut deck, "Agenda", "One\nTwo");
    assert_eq!(slide_ids(&deck), [first.clone(), agenda.clone()]);
    assert_eq!(words(&deck, &agenda), ["Agenda", "One\nTwo"]);

    apply(
        &mut deck,
        Deck::DUPLICATE_SLIDE.with(deck::DuplicateSlide {
            slide: agenda.clone(),
        }),
    )
    .unwrap();
    let ids = slide_ids(&deck);
    assert_eq!(ids.len(), 3);
    let copy = ids[2].clone();
    assert_ne!(copy, agenda);
    assert_eq!(words(&deck, &copy), words(&deck, &agenda));
    let originals = element_ids(&deck, &agenda);
    assert!(
        element_ids(&deck, &copy)
            .iter()
            .all(|id| !originals.contains(id)),
        "a copy's elements are its own"
    );

    apply(
        &mut deck,
        Deck::MOVE_SLIDE.with(deck::MoveSlide {
            slide: copy.clone(),
            position: 0,
        }),
    )
    .unwrap();
    assert_eq!(
        slide_ids(&deck),
        [copy.clone(), first.clone(), agenda.clone()]
    );
    assert!(
        apply(
            &mut deck,
            Deck::MOVE_SLIDE.with(deck::MoveSlide {
                slide: copy.clone(),
                position: 3,
            }),
        )
        .is_err(),
        "there is no fourth place"
    );

    for gone in [&copy, &agenda] {
        confirmed(
            &mut deck,
            Deck::DELETE_SLIDE.with(deck::DeleteSlide {
                slide: gone.clone(),
            }),
        )
        .unwrap();
    }
    assert_eq!(slide_ids(&deck), std::slice::from_ref(&first));
    assert!(
        confirmed(
            &mut deck,
            Deck::DELETE_SLIDE.with(deck::DeleteSlide { slide: first }),
        )
        .is_err(),
        "the last slide stays"
    );
}

/// `set-text` rewrites a slide's title and body words in place; blank text
/// is refused.
#[test]
fn set_text_rewrites_title_and_body() {
    let mut deck = Replica::<Deck>::new();
    let slide = add_slide(&mut deck, "Draft", "Old body");
    apply(
        &mut deck,
        Deck::SET_TEXT.with(deck::SetText {
            slide: slide.clone(),
            title: Some("Final".into()),
            body: None,
        }),
    )
    .unwrap();
    assert_eq!(words(&deck, &slide), ["Final", "Old body"]);
    assert!(
        apply(
            &mut deck,
            Deck::SET_TEXT.with(deck::SetText {
                slide,
                title: None,
                body: Some("  ".into()),
            }),
        )
        .is_err()
    );
}

/// Elements are added on top, restacked, placed by field edits checked
/// against the slide, and deleted.
#[test]
fn elements_are_added_restacked_placed_and_deleted() {
    let mut deck = Replica::<Deck>::new();
    let slide = add_slide(&mut deck, "Shapes", "Body");
    let before = element_ids(&deck, &slide);
    apply(
        &mut deck,
        Deck::ADD_ELEMENT.with(deck::AddElement {
            slide: slide.clone(),
            element: serde_json::from_value(serde_json::json!({
                "kind": "ellipse", "x": 100, "y": 100, "w": 200, "h": 120,
            }))
            .unwrap(),
        }),
    )
    .unwrap();
    let ids = element_ids(&deck, &slide);
    let added = ids.last().unwrap().clone();
    assert!(!before.contains(&added), "a new element is on top");

    apply(
        &mut deck,
        Deck::SEND_TO_BACK.with(deck::SendToBack {
            slide: slide.clone(),
            element: added.clone(),
        }),
    )
    .unwrap();
    assert_eq!(element_ids(&deck, &slide)[0], added);
    apply(
        &mut deck,
        Deck::BRING_TO_FRONT.with(deck::BringToFront {
            slide: slide.clone(),
            element: added.clone(),
        }),
    )
    .unwrap();
    assert_eq!(element_ids(&deck, &slide).last(), Some(&added));

    let place = Deck::paths().slides.item(&slide).elements.item(&added);
    edit(&mut deck, &place.x.set(300.0)).unwrap();
    let moved = state(&deck);
    let element = moved
        .slides
        .get(&slide)
        .unwrap()
        .elements
        .get(&added)
        .unwrap();
    assert!((element.x - 300.0).abs() < f64::EPSILON);
    assert!(
        edit(&mut deck, &place.w.set(f64::NAN)).is_err(),
        "a size the slide cannot hold is refused"
    );

    confirmed(
        &mut deck,
        Deck::DELETE_ELEMENT.with(deck::DeleteElement {
            slide: slide.clone(),
            element: added.clone(),
        }),
    )
    .unwrap();
    assert_eq!(element_ids(&deck, &slide), before);
}

/// A replacement deck naming a slide twice, or holding none, is refused.
#[test]
fn a_replacement_deck_is_checked_whole() {
    let mut deck = Replica::<Deck>::new();
    let seed = crate::model::seed_deck();
    let mut slides: Vec<SlideData> = seed.slides.iter().map(SlideData::of).collect();
    slides.push(slides[0].clone());
    let replace = |slides: Vec<SlideData>| {
        Deck::REPLACE_DECK.with(deck::ReplaceDeck {
            title: "Twice".into(),
            palette: PaletteKind::Indigo,
            slides,
        })
    };
    assert!(apply(&mut deck, replace(slides)).is_err());
    assert!(apply(&mut deck, replace(Vec::new())).is_err());
    let whole = seed.slides.iter().map(SlideData::of).collect();
    apply(&mut deck, replace(whole)).unwrap();
    assert_eq!(state(&deck).slides.len(), seed.slides.len());
    assert_eq!(
        mirror(&state(&deck)).slides[0].elements.len(),
        seed.slides[0].elements.len()
    );
}

/// Two people editing one text box and moving one element at once both
/// keep their change.
#[test]
fn two_replicas_converge() {
    let mut left = Replica::<Deck>::new();
    let slide = add_slide(&mut left, "Shared", "Words");
    let mut right = left.fork();
    let text_box = state(&left)
        .slides
        .get(&slide)
        .unwrap()
        .elements
        .iter()
        .find(|(_, element)| element.kind == ElementKind::Text)
        .map(|(id, _)| id.clone())
        .unwrap();
    let body = Deck::paths().slides.item(&slide).elements.item(&text_box);
    let block = blocks_of(
        &state(&left)
            .slides
            .get(&slide)
            .unwrap()
            .elements
            .get(&text_box)
            .unwrap()
            .body,
    )[0]
    .id
    .clone();
    let splice = |at: usize, text: &str| RichTextAction::SpliceText {
        region: RegionKind::Body,
        block: block.clone(),
        at,
        del: 0,
        ins: text.into(),
    };
    edit(&mut left, &body.body.perform(&splice(0, "L ")).unwrap()).unwrap();
    edit(&mut right, &body.body.perform(&splice(6, " R")).unwrap()).unwrap();
    edit(&mut right, &body.y.set(40.0)).unwrap();

    left.sync_with(&mut right).unwrap();
    assert_eq!(state(&left), state(&right), "both read the same deck");
    assert_eq!(words(&left, &slide)[0], "L Shared R");
    let merged = state(&left);
    let element = merged
        .slides
        .get(&slide)
        .unwrap()
        .elements
        .get(&text_box)
        .unwrap();
    assert!((element.y - 40.0).abs() < f64::EPSILON);
}

/// Stale, hostile and over-limit changes are refused and write nothing.
#[test]
fn hostile_or_over_limit_changes_are_refused_atomically() {
    let mut deck = Replica::<Deck>::new();
    let slide = slide_ids(&deck)[0].clone();
    let element = element_ids(&deck, &slide)[0].clone();
    let place = Deck::paths().slides.item(&slide).elements.item(&element);
    let before = state(&deck);
    assert!(
        edit(&mut deck, &place.x.set(1.0e9)).is_err(),
        "off the slide"
    );
    assert!(edit(&mut deck, &place.stroke_width.set(1_000.0)).is_err());
    assert!(
        confirmed(
            &mut deck,
            Deck::DELETE_ELEMENT.with(deck::DeleteElement {
                slide: slide.clone(),
                element: Id::from("stale-element"),
            }),
        )
        .is_err()
    );
    assert!(
        apply(
            &mut deck,
            Deck::MOVE_SLIDE.with(deck::MoveSlide {
                slide: Id::from("stale-slide"),
                position: 0,
            }),
        )
        .is_err()
    );
    let palette = serde_json::to_string(&Deck::paths().palette.set(PaletteKind::Rose))
        .unwrap()
        .replace("\"rose\"", "\"untrusted-purple\"");
    assert!(
        serde_json::from_str::<Edit<Deck>>(&palette)
            .map_err(|error| error.to_string())
            .and_then(|palette| edit(&mut deck, &palette))
            .is_err(),
        "a theme outside the choices is refused"
    );
    let twin = mirror(&state(&deck)).slides[0].elements[0].clone();
    assert!(
        apply(
            &mut deck,
            Deck::INSERT_ELEMENT.with(deck::InsertElement {
                slide: slide.clone(),
                element: ElementData::of(&twin),
            }),
        )
        .is_err(),
        "an element of an id the deck holds is refused"
    );
    assert_eq!(state(&deck), before, "nothing was written");

    let mut full = crate::model::starter_deck();
    full.slides = (0..MAX_SLIDES)
        .map(|_| crate::model::Slide::new(Layout::Blank))
        .collect();
    apply(
        &mut deck,
        Invoke::apply(&crate::actions::replace_deck_action(&full)).unwrap(),
    )
    .unwrap();
    let at_limit = state(&deck);
    assert!(
        apply(
            &mut deck,
            Deck::ADD_SLIDE.with(deck::AddSlide {
                title: None,
                body: None,
                layout: None,
                after: None,
            }),
        )
        .is_err(),
        "a deck holds at most {MAX_SLIDES} slides"
    );
    assert_eq!(state(&deck).slides.len(), at_limit.slides.len());
}

/// Changes name slides and elements by id, so a change made while a peer
/// inserts, moves or restacks elsewhere lands where it was meant to, and
/// both replicas end alike.
#[test]
fn ids_keep_changes_in_place_through_divergent_edits() {
    let mut left = Replica::<Deck>::new();
    let first = slide_ids(&left)[0].clone();
    let second = add_slide(&mut left, "Second", "Body");
    let third = add_slide(&mut left, "Third", "Body");
    let mut right = left.fork();

    // Left moves an element of the second slide; right inserts a slide
    // before it and moves the second slide to the end.
    let target = element_ids(&left, &second)[0].clone();
    let place = Deck::paths().slides.item(&second).elements.item(&target);
    edit(&mut left, &place.x.set(321.0)).unwrap();
    edit(&mut left, &place.y.set(222.0)).unwrap();
    apply(
        &mut left,
        Deck::BRING_TO_FRONT.with(deck::BringToFront {
            slide: second.clone(),
            element: target.clone(),
        }),
    )
    .unwrap();
    apply(
        &mut right,
        Deck::ADD_SLIDE.with(deck::AddSlide {
            title: Some("Inserted".into()),
            body: None,
            layout: None,
            after: Some(first.clone()),
        }),
    )
    .unwrap();
    apply(
        &mut right,
        Deck::MOVE_SLIDE.with(deck::MoveSlide {
            slide: second.clone(),
            position: 3,
        }),
    )
    .unwrap();
    let other = element_ids(&right, &second)[1].clone();
    apply(
        &mut right,
        Deck::SEND_TO_BACK.with(deck::SendToBack {
            slide: second.clone(),
            element: other.clone(),
        }),
    )
    .unwrap();

    left.sync_with(&mut right).unwrap();
    let (merged, theirs) = (state(&left), state(&right));
    assert_eq!(mirror(&merged), mirror(&theirs), "both read the same deck");
    let ids = slide_ids(&left);
    assert_eq!(ids.len(), 4);
    assert_eq!(ids.last(), Some(&second), "the moved slide is last");
    assert_eq!(ids[0], first);
    assert!(ids.contains(&third));
    let moved = merged
        .slides
        .get(&second)
        .unwrap()
        .elements
        .get(&target)
        .unwrap();
    assert_eq!((moved.x, moved.y), (321.0, 222.0));
    let order = element_ids(&left, &second);
    assert_eq!(order.last(), Some(&target), "brought to the front");
    assert_eq!(order.first(), Some(&other), "sent to the back");
}
