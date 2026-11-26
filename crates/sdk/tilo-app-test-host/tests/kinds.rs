//! A framework app's kind publishes, for a host with no window, the document
//! operations the running app publishes, under the same addresses and the
//! name the app gives the open resource.

use tilo_app::prelude::*;

#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Tally {
    pub name: String,
    pub count: i64,
}

#[resource(
    kind = "test.tally",
    app = "so.tilo.test-tally",
    catalogue = "tests/locales"
)]
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
