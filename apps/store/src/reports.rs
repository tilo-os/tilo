//! The Store document: collaborative report drafts, their notes and the
//! moderation case each one became.
//!
//! Everything else the Store shows — the catalogue, what is installed, the
//! Flatpak remotes — is live device state its surface publishes as rows.
//! A report's note is typed like any text field
//! (`Reports::paths().reports.key(app).note`), merging with a collaborator
//! typing in it; starting a report and recording the registry's case are
//! the verbs.

use std::collections::BTreeMap;

use ui::tilo_resource::{self, Model, Refusal, resource};

/// The status a report has until the registry knows of it.
pub const DRAFT_STATUS: &str = "Not submitted";
/// How long a report's note may be, in characters.
pub const MAX_REPORT_NOTE_CHARS: usize = 4_096;
const MAX_REPORT_STATUS_CHARS: usize = 128;
const MAX_CASE_ID_CHARS: usize = 128;

pub(crate) fn valid_app_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 255
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-')
}

pub(crate) fn valid_case_id(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_CASE_ID_CHARS
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '.' || ch == '-' || ch == ':')
}

/// One app's report: a draft until the registry knows of it, then its case.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
#[model(check = Report::valid)]
pub struct Report {
    /// Status
    ///
    /// "Not submitted" until the registry answers, then the case's status.
    #[model(readonly)]
    pub status: String,
    /// Concern
    ///
    /// What happened, as evidence another reviewer can verify.
    pub note: String,
    /// The registry's case id, once there is one.
    #[model(readonly)]
    pub case: Option<String>,
}

impl Report {
    fn valid(&self) -> Result<(), Refusal> {
        if self.note.chars().count() > MAX_REPORT_NOTE_CHARS {
            return Err(Refusal::new(format!(
                "a report note holds at most {MAX_REPORT_NOTE_CHARS} characters"
            )));
        }
        Ok(())
    }

    /// The registry's case id, once it has one.
    pub fn case_id(&self) -> Option<&str> {
        self.case.as_deref().filter(|case| valid_case_id(case))
    }
}

/// The reports, by the id of the app each is about.
#[derive(Model, Clone, Debug, Default, PartialEq, Eq)]
#[model(check = Reports::valid)]
pub struct Reports {
    /// Reports
    pub reports: BTreeMap<String, Report>,
}

#[resource(kind = "store.catalog", label = "App Store", search = Reports::search)]
impl Reports {
    /// Create report draft.
    ///
    /// Start a collaborative report about an app, with a note to describe
    /// it. A report the app already has keeps its note.
    #[action]
    pub fn draft_report(
        &mut self,
        #[param("App identity (for example so.tilo.notes)")] app_id: String,
    ) -> Result<(), Refusal> {
        let app_id = app_id.trim();
        if !valid_app_id(app_id) {
            return Err(Refusal::new("invalid app id for report draft"));
        }
        let report = self.reports.entry(app_id.to_owned()).or_default();
        DRAFT_STATUS.clone_into(&mut report.status);
        Ok(())
    }

    /// Record a moderation case.
    ///
    /// The case the app's registry opened for its report, and its status.
    #[action(external)]
    pub fn record_moderation_case(
        &mut self,
        #[param("App identity")] app_id: String,
        #[param("The registry's case id")] case_id: String,
        #[param("The case's status")] status: String,
    ) -> Result<(), Refusal> {
        let app_id = app_id.trim();
        let case_id = case_id.trim();
        if !valid_app_id(app_id)
            || !valid_case_id(case_id)
            || status.is_empty()
            || status.chars().count() > MAX_REPORT_STATUS_CHARS
        {
            return Err(Refusal::new("invalid moderation case record"));
        }
        let report = self.reports.entry(app_id.to_owned()).or_default();
        report.case = Some(case_id.to_owned());
        report.status = status;
        Ok(())
    }
}

impl Reports {
    fn valid(&self) -> Result<(), Refusal> {
        if self.reports.keys().any(|app_id| !valid_app_id(app_id)) {
            return Err(Refusal::new("a report is about an app by its id"));
        }
        Ok(())
    }

    /// Each report, found by the app it is about and its status.
    fn search(&self) -> Vec<tilo_resource::SearchEntry> {
        self.reports
            .iter()
            .map(|(app_id, report)| {
                tilo_resource::SearchEntry::new(
                    format!("Report for {app_id}"),
                    report.status.clone(),
                )
                .anchored(report_row_id(app_id, report))
            })
            .collect()
    }

    /// The report about `app_id`.
    pub fn report(&self, app_id: &str) -> Option<&Report> {
        self.reports.get(app_id)
    }

    /// The path of the note of `app_id`'s report.
    pub fn note(app_id: &str) -> tilo_resource::Path<Self, String> {
        Self::paths().reports.key(app_id).note
    }
}

/// The id of the live row a report is published as: its case once the
/// registry has one, else `report:<app id>`.
pub fn report_row_id(app_id: &str, report: &Report) -> String {
    report
        .case_id()
        .map_or_else(|| format!("report:{app_id}"), str::to_owned)
}

#[cfg(test)]
mod tests;
