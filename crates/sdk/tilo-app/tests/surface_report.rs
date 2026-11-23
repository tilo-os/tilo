//! What `tilolang surface` and `tilolang check` read from an app's binary:
//! the resource's public surface, and the controls bound to what it does
//! not publish.

use tilo_app::prelude::*;
use tilo_app::tilo_resource::ActionRef;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Counter {
    pub count: i64,
}

#[resource(
    kind = "test.counter",
    app = "so.tilo.test-surface-good",
    catalogue = "tests/locales"
)]
impl Counter {
    /// Count one more.
    #[action]
    pub fn bump(&mut self) {
        self.count = self.count.saturating_add(1);
    }
}

/// A record the app keeps beside its own.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Prefs {
    pub compact: bool,
}

#[resource(
    kind = "test.counter-prefs",
    app = "so.tilo.test-surface-good",
    catalogue = "tests/locales"
)]
impl Prefs {}

struct GoodApp;

impl ResourceApp for GoodApp {
    type Resource = Counter;
    const ID: &'static str = "so.tilo.test-surface-good";
    const TITLE: &'static str = "Counter";

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let _prefs = cx.profile_resource::<Prefs>();
        Box::new(cx.button("bump", Counter::BUMP.invoke()))
    }
}

struct TypoApp;

impl ResourceApp for TypoApp {
    type Resource = Counter;
    const ID: &'static str = "so.tilo.test-surface-typo";
    const TITLE: &'static str = "Counter";

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let typo = ActionRef::<Counter, ()>::new(
            "bunp",
            &[],
            tilo_resource::ResourceKind::of::<Counter>(),
        );
        Box::new(cx.button("bump", typo.invoke()))
    }
}

tilo_test_env::scenario! {
    fn the_surface_report_lists_what_the_app_publishes_and_binds;

    phase the_surface_report_names_verbs_fields_and_unpublished_bindings;
    {
        let report = tilo_app::framework::headless::surface_report::<GoodApp>();
        let surface = &report["resources"][0];
        assert_eq!(surface["kind"], "test.counter");
        assert!(
            surface["verbs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|verb| verb["id"] == "bump")
        );
        assert!(
            surface["fields"]
                .as_array()
                .unwrap()
                .iter()
                .any(|field| field["path"] == "count")
        );
        assert_eq!(report["unbound"], serde_json::json!([]));
        assert_eq!(
            report["resources"][1]["kind"], "test.counter-prefs",
            "the records the view opens beside its own are part of the surface: {report}"
        );
        assert_eq!(
            report["resources"][1]["person"], true,
            "kept in the person's own scope, apart from the workspace's: {report}"
        );
        assert!(
            report["resources"][0].get("person").is_none(),
            "the app's own record is the workspace's: {report}"
        );
        assert!(report["view"].is_null(), "{report}");

        let report = tilo_app::framework::headless::surface_report::<TypoApp>();
        let unbound = report["unbound"].as_array().unwrap();
        assert_eq!(unbound.len(), 1, "{report}");
        assert!(unbound[0].as_str().unwrap().contains("bunp"), "{report}");
    }

    phase a_read_effect_publishes_its_answer_as_an_action_of_the_resource;
    // A read effect is an action of the app's own resource in its surface, with
    // the value it answers with; what the window shows of itself is no action a
    // caller of another language has.
    {
        let report = tilo_app::framework::headless::surface_report::<ReadingApp>();
        let surface = &report["resources"][0];
        let verbs = surface["verbs"].as_array().unwrap();
        let count = verbs
            .iter()
            .find(|verb| verb["id"] == "count")
            .unwrap_or_else(|| panic!("a read effect publishes a verb: {report}"));
        assert_eq!(count["kind"], "read");
        assert_eq!(
            count["returns"]["type"],
            serde_json::json!({"named": "Counted"}),
            "{report}"
        );
        assert_eq!(
            count["returns"]["schema"]["properties"]["what"]["type"],
            "string"
        );
        let counted = surface["types"]
            .as_array()
            .unwrap()
            .iter()
            .find(|ty| ty["name"] == "Counted")
            .unwrap_or_else(|| panic!("the answer's record is declared: {report}"));
        let fields: Vec<&str> = counted["fields"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|field| field["name"].as_str())
            .collect();
        assert_eq!(fields, ["count", "what"]);
        assert!(
            !verbs.iter().any(|verb| verb["id"] == "zoom-in"),
            "a view is no action of the catalogue: {report}"
        );
    }

    phase a_row_bound_effect_is_an_action_of_the_resource;
    // An effect offered on a row of a declared type is the resource's action
    // too: its parameter names the row it acts on.
    {
        let report = tilo_app::framework::headless::surface_report::<ReadingApp>();
        let surface = &report["resources"][0];
        let open = surface["verbs"]
            .as_array()
            .unwrap()
            .iter()
            .find(|verb| verb["id"] == "open-row")
            .unwrap_or_else(|| panic!("a row-bound read effect publishes a verb: {report}"));
        assert_eq!(open["params"][0]["name"], "row", "{report}");
        assert_eq!(open["params"][0]["required"], true, "{report}");
        assert_eq!(open["params"][0]["type"], "text", "{report}");
    }

    phase an_agent_calls_a_row_bound_effect_by_its_published_address;
    // The address and the row parameter come from the surface; an agent calls
    // an effect that reads, and only a screen's own person calls one that
    // shows.
    {
        use tilo_app::tilo_actions::ActionInvoker;

        let mut app = TestApp::<ReadingApp>::new().unwrap();
        let answer = app
            .call_by(
                ActionInvoker::agent("reader"),
                Counter::KIND,
                "read-row",
                serde_json::json!({ "row": "row-7" }),
            )
            .unwrap();
        assert_eq!(
            answer.value,
            Some(serde_json::json!({ "count": 1, "what": "row-7" })),
            "the row names what it read"
        );
    }
}

/// What a read action answers, as the surface publishes it.
#[derive(serde::Serialize, tilo_app::tilo_resource::JsonSchema)]
pub struct Counted {
    /// How many there are.
    pub count: i64,
    /// What they are.
    pub what: String,
}

/// A row of a type the app publishes.
#[derive(tilo_app::tilo_resource::Row)]
#[row(kind = "test.counter-row", label = "Counter row", effects = ReadEffect)]
pub struct CounterRow;

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.surface-report.effects", catalogue = "tests/locales")]
pub enum ReadEffect {
    /// Count what the record holds.
    #[effect(kind = "read", returns = Counted, idempotent)]
    Count,
    /// Read the row it is offered on.
    #[effect(kind = "read", on = CounterRow, returns = Counted)]
    ReadRow {
        #[param("The row's id")]
        row: String,
    },
    /// Show the row it is offered on.
    #[effect(screen, on = CounterRow)]
    OpenRow {
        #[param("The row's id")]
        row: String,
    },
    /// Zoom the window in.
    #[effect(view)]
    ZoomIn,
}

struct ReadingApp;

impl ResourceApp for ReadingApp {
    type Resource = Counter;
    const ID: &'static str = "so.tilo.test-surface-reads";
    const TITLE: &'static str = "Counter";

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        cx.effects(|effect: ReadEffect, _cx| match effect {
            ReadEffect::Count => Ok(Effected::value(Counted {
                count: 0,
                what: String::new(),
            })),
            ReadEffect::ReadRow { row } => Ok(Effected::value(Counted {
                count: 1,
                what: row,
            })),
            ReadEffect::OpenRow { .. } | ReadEffect::ZoomIn => Ok(Effected::done()),
        });
        Box::new(cx.button("bump", Counter::BUMP.invoke()))
    }
}
