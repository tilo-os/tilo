//! Logs as a resource: the journal filter follows the person and syncs to collaborators;
//! the journal itself is this device's, shown and offered as live rows by the window's
//! view.

use ui::prelude::*;

use crate::journal::{JournalLoadError, sample_entries};
use crate::model::{Filter, LogEntry, Priority};

/// The shared journal filter.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Logs {
    /// The least severe priority shown.
    pub severity: Priority,
    /// Exact unit, or empty for every unit.
    pub unit: Atom,
    /// Exact boot id, or empty for every boot.
    pub boot: Atom,
    /// Text to find in messages, units or identifiers.
    pub search: String,
}

impl Logs {
    /// The filter entries pass to be shown.
    pub fn filter(&self) -> Filter {
        Filter {
            min_priority: self.severity.level(),
            unit: self.unit.0.clone(),
            boot: self.boot.0.clone(),
            search: self.search.clone(),
        }
    }
}

impl Logs {
    /// What the record holds: the shared journal filter: severity, unit, boot and search
    /// text.
    pub fn read_document(
        &self,
        item: Option<&tilo_resource::Id>,
    ) -> Result<serde_json::Value, String> {
        tilo_resource::read::fields(self, item)
    }
}

#[resource(
    kind = "logs.journal",
    read_content = Logs::read_document
)]
impl Logs {
    /// Clear every journal filter.
    #[action]
    pub fn clear_filters(&mut self) {
        *self = Self::default();
    }
}

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
    const ID: &'static str = "so.tilo.logs";
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
            Some("empty") => JournalFixture::Entries(Vec::new()),
            Some("loading") => JournalFixture::Loading,
            _ => JournalFixture::Entries(sample_entries()),
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

ui::reach_tests!(LogsShell);
