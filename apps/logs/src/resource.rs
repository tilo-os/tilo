//! Logs as a resource: the journal filter follows the person and syncs to
//! collaborators; the journal itself is this device's, shown and offered as
//! live rows by [`LogsApp`].
//!
//! Refresh, live tail, export and showing an entry are effects with no
//! document delta ([`JournalEffect`]), answered by the surface.

use std::rc::Rc;

use ui::prelude::*;

use crate::actions::JournalEffect;
use crate::app::LogsApp;
use crate::journal::{JournalLoadError, sample_entries};
use crate::model::{Filter, LogEntry, Priority};

/// The shared journal filter.
#[derive(Model, Default, Debug, Clone, PartialEq)]
pub struct Logs {
    /// The least severe priority shown.
    pub severity: Priority,
    /// Exact unit, or empty for every unit.
    pub unit: String,
    /// Exact boot id, or empty for every boot.
    pub boot: String,
    /// Text to find in messages, units or identifiers.
    pub search: String,
}

impl Logs {
    /// The filter entries pass to be shown.
    pub fn filter(&self) -> Filter {
        Filter {
            min_priority: self.severity.level(),
            unit: self.unit.clone(),
            boot: self.boot.clone(),
            search: self.search.clone(),
        }
    }
}

#[resource(kind = "logs.journal", label = "System journal")]
impl Logs {
    /// Set the severity threshold (0 emergencies only; 7 everything).
    #[action(label = "Set severity threshold")]
    pub fn set_severity(&mut self, #[param("Severity threshold from 0 to 7")] priority: i64) {
        self.severity = Priority::from_level(priority.clamp(0, 7));
    }

    /// Show only the entries of one unit.
    #[action(label = "Filter by unit")]
    pub fn set_unit(&mut self, #[param("Exact unit, or omit to show all")] unit: Option<String>) {
        self.unit = unit.unwrap_or_default();
    }

    /// Show only the entries of one boot.
    #[action(label = "Filter by boot")]
    pub fn set_boot(
        &mut self,
        #[param("Exact boot id, or omit to show all")] boot: Option<String>,
    ) {
        self.boot = boot.unwrap_or_default();
    }

    /// Show only the entries containing some text.
    #[action(label = "Search journal")]
    pub fn set_search(
        &mut self,
        #[param("Text to find in messages, units, or identifiers")] query: Option<String>,
    ) {
        self.search = query.unwrap_or_default();
    }

    /// Clear every journal filter.
    #[action]
    pub fn clear_filters(&mut self) {
        *self = Self::default();
    }
}

/// What a capture shows in place of this device's journal.
pub enum JournalFixture {
    /// These entries, read.
    Entries(Vec<LogEntry>),
    /// A journal that could not be read.
    Failed(JournalLoadError),
    /// A first read still in flight.
    Loading,
}

/// The Logs app.
pub struct LogsShell;

impl ResourceApp for LogsShell {
    type Resource = Logs;
    const ID: &'static str = "so.tilo.logs";
    const TITLE: &'static str = "Logs";
    const ICON: Option<&'static str> = Some("logs");

    fn view(cx: &mut ViewCx<Logs>) -> Element {
        cx.surface(LogsApp::new())
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
            Some("filter-errors") => vec![Logs::SET_SEVERITY.with(Priority::Err.level())],
            _ => Vec::new(),
        }
    }
}

impl ResourceSurface<Logs, JournalEffect> for LogsApp {
    fn present(&mut self, state: &Rc<Logs>) {
        self.adopt_filter(state.filter());
    }

    fn attach(&mut self, cx: SurfaceCx<Logs>) {
        self.attach_framework(&cx);
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.stage_scenario(scenario);
    }

    fn effect(
        &mut self,
        effect: JournalEffect,
        cx: &mut EffectCx<Logs, Self>,
    ) -> Result<Effected, String> {
        self.answer_effect(effect, cx)
    }

    fn effects(&self) -> Vec<EffectStatus> {
        self.effect_statuses()
    }

    fn live_rows(&self) -> Vec<LiveRow> {
        self.entry_rows()
    }

    fn live_row(&self, id: &str) -> Option<LiveRow> {
        self.entry_row(id)
    }
}
