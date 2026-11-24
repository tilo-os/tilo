//! Effects offered on the items they act on, `open` on every item a view
//! can show, and labelled items published with no item verb, and a verb
//! that answers its caller with what it made.

use std::cell::RefCell;
use tilo_app_test_host::TestApp;

use tilo_app::framework::{Effected, Publishes};
use tilo_app::prelude::*;

thread_local! {
    static RAN: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
    static OPENED: RefCell<Vec<Id>> = const { RefCell::new(Vec::new()) };
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Query {
    #[model(label)]
    pub name: String,
}

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Queries {
    pub queries: Collection<Query>,
}

#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "test.queries.effect", catalogue = "tests/locales")]
pub enum QueryEffect {
    /// Run a saved query.
    #[effect(kind = "outside")]
    #[effect(row, item = queries)]
    Run {
        #[param("The query")]
        query: String,
        #[param("Rows at most")]
        limit: u32,
    },
}

fn answer(
    _doc: &tilo_resource::Doc,
    action: &str,
    _changes: &[tilo_resource::Patch],
) -> Option<serde_json::Value> {
    (action == "save").then(|| serde_json::json!({ "saved": true }))
}

#[resource(kind = "test.queries", effects = QueryEffect, answer = answer, app = "so.tilo.test-queries", catalogue = "tests/locales")]
impl Queries {
    /// Save a query.
    #[action(answers)]
    pub fn save(&mut self, cx: &mut ActionCx, name: String) {
        self.queries.push(cx.id(), Query { name });
    }
}

struct QueriesApp;

impl ResourceApp for QueriesApp {
    type Resource = Queries;
    const ID: &'static str = "so.tilo.test-queries";
    const TITLE: &'static str = "Queries";

    fn view(cx: &mut ViewCx<Queries>) -> Element {
        cx.effects(|effect: QueryEffect, _| {
            let QueryEffect::Run { query, limit } = effect;
            RAN.with(|ran| ran.borrow_mut().push(format!("{query}:{limit}")));
            Ok(Effected::done())
        });
        cx.on_open_item(|item| {
            OPENED.with(|opened| opened.borrow_mut().push(item.clone()));
            Ok(())
        });
        Box::new(Column::new())
    }
}

fn find<'a>(
    resources: &'a [tilo_actions::ResourceSpec],
    id: &str,
) -> Option<&'a tilo_actions::ResourceSpec> {
    resources.iter().find_map(|resource| {
        (resource.id.as_str() == id)
            .then_some(resource)
            .or_else(|| find(&resource.children, id))
    })
}

#[test]
fn items_offer_their_effects_and_open_and_labelled_items_are_published() {
    let mut app = TestApp::<QueriesApp>::new().unwrap();
    app.invoke(Queries::SAVE.with("Top customers")).unwrap();
    let id = app.state().queries.ids().next().cloned().unwrap();
    let item = tilo_resource::items::item_resource_id::<Queries>("queries", &id);

    let resources = app.harness().resources();
    let published = find(&resources, &item).expect("a labelled item is published");
    assert_eq!(published.label, "Top customers");
    let actions: Vec<&str> = published.actions.iter().map(|a| a.id.as_str()).collect();
    assert!(actions.contains(&"run"), "{actions:?}");
    assert!(actions.contains(&"open"), "{actions:?}");
    let run = published.actions.iter().find(|a| a.id == "run").unwrap();
    assert_eq!(
        run.params()
            .iter()
            .map(|p| p.name.to_string())
            .collect::<Vec<_>>(),
        ["limit"],
        "the query is the item; only the limit is asked"
    );

    app.call_on(&item, "run", serde_json::json!({ "limit": 10 }))
        .unwrap();
    assert_eq!(RAN.with(|ran| ran.borrow().clone()), [format!("{id}:10")]);
    app.call_on(&item, "open", serde_json::json!({})).unwrap();
    assert_eq!(OPENED.with(|opened| opened.borrow().clone()), [id]);

    let answered = app
        .call("save", serde_json::json!({ "name": "Churn" }))
        .unwrap();
    let answered = answered.expect("the verb answers");
    assert_eq!(answered["saved"], true, "with what it made");
    let churn = app
        .state()
        .queries
        .iter()
        .find(|(_, query)| query.name == "Churn")
        .map(|(id, _)| id.as_str().to_owned())
        .unwrap();
    assert_eq!(
        answered["minted"],
        serde_json::json!([churn]),
        "and the ids it minted"
    );
}

/// The action of a read that answers with the item's note, as an app's
/// registered reader publishes it: the spec a view hands out for the item it
/// knows, never an operation the app's handler answers.
fn note_read(id: &str) -> tilo_resource::read::Declared {
    let id = id.to_owned();
    tilo_resource::read::read_action(
        tilo_resource::read::ReadName {
            id: "read-note",
            label: "Read note",
            description: "Read this query's note from its committed state",
        },
        "test.queries.read",
        note_schema,
        move |_: tilo_resource::read::NoReadParams| serde_json::json!({ "note": id }),
    )
}

fn note_schema() -> &'static serde_json::Value {
    static SCHEMA: std::sync::OnceLock<serde_json::Value> = std::sync::OnceLock::new();
    SCHEMA.get_or_init(|| serde_json::json!({ "type": "object" }))
}

struct QueriesReadApp;

impl ResourceApp for QueriesReadApp {
    type Resource = Queries;
    const ID: &'static str = "so.tilo.test-queries";
    const TITLE: &'static str = "Queries";

    fn view(cx: &mut ViewCx<Queries>) -> Element {
        cx.effects(|_effect: QueryEffect, _| Ok(Effected::done()));
        cx.publishing(
            Publishes::new().item_reads(|field, id| {
                (field == "queries")
                    .then(|| note_read(id))
                    .into_iter()
                    .collect()
            }),
            Box::new(Column::new()),
        )
    }
}

#[test]
fn a_view_publishes_the_reads_its_items_answer() {
    let mut app = TestApp::<QueriesReadApp>::new().unwrap();
    app.invoke(Queries::SAVE.with("Top customers")).unwrap();
    let id = app.state().queries.ids().next().cloned().unwrap();
    let item = tilo_resource::items::item_resource_id::<Queries>("queries", &id);

    let resources = app.harness().resources();
    let published = find(&resources, &item).expect("the item is published");
    let read = published
        .actions
        .iter()
        .find(|action| action.id == "read-note")
        .expect("the read the view declares is on the item");
    assert_eq!(
        read.operation_type.as_deref(),
        Some("test.queries.read"),
        "it is the registered reader's operation, not the app's handler's"
    );
    assert_eq!(
        published
            .actions
            .iter()
            .filter(|action| action.id == "read-note")
            .count(),
        1,
        "a read declared twice is published once"
    );
}
