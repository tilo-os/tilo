//! A screen effect offered on a row answers its invoker at that row, in the
//! invoker's own view: the row's id, which is not always the id the effect's
//! argument names.

use std::cell::RefCell;
use tilo_app_test_host::TestApp;

use tilo_app::framework::{Effected, LiveRow, Publishes};
use tilo_app::prelude::presence::PresenceKey;
use tilo_app::prelude::*;

thread_local! {
    static ANSWERED: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

/// A card's row. Its id carries the kind (`test.card:alpha`); the effect that
/// shows it names the card alone (`alpha`).
#[derive(tilo_resource::Row)]
#[row(kind = "test.card", label = "Card", effects = CardEffect)]
pub struct CardRow;

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.cards.effect", catalogue = "tests/locales")]
pub enum CardEffect {
    /// Show a card.
    #[effect(screen, on = CardRow)]
    Show {
        #[param("The card's name")]
        card: String,
    },
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Cards {
    #[model(label)]
    pub name: String,
}

#[resource(kind = "test.cards", effects = CardEffect, app = "so.tilo.test-cards", catalogue = "tests/locales")]
impl Cards {
    /// Rename the cards.
    #[action]
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }
}

const ROW: &str = "test.card:alpha";

/// The one card the view shows.
fn rows() -> Vec<LiveRow> {
    let bindings = tilo_resource::RowBindings::new().bind(
        CardEffect::SHOW,
        [("card", serde_json::Value::String("alpha".into()))],
    );
    vec![LiveRow::typed_with::<CardRow>(ROW, "Alpha", &bindings)]
}

struct CardsApp;

impl ResourceApp for CardsApp {
    type Resource = Cards;
    const ID: &'static str = "so.tilo.test-cards";
    const TITLE: &'static str = "Cards";

    fn view(cx: &mut ViewCx<Cards>) -> Element {
        cx.effects(|effect: CardEffect, _| {
            let CardEffect::Show { card } = effect;
            ANSWERED.with(|answered| answered.borrow_mut().push(card));
            Ok(Effected::done())
        });
        cx.publishing(Publishes::new().live_rows(rows), Box::new(Column::new()))
    }
}

#[test]
fn a_screen_effect_places_its_invoker_at_the_row_it_is_offered_on() {
    let mut app = TestApp::<CardsApp>::new().unwrap();
    let show = CardEffect::Show {
        card: "alpha".into(),
    };
    let agent = tilo_actions::PresenceId::new("agent-alpha-view").unwrap();
    app.call_by(
        tilo_actions::ActionInvoker::agent("card-helper").with_presence(agent.clone()),
        ROW,
        show.id(),
        serde_json::json!({ "card": "alpha" }),
    )
    .unwrap();
    app.settle();
    assert_eq!(
        ANSWERED.with(|answered| answered.borrow().clone()),
        Vec::<String>::new(),
        "the person's own screen does not move: the agent's press is answered in its view"
    );
    let presences = app.presences();
    let shown = presences
        .iter()
        .find(|shown| shown.presence == agent)
        .unwrap_or_else(|| panic!("the person sees the agent: {presences:?}"));
    assert!(
        matches!(&shown.key, PresenceKey::Resource { id, .. } if id == ROW),
        "the agent is placed at the row, not at the id its argument names: {shown:?}"
    );

    // The person's own press moves their screen, and nothing is placed.
    app.call_on(ROW, show.id(), serde_json::json!({ "card": "alpha" }))
        .unwrap();
    assert_eq!(
        ANSWERED.with(|answered| answered.borrow().clone()),
        ["alpha".to_owned()]
    );
}
