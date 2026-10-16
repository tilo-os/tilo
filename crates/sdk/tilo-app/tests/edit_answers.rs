//! An effect that edits its record answers its caller once the edits it
//! submitted landed: they go through the door like any other submission,
//! checked again, and the effect replies from what the door answered.

use tilo_app::framework::{EffectCx, Effected};
use tilo_app::prelude::*;
use tilo_resource::Effect;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    #[model(label)]
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
#[model(check = Deck::valid)]
pub struct Deck {
    pub title: String,
    pub cards: Collection<Card>,
}

impl Deck {
    fn valid(&self) -> Result<(), Refusal> {
        if self.cards.len() > 2 {
            Err(Refusal::new("a deck holds two cards"))
        } else {
            Ok(())
        }
    }
}

#[resource(kind = "test.deck", label = "Deck")]
impl Deck {}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.deck.effects")]
pub enum DeckEffect {
    /// Add a card.
    #[effect(edit, job, returns)]
    AddCard {
        #[param("What the card says")]
        text: String,
    },
    /// Add two cards.
    #[effect(edit, job, returns)]
    AddTwo,
}

fn add(cx: &EffectCx<Deck>, text: &str) {
    let at = cx.live().with(|deck| deck.cards.len());
    cx.dispatcher()
        .submit(Deck::paths().cards.insert(at, &Card { text: text.into() }));
}

struct DeckApp;

impl ResourceApp for DeckApp {
    type Resource = Deck;
    const ID: &'static str = "so.tilo.test-deck";
    const TITLE: &'static str = "Deck";

    fn view(cx: &mut ViewCx<Deck>) -> Element {
        cx.effects(|effect: DeckEffect, cx| {
            match effect {
                DeckEffect::AddCard { text } => add(cx, &text),
                DeckEffect::AddTwo => {
                    cx.dispatcher().group(|| {
                        add(cx, "one");
                        add(cx, "two");
                    });
                }
            }
            cx.when_landed(|(), landed, _| {
                let minted: Vec<String> = landed.minted().iter().map(ToString::to_string).collect();
                Ok(Effected::value(minted))
            })
        });
        Box::new(Column::new())
    }
}

#[test]
fn an_edit_effect_answers_with_what_its_edit_made_after_it_landed() {
    let mut app = TestApp::<DeckApp>::new().unwrap();
    let output = app
        .effect(&DeckEffect::AddCard {
            text: "Hearts".into(),
        })
        .unwrap();
    let state = app.state();
    let (id, card) = state.cards.iter().next().expect("the card landed");
    assert_eq!(card.text, "Hearts");
    assert_eq!(
        output.value,
        Some(serde_json::json!([id.to_string()])),
        "the answer names the card the edit made"
    );
}

#[test]
fn a_refused_edit_fails_the_effect_that_submitted_it() {
    let mut app = TestApp::<DeckApp>::new().unwrap();
    app.effect(&DeckEffect::AddCard { text: "a".into() })
        .unwrap();
    let refused = app.effect(&DeckEffect::AddTwo).unwrap_err();
    assert!(
        refused.to_string().contains("a deck holds two cards"),
        "{refused}"
    );
    assert_eq!(
        app.state().cards.len(),
        1,
        "the refused edit changed nothing"
    );
}
