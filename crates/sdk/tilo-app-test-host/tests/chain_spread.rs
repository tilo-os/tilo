//! What one change of a chain means for the items its other parts keep: a
//! label renamed in one part is written into every part naming it, as the
//! same invoker's step in each.

use std::rc::Rc;

use tilo_app::framework::{Chain, Chained, PartLimits};
use tilo_app::prelude::*;
use tilo_app::resource::{DocumentId, ResourceOwners, ResourceStore};
use tilo_app::tilo_resource::Collection;

/// Cards a part holds before the next one starts.
const PART_CARDS: usize = 2;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Deck {
    pub title: String,
}

#[resource(kind = "test.spread-deck", catalogue = "tests/locales")]
impl Deck {
    /// Rename the deck.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    pub name: String,
    /// The name of the card this one points at, as it read when written.
    pub points_at: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct CardPart {
    #[model(readonly)]
    pub cards: Collection<Card>,
}

#[resource(kind = "test.spread-deck-part", catalogue = "tests/locales")]
impl CardPart {
    /// Add a card.
    #[action]
    pub fn add(&mut self, cx: &mut ActionCx, name: String, points_at: String) {
        self.cards.push(cx.id(), Card { name, points_at });
    }

    /// Rename a card.
    #[action]
    pub fn rename(&mut self, card: &Id, name: String) {
        if let Some(card) = self.cards.get_mut(card) {
            card.name = name;
        }
    }

    /// Point every card at `from` at `to` instead.
    #[action]
    pub fn repoint(&mut self, from: String, to: String) {
        for (_, card) in self.cards.iter_mut() {
            if card.points_at == from {
                card.points_at.clone_from(&to);
            }
        }
    }
}

impl Chained for CardPart {
    type Base = Deck;

    fn chain() -> DocumentId {
        DocumentId::derived("cards", &DocumentId::workspace_default())
    }

    fn spread(
        action: &Self::Action,
        index: u64,
        parts: &[(u64, Rc<Self>)],
    ) -> Vec<(u64, Self::Action)> {
        // Cards elsewhere point at the card by the name it had: "first",
        // in this test's deck.
        let CardPartAction::Rename(card_part::Rename { name, .. }) = &action.operation else {
            return Vec::new();
        };
        parts
            .iter()
            .filter(|(held, part)| {
                *held != index && part.cards.values().any(|card| card.points_at == "first")
            })
            .map(|(held, _)| {
                (
                    *held,
                    tilo_app::tilo_resource::Stamped {
                        operation: CardPartAction::Repoint(card_part::Repoint {
                            from: "first".into(),
                            to: name.clone(),
                        }),
                        now_ms: action.now_ms,
                    },
                )
            })
            .collect()
    }
}

fn device() -> ResourceStore {
    ResourceOwners::in_memory()
        .open("so.tilo.test-spread-deck", "workspace")
        .unwrap()
}

fn add(chain: &Chain<CardPart>, name: &str, points_at: &str) {
    chain
        .invoke(&CardPart::ADD.with(card_part::Add {
            name: name.into(),
            points_at: points_at.into(),
        }))
        .unwrap();
}

#[test]
fn a_change_spreads_to_the_parts_naming_what_it_changed() {
    let _parts = PartLimits::scoped(PART_CARDS, usize::MAX);
    let store = device();
    let me = MemberId::person("ada");
    let deck = Rc::new(Deck::default());
    let chain = Chain::<CardPart>::detached_in(store, "so.tilo.test-spread-deck", &me, move || {
        Rc::clone(&deck)
    })
    .unwrap();
    add(&chain, "first", "");
    add(&chain, "second", "first");
    add(&chain, "third", "first");
    add(&chain, "fourth", "second");
    assert_eq!(chain.head(), Some(1), "the cards fill two parts");
    let first = chain.parts()[0]
        .1
        .cards
        .iter()
        .find(|(_, card)| card.name == "first")
        .map(|(id, _)| id.clone())
        .unwrap();

    let written = chain
        .invoke(&CardPart::RENAME.with(card_part::Rename {
            card: first,
            name: "one".into(),
        }))
        .unwrap();
    assert_eq!(written, Some(0), "the rename lands where the card is");
    let pointing: Vec<(u64, String, String)> = chain
        .parts()
        .iter()
        .flat_map(|(index, part)| {
            part.cards
                .values()
                .map(|card| (*index, card.name.clone(), card.points_at.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(
        pointing.contains(&(1, "third".into(), "one".into())),
        "the other part follows the rename: {pointing:?}"
    );
    assert!(
        pointing.contains(&(0, "second".into(), "first".into())),
        "the part the rename landed in is the action's own to change: {pointing:?}"
    );

    // Each part's change is a step of its own there: undoing the spread
    // step keeps the rename.
    chain.undo_in(1).unwrap();
    let names: Vec<(String, String)> = chain
        .parts()
        .iter()
        .flat_map(|(_, part)| {
            part.cards
                .values()
                .map(|card| (card.name.clone(), card.points_at.clone()))
                .collect::<Vec<_>>()
        })
        .collect();
    assert!(names.contains(&("one".into(), String::new())));
    assert!(names.contains(&("third".into(), "first".into())));
}
