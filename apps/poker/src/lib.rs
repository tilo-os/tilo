//! Poker: Texas hold'em for everyone in the workspace.

pub const APP_ID: &str = "so.tilo.poker";
pub const APP_TITLE: &str = "Poker";

pub mod card;
pub mod cards;
pub mod game;
pub mod locales;
#[cfg(feature = "samples")]
mod sample;
pub mod view;

use ui::prelude::*;

pub use game::Table;

pub struct App;

impl ResourceApp for App {
    type Resource = Table;
    const ID: &'static str = crate::APP_ID;
    const TITLE: &'static str = crate::APP_TITLE;
    const TITLE_KEY: &'static str = "poker-app-title";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    fn shot_viewport(_scenario: Option<&str>) -> (u32, u32) {
        (900, 760)
    }

    fn label(table: &Table) -> String {
        view::location(table)
    }

    #[cfg(feature = "samples")]
    fn scenarios() -> &'static [&'static str] {
        sample::SCENARIOS
    }

    /// A capture cannot play twelve seconds into a turn, so the scenario stages the table
    /// it reached as the record's start.
    #[cfg(feature = "samples")]
    fn fixture(fixture: &mut ui::framework::Fixture<Table>) {
        let Some(scenario) = fixture.scenario() else {
            return;
        };
        fixture.start(sample::table(scenario, fixture.person()));
    }

    fn view(cx: &mut ViewCx<Table>) -> Element {
        let names = cx.people().revision();
        cx.switch(&names, |cx, _| {
            let table = cx.value(Table::clone);
            cx.switch(&table, view::table)
        })
    }
}

ui::dev_reload_entry!(App);
