//! A write into one part of a chain costs what it touched, not what the
//! part holds: the chain publishes again only the items that changed, the
//! same work beside fifty items as beside five hundred. A copy of the board
//! that takes its parts from the workspace writes into the part the workspace
//! holds, never into one it started before the workspace answered.

use tilo_app::framework::testing::TEST_WORKSPACE;
use tilo_app::framework::{AttachedView, Chained, definition};
use tilo_app::prelude::*;
use tilo_app::resource::{DocumentCheckout, DocumentId, chain_resource_id};
use tilo_app::tilo_resource::Collection;
use tilo_app::tilo_resource::profiling::observe;
use tilo_app_test_host::TestApp;
use tilo_permissions::PermissionContext;

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

/// A copy of the board where no window is open, as a workspace's server keeps one for each
/// member who acts there.
fn copy_for(app: &TestApp<BoardApp>, member: &str) -> AttachedView {
    let definition = definition::<BoardApp>();
    AttachedView::open(
        definition,
        TEST_WORKSPACE,
        DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
        app.read_document(|doc| doc.clone().save()),
        &MemberId::person(member),
        PermissionContext::permissive(),
        false,
    )
    .unwrap()
}

/// [`copy_for`] whose parts come from the workspace, as a page's worker holds a record: the
/// copy is handed the board alone and asks the workspace for what it lacks.
fn asking_copy_for(app: &TestApp<BoardApp>, member: &str) -> AttachedView {
    let definition = definition::<BoardApp>();
    AttachedView::opening(
        definition,
        TEST_WORKSPACE,
        DocumentCheckout::new(app.record().clone(), definition.resource_type.clone()),
        app.read_document(|doc| doc.clone().save()),
    )
    .for_invoker(&tilo_actions::ActionInvoker::member(MemberId::person(
        member,
    )))
    .asking_the_workspace()
    .open()
    .unwrap()
}

/// The cards `view` holds in the chain's part `index`.
fn cards_in(view: &AttachedView, index: u64) -> Vec<String> {
    let mut texts: Vec<String> = view
        .read_resource(&BoardPart::chain().part(index), |doc| {
            <BoardPart as Resource>::read(doc)
                .map(|part| part.cards.values().map(|card| card.text.clone()).collect())
                .unwrap_or_default()
        })
        .unwrap_or_default();
    texts.sort();
    texts
}

/// What `put` takes to put `texts` up.
fn putting(texts: &[String]) -> serde_json::Map<String, serde_json::Value> {
    let mut arguments = serde_json::Map::new();
    arguments.insert("texts".into(), serde_json::json!(texts));
    arguments
}

/// Hand what `writer` changed to `keeper`, as a workspace's server hands a member's change
/// to the copy it keeps. The parts that changed.
fn hand_over(writer: &mut AttachedView, keeper: &mut AttachedView) -> Vec<DocumentId> {
    if let Some(change) = writer.take_changes().unwrap() {
        keeper.merge(&change).unwrap();
    }
    let mut parts = Vec::new();
    for handed in writer.take_resources_beside_changes().unwrap() {
        keeper
            .merge_resource(&handed.document, &handed.mutation())
            .unwrap();
        parts.push(handed.document);
    }
    parts
}

