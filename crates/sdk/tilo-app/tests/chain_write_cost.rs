//! A write into one part of a chain costs what it touched, not what the
//! part holds: the chain publishes again only the items that changed, the
//! same work beside fifty items as beside five hundred.
#![allow(clippy::unwrap_used, reason = "test assertions")]

use tilo_app::framework::Chained;
use tilo_app::prelude::*;
use tilo_app::resource::DocumentId;
use tilo_app::tilo_resource::Collection;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Board {
    pub title: String,
}

#[resource(kind = "test.cost-board", catalogue = "tests/locales")]
impl Board {
    /// Rename the board.
    #[action]
    pub fn rename(&mut self, title: String) {
        self.title = title;
    }
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Card {
    #[model(label)]
    pub text: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct BoardPart {
    pub cards: Collection<Card>,
}

#[resource(kind = "test.cost-board-part", catalogue = "tests/locales")]
impl BoardPart {
    /// Put cards up.
    #[action]
    pub fn put(&mut self, cx: &mut ActionCx, texts: Vec<String>) {
        for text in texts {
            self.cards.push(cx.id(), Card { text });
        }
    }
}

impl Chained for BoardPart {
    type Base = Board;

    fn chain() -> DocumentId {
        DocumentId::derived("cards", &DocumentId::workspace_default())
    }
}

struct BoardApp;

impl ResourceApp for BoardApp {
    type Resource = Board;
    const ID: &'static str = "so.tilo.test-cost-board";
    const TITLE: &'static str = "Board";

    fn resources(resources: &mut tilo_app::framework::Resources<'_>) {
        resources.chain::<BoardPart>();
    }

    fn view(_: &mut ViewCx<Board>) -> Element {
        Box::new(Column::new())
    }
}

fn put(app: &mut TestApp<BoardApp>, texts: Vec<String>) {
    app.chain_invoke(&BoardPart::PUT.with(board_part::Put { texts }))
        .unwrap();
}

/// How many item resources publishing the app built after one card was put
/// up beside `cards` others, and whether the new card is published.
fn items_built_by_one_write(cards: usize) -> (usize, bool) {
    let mut app = TestApp::<BoardApp>::new().unwrap();
    put(&mut app, (0..cards).map(|n| format!("card {n}")).collect());
    app.settle();
    let before = app.actions().unwrap();
    assert!(
        before.len() > cards,
        "every card is published with its actions"
    );
    put(&mut app, vec!["one more".into()]);
    let (after, phases) = tilo_app::tilo_resource::profiling::observe(|| {
        app.settle();
        app.actions().unwrap()
    });
    let built = phases
        .iter()
        .find(|phase| phase.phase == "framework.item_child")
        .map_or(0, |phase| phase.calls);
    (built, after.len() > before.len())
}

tilo_test_env::scenario! {
    fn a_write_into_a_part_costs_the_same_however_many_items_it_holds;

    phase only_the_written_item_is_published_anew;
    {
        // Both within the page of items a part publishes.
        let (few, shown_few) = items_built_by_one_write(50);
        let (many, shown_many) = items_built_by_one_write(250);
        assert!(shown_few && shown_many, "the new card is published");
        assert_eq!(few, 1, "only the new card is built");
        assert_eq!(many, 1, "only the new card is built beside 250 others");
        // Past the page, the new card is not published and nothing is built.
        let (past, _) = items_built_by_one_write(500);
        assert!(past <= 1, "a full page builds no card again, built {past}");
    }
}
