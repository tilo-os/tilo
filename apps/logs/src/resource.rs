//! The app on the shared filter: `logs-model` keeps the resource and its verb; the
//! journal itself is this device's, shown and offered as live rows by the window's
//! view.

use ui::prelude::*;

use crate::journal::{JournalLoadError, sample_entries};
use crate::model::{LogEntry, Priority};

pub use logs_model::Logs;

/// What a capture shows in place of this device's journal.
pub enum JournalFixture {
    Entries(Vec<LogEntry>),
    Failed(JournalLoadError),
    Loading,
}

/// The Logs app.
pub struct LogsShell;

impl ResourceApp for LogsShell {
    type Resource = Logs;
    const ID: &'static str = ui::apps::LOGS;
    const READS_HOST: &'static [ui::launch::HostSource] = &[ui::launch::HostSource::Journal];
    const TITLE: &'static str = "Logs";
    const TITLE_KEY: &'static str = "logs-app-title";

    fn catalog() -> Option<&'static ui::i18n::EmbeddedCatalog> {
        Some(&crate::locales::CATALOG)
    }

    const ICON: Option<&'static str> = Some("logs");

    fn view(cx: &mut ViewCx<Logs>) -> Element {
        crate::window::window(cx)
    }

    fn scenarios() -> &'static [&'static str] {
        &[
            "detail",
            "error",
            "empty",
            "filter-errors",
            "loading",
            "actions",
        ]
    }

    fn shot_viewport(scenario: Option<&str>) -> (u32, u32) {
        match scenario {
            Some("detail" | "actions") => (390, 844),
            _ => (1280, 800),
        }
    }

    fn fixture(fixture: &mut Fixture<Logs>) {
        fixture.provide(match fixture.scenario() {
            Some("error") => JournalFixture::Failed(JournalLoadError::AccessDenied),
            // A plain capture shows the app as it opens: no journal read has answered yet.
            // The sample rows stand in only for a capture that names the scene showing them.
            None | Some("empty") => JournalFixture::Entries(Vec::new()),
            Some("loading") => JournalFixture::Loading,
            Some(_) => JournalFixture::Entries(sample_entries()),
        });
    }

    fn scenario(name: Option<&str>) -> Vec<Invoke<Logs>> {
        match name {
            Some("filter-errors") => Invoke::edit(&Logs::paths().severity.set(Priority::Err))
                .into_iter()
                .collect(),
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
tilo_app_test_host::reach_tests!(LogsShell);