/// What two copies of a board do for ONE more card beside `cards` others: every scope that
/// ran, by name, with how often it ran. The writer resolves the action, carries it out and
/// hands its change over; the keeper takes it, says what it now wants of the workspace and
/// answers the lookup the next write begins with.
fn work_for_one_more_card(cards: usize) -> Vec<(&'static str, usize)> {
    let app = TestApp::<BoardApp>::new().unwrap();
    let mut writer = copy_for(&app, "ada");
    let mut keeper = copy_for(&app, "bo");
    let put = tilo_actions::ActionAddress::new(chain_resource_id(&BoardPart::chain()), "put");
    let before: Vec<String> = (0..cards).map(|n| format!("card {n}")).collect();
    writer.invoke(&put, &putting(&before), None).unwrap();
    let parts = hand_over(&mut writer, &mut keeper);
    assert_eq!(parts.len(), 1, "every card went into the one part");
    // Each copy is read once: what the cards before cost is behind it.
    let _ = writer.resources();
    let _ = keeper.wanted_resources();
    let _ = keeper.resources();
    let one = putting(&["one more".to_owned()]);
    let ((), phases) = observe(|| {
        assert!(
            writer.descriptor(&put).is_some(),
            "the writer is offered it"
        );
        writer.invoke(&put, &one, None).unwrap();
        let changed = hand_over(&mut writer, &mut keeper);
        assert_eq!(changed, parts, "the card went into the same part");
        let _ = keeper.wanted_resources();
        assert!(
            keeper.descriptor(&put).is_some(),
            "the keeper answers the next"
        );
    });
    let mut work: Vec<(&'static str, usize)> = phases
        .iter()
        .map(|phase| (phase.phase, phase.calls))
        .collect();
    work.sort_unstable();
    work
}

/// How often the scope `name` ran in `work`.
fn ran(work: &[(&'static str, usize)], name: &str) -> usize {
    work.iter()
        .filter(|(phase, _)| *phase == name)
        .map(|(_, calls)| *calls)
        .sum()
}

tilo_test_env::scenario! {
    fn one_more_item_costs_two_copies_the_same_however_many_the_part_holds;

    phase no_view_is_laid_out_and_no_scope_is_entered_more_often_beside_six_hundred;
    {
        let few = work_for_one_more_card(20);
        let many = work_for_one_more_card(600);
        // The write into the part is ServerPreferred: the workspace's broker applies it, so its
        // answer arrives as one model sync of each copy. Whatever the part holds, that is one
        // sync and no layout: a second frame, or a layout, would cost what the part holds.
        assert_eq!(
            (ran(&few, "ui.frame_sync"), ran(&many, "ui.frame_sync")),
            (1, 1),
            "the write costs one model sync of either copy: {many:?}"
        );
        assert_eq!(
            (ran(&few, "ui.layout"), ran(&many, "ui.layout")),
            (0, 0),
            "and lays neither copy's view out: {many:?}"
        );
        let grown: Vec<(&str, usize, usize)> = many
            .iter()
            .map(|(phase, calls)| (*phase, ran(&few, phase), *calls))
            .filter(|(_, beside_few, beside_many)| beside_many > beside_few)
            .collect();
        assert!(
            grown.is_empty(),
            "one more card enters no scope more often beside 600 cards than beside 20 \
             (scope, beside 20, beside 600): {grown:?}"
        );
    }
}

tilo_test_env::scenario! {
    fn a_part_handed_whole_to_a_copy_that_holds_none_of_it_is_loaded_once;

    phase the_copy_opens_it_from_what_it_was_handed_and_merges_nothing_again;
    {
        // A copy taking a chain's parts in after a start is handed each whole: it opens the
        // part from those bytes, and merging the same bytes into what it just opened loaded
        // the part a second time, a second for a part of a large tracker.
        let app = TestApp::<BoardApp>::new().unwrap();
        let mut writer = copy_for(&app, "ada");
        let mut keeper = copy_for(&app, "bo");
        let put = tilo_actions::ActionAddress::new(chain_resource_id(&BoardPart::chain()), "put");
        let cards: Vec<String> = (0..200).map(|n| format!("card {n}")).collect();
        writer.invoke(&put, &putting(&cards), None).unwrap();
        let handed = writer.take_resources_beside_changes().unwrap();
        assert_eq!(handed.len(), 1, "every card went into the one part");
        tilo_automerge::shared::clear();
        for handed in &handed {
            assert!(
                handed.whole.is_some(),
                "the part is handed whole the first time"
            );
            assert!(
                keeper
                    .merge_resource(&handed.document, &handed.mutation())
                    .unwrap(),
                "the part is new to the copy"
            );
        }
        assert_eq!(
            tilo_automerge::shared::decoded(),
            0,
            "the copy loads the part it was handed once, as it opens it"
        );
    }
}

tilo_test_env::scenario! {
    fn a_copy_that_asks_the_workspace_writes_into_the_part_it_holds;

    phase the_newest_part_is_asked_for_and_a_write_is_made_once_it_is_here;
    {
        // The workspace holds a part another member started.
        let app = TestApp::<BoardApp>::new().unwrap();
        let mut writer = copy_for(&app, "ada");
        let put = tilo_actions::ActionAddress::new(chain_resource_id(&BoardPart::chain()), "put");
        writer
            .invoke(&put, &putting(&["the workspace's".to_owned()]), None)
            .unwrap();
        let handed = writer.take_resources_beside_changes().unwrap();
        assert_eq!(handed.len(), 1, "the card went into one part");

        // A copy handed the board alone does not know which part is the newest: it asks, and
        // starts none meanwhile, which would be the board's first part again beside the
        // workspace's own.
        let mut keeper = asking_copy_for(&app, "bo");
        assert!(
            keeper
                .wanted_resources()
                .contains(&BoardPart::chain().chain_head()),
            "the chain's newest part is asked for: {:?}",
            keeper.wanted_resources()
        );
        // The write is not made: whoever asked is told to ask again once the part is here.
        let refused = keeper.invoke(&put, &putting(&["bo's".to_owned()]), None);
        assert!(
            matches!(
                &refused,
                Err(tilo_actions::api::ApiError::Unavailable { code, .. }) if *code == "not_ready"
            ),
            "the write waits for the newest part: {refused:?}"
        );
        assert!(
            keeper.take_resources_beside_changes().unwrap().is_empty(),
            "no part is started before the workspace answered"
        );

        // The workspace sends its part, and the write asked again lands in it, as a change to
        // what the workspace holds rather than a part of the copy's own.
        for part in &handed {
            assert!(
                keeper
                    .merge_resource(&part.document, &part.mutation())
                    .unwrap()
            );
        }
        keeper
            .invoke(&put, &putting(&["bo's".to_owned()]), None)
            .unwrap();
        let _ = keeper.resources();
        assert_eq!(cards_in(&keeper, 0), ["bo's", "the workspace's"]);
        let written = keeper.take_resources_beside_changes().unwrap();
        assert_eq!(written.len(), 1, "one part changed: {:?}", written.len());
        assert_eq!(written[0].document, handed[0].document);
        assert!(
            written[0].whole.is_none(),
            "the copy did not start the part, it wrote into the workspace's"
        );
    }

    phase a_part_the_workspace_holds_none_of_is_started_once_it_said_so;
    {
        let app = TestApp::<BoardApp>::new().unwrap();
        let mut keeper = asking_copy_for(&app, "bo");
        let put = tilo_actions::ActionAddress::new(chain_resource_id(&BoardPart::chain()), "put");
        // The answer arrives after something else was handed to the copy, which has the store
        // read again what it already read: the workspace's word still stands.
        keeper.workspace_answered(
            BoardPart::chain().chain_head(),
            WorkspaceAnswer::HoldsNone,
        );
        for wanted in keeper.wanted_resources() {
            keeper.workspace_answered(wanted, WorkspaceAnswer::HoldsNone);
        }
        keeper
            .invoke(&put, &putting(&["first".to_owned()]), None)
            .unwrap();
        let _ = keeper.resources();
        assert_eq!(cards_in(&keeper, 0), ["first"]);
        let written = keeper.take_resources_beside_changes().unwrap();
        assert_eq!(written.len(), 1);
        assert!(
            written[0].whole.is_some(),
            "a part the workspace holds none of is started here, and handed whole"
        );
    }
}
