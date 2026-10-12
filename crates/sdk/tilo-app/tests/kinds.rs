#![allow(clippy::unwrap_used, clippy::expect_used, reason = "test assertions")]

//! A framework app's kind publishes, for a host with no window, the document
//! operations the running app publishes, under the same addresses and the
//! name the app gives the open resource.

use tilo_app::framework::resource_kind;
use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Tally {
    pub name: String,
    pub count: i64,
}

#[resource(kind = "test.tally", label = "Tally")]
impl Tally {
    /// Rename the tally.
    #[action]
    pub fn rename(&mut self, name: String) {
        self.name = name;
    }

    /// Count one more.
    #[action]
    pub fn bump(&mut self) {
        self.count = self.count.saturating_add(1);
    }
}

pub struct TallyApp;

impl ResourceApp for TallyApp {
    type Resource = Tally;
    const ID: &'static str = "so.tilo.test-tally";
    const TITLE: &'static str = "Tally";

    fn view(cx: &mut ViewCx<Tally>) -> Element {
        Box::new(cx.button("Bump", Tally::BUMP.invoke()))
    }

    fn label(state: &Tally) -> String {
        format!("Tally of {}", state.name)
    }
}

#[test]
fn a_kind_publishes_the_document_operations_the_running_app_publishes() {
    let mut app = TestApp::<TallyApp>::new().unwrap();
    app.invoke(Tally::RENAME.with("visitors")).unwrap();
    let kind = resource_kind::<TallyApp>();
    assert_eq!(kind.kind(), "test.tally");
    assert_eq!(kind.app_id(), Some("so.tilo.test-tally"));
    assert_eq!(kind.app_title(), Some("Tally"));

    let published = app
        .read_document(|doc| kind.publish(kind.kind(), doc))
        .unwrap();
    assert_eq!(published.label, "Tally of visitors");
    let surface = app.agent_surface();
    let running: Vec<(String, String)> = surface
        .as_array()
        .unwrap()
        .iter()
        .map(|row| {
            (
                row["resource"].as_str().unwrap().to_owned(),
                row["action"].as_str().unwrap().to_owned(),
            )
        })
        .collect();
    let documents: Vec<_> = tilo_actions::door::survey(std::slice::from_ref(&published))
        .into_iter()
        .filter(|descriptor| descriptor.kind == tilo_actions::OperationKind::Document)
        .collect();
    assert!(
        documents
            .iter()
            .any(|descriptor| descriptor.address.action == "bump")
    );
    for descriptor in documents {
        let address = (
            descriptor.address.resource.clone(),
            descriptor.address.action.clone(),
        );
        assert!(
            running.contains(&address),
            "{address:?} is not on the running app's door"
        );
    }
}
