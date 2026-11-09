//! What `tilo-app surface` and `tilo-app check` read from an app's binary:
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
    label = "Counter",
    app = "so.tilo.test-surface-good"
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
    label = "Counter preferences",
    app = "so.tilo.test-surface-good"
)]
impl Prefs {}

struct GoodApp;

impl ResourceApp for GoodApp {
    type Resource = Counter;
    const ID: &'static str = "so.tilo.test-surface-good";
    const TITLE: &'static str = "Counter";

    fn view(cx: &mut ViewCx<Counter>) -> Element {
        let _prefs = cx.person::<Prefs>();
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

#[test]
fn the_surface_report_names_verbs_fields_and_unpublished_bindings() {
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
    assert!(
        report["resources"][1].get("person").is_none(),
        "kept in the workspace the app is open in, like its own: {report}"
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

/// What a read action answers, as the surface publishes it.
#[derive(serde::Serialize, tilo_app::tilo_resource::JsonSchema)]
pub struct Counted {
    /// How many there are.
    pub count: i64,
    /// What they are.
    pub what: String,
}

#[derive(Clone, Debug, PartialEq, Effect)]
#[effect(operation = "test.surface-report.effects")]
pub enum ReadEffect {
    /// Count what the record holds.
    #[effect(kind = "read", returns = Counted, idempotent)]
    Count,
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
            ReadEffect::ZoomIn => Ok(Effected::done()),
        })
    }
}

/// A read effect is an action of the app's own resource in its surface, with
/// the value it answers with; what the window shows of itself is no action a
/// caller of another language has.
#[test]
fn a_read_effect_publishes_its_answer_as_an_action_of_the_resource() {
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
