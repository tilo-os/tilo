//! The reports as resources: the list as the root's reading, each report's
//! text as its own.
//!
//! The session publishes them to the apps it runs, Settings first among
//! them: an app reads [`wire::RESOURCE`] for every report and
//! [`wire::report`] of one for its text, and so does an agent or a script
//! through the same action API. Nothing here draws; the one change a report takes is its
//! removal from this computer.

use std::path::PathBuf;

use crate::reports::{self as wire, CrashKind, CrashReport, CrashReportText, StopReason};
use serde_json::Value;
use tilo_actions::{ApiError, ResourcePresentation, ResourceSpec};
use tilo_service::{Service, reading};

/// The kind of [`wire::RESOURCE`].
pub const CRASH_REPORTS_KIND: &str = "session.crash-reports";
/// The kind of each report's resource.
pub const CRASH_REPORT_KIND: &str = "session.crash-report";

/// What a person calls a report of `kind`, for a reader with no words of its
/// own (an agent's list, a settings row read aloud).
const fn label(kind: CrashKind) -> &'static str {
    match kind {
        CrashKind::Froze => "Desktop stopped responding",
        CrashKind::Crashed => "Desktop stopped unexpectedly",
        CrashKind::PartRestarted => "Part of the desktop restarted",
        CrashKind::ComputerStopped => "Computer stopped",
        CrashKind::SystemFault => "System fault",
    }
}

/// Why the system stopped or what it met, in a word.
const fn reason_label(reason: StopReason) -> &'static str {
    match reason {
        StopReason::Panic => "panic",
        StopReason::Oops => "oops",
        StopReason::Emergency => "emergency restart",
    }
}

/// A report's facts as its resource's presentation: when it happened, and
/// what stopped the computer as its badge.
fn present(report: &CrashReport) -> ResourcePresentation {
    ResourcePresentation {
        detail: Some(tilo_ui_base::format::timestamp(
            &tilo_ui_base::format::UnixSecs(report.at),
        )),
        badge: report.reason.map(|reason| reason_label(reason).to_owned()),
        preview: report.build.as_ref().map(|build| format!("build {build}")),
    }
}

/// What a person asks of the reports: nothing but removing one from this computer.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "tilo.crash-reports")]
pub enum CrashReportsAction {
    /// Remove
    ///
    /// Delete this report from this computer. Nothing else keeps it: a report that is removed is
    /// gone, the notice that told the person it was here included.
    #[effect(kind = "outside")]
    #[effect(own_device, idempotent)]
    Remove {
        /// The report, by its id as the list answers it.
        #[param("The report, by its id as the list answers it")]
        report: String,
    },
}

/// The listed reports as one resource of a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "session.crash-reports", label = "Crash reports")]
pub struct CrashReportsList;

/// One report as a resource of a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "session.crash-report", label = "Crash report")]
pub struct CrashReportResource;

/// `report` with the time it happened as this computer reads a clock.
fn stated(report: &CrashReport) -> CrashReport {
    CrashReport {
        when: tilo_ui_base::format::timestamp(&tilo_ui_base::format::UnixSecs(report.at)),
        ..report.clone()
    }
}

/// What a report's resource is called to a reader with no words of its own.
fn resource(report: &CrashReport) -> ResourceSpec {
    tilo_resource::typed_resource::<CrashReportResource>(
        wire::report(&report.id),
        label(report.kind),
    )
    .with_presentation(present(report))
}

/// The reports in the crash directory, listed and read as resources. The
/// directory is [`crate::directory`]'s unless [`Self::in_dir`] names one.
#[derive(Clone, Debug, Default)]
pub struct CrashReports {
    dir: Option<PathBuf>,
}

impl CrashReports {
    /// The reports in the person's crash directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dir: crate::directory(),
        }
    }

    /// The reports in `dir`.
    #[must_use]
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    /// Every report, newest first, as a view reads them: each with the time it happened in the
    /// person's own words beside the seconds a machine reads.
    fn reports(&self) -> Vec<CrashReport> {
        self.dir.as_deref().map_or_else(Vec::new, |dir| {
            crate::list(dir).iter().map(stated).collect()
        })
    }
}

impl Service for CrashReports {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![tilo_resource::offer::<CrashReportsAction>(
            tilo_resource::typed_resource::<CrashReportsList>(wire::RESOURCE, "Crash reports")
                .with_children(self.reports().iter().map(resource)),
        )]
    }

    /// The root reads as every report's facts, newest first; a report as
    /// its facts and its text, stacks included.
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == wire::RESOURCE {
            return Some(serde_json::json!({ "reports": self.reports() }));
        }
        let dir = self.dir.as_deref()?;
        let id = resource.strip_prefix(wire::REPORT_PREFIX)?;
        let mut text: CrashReportText = crate::read(dir, id).ok()?;
        text.report = stated(&text.report);
        Some(reading(&text))
    }

    /// The list's one verb removes a report from this computer; nothing else changes.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, Value>,
    ) -> Result<tilo_service::Started, ApiError> {
        let published = self.resources();
        let ran =
            tilo_resource::decode::<CrashReportsAction>(&published, resource, action, arguments)?;
        match ran {
            CrashReportsAction::Remove { report } => {
                let dir = self.dir.as_deref().ok_or_else(|| ApiError::Unavailable {
                    code: "crash_reports_unkept".to_owned(),
                    reason: "this computer keeps no crash reports".to_owned(),
                })?;
                crate::remove(dir, &report).map_err(ApiError::Failed)?;
                Ok(tilo_service::Started::Done(
                    tilo_actions::ActionOutput::default(),
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    fn dir_with(names: &[(&str, &str)]) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        tilo_files::native::private::directory(dir.path()).unwrap();
        for (name, body) in names {
            crate::write_report(dir.path(), name, body).unwrap();
        }
        dir
    }

    fn read_json(service: &CrashReports, resource: &str) -> Value {
        service.read(resource).unwrap()
    }

    tilo_test_env::scenario! {
        fn the_crash_reports_service_publishes_the_reports_to_read;

        phase the_root_lists_the_reports_and_each_one_reads_as_its_text;
        {
            let dir = dir_with(&[
                (
                    "2026-10-24-225201-9-freeze.log",
                    "tilo session freeze\n===================\nversion:   0.1.0\nthread:    412\n",
                ),
                (
                    "2026-10-23-080000-5.log",
                    "tilo session panic\n==================\nversion:   0.1.0\n",
                ),
            ]);
            let mut service = CrashReports::in_dir(dir.path());
            let resources = service.resources();
            assert_eq!(resources.len(), 1);
            let children = &resources[0].children;
            assert_eq!(children.len(), 2);
            assert_eq!(
                children[0].id.as_str(),
                "crash-report:2026-10-24-225201-9-freeze.log"
            );
            assert_eq!(children[0].label, "Desktop stopped responding");
            assert_eq!(children[1].label, "Desktop stopped unexpectedly");

            let listed = read_json(&service, wire::RESOURCE);
            let reports = listed["reports"].as_array().unwrap();
            assert_eq!(reports.len(), 2);
            assert_eq!(reports[0]["kind"], "froze");
            assert!(
                reports[0]["when"].as_str().is_some_and(|when| !when.is_empty()),
                "a report says when it happened in the person's words: {listed:#?}"
            );

            let text = read_json(&service, "crash-report:2026-10-24-225201-9-freeze.log");
            assert_eq!(text["report"]["id"], "2026-10-24-225201-9-freeze.log");
            assert!(text["text"].as_str().unwrap().contains("thread:    412"));
            assert_eq!(text["truncated"], false);
            assert_eq!(
                text["report"]["when"], reports[0]["when"],
                "one report's page and the list say the same time"
            );
            assert_eq!(service.read("crash-report:no-such.log"), None);
            assert_eq!(service.read("crash-report:../escape.log"), None);

            // The one change a report takes: it goes off this computer, and the list says so.
            let arguments = |report: &str| {
                serde_json::Map::from_iter([("report".to_owned(), serde_json::json!(report))])
            };
            assert!(
                service
                    .invoke(wire::RESOURCE, "remove", &arguments("../escape.log"))
                    .is_err(),
                "a name that is not one of this computer's reports removes nothing"
            );
            assert!(
                matches!(
                    service.invoke(
                        wire::RESOURCE,
                        "remove",
                        &arguments("2026-10-24-225201-9-freeze.log")
                    ),
                    Ok(tilo_service::Started::Done(_))
                ),
                "the person removes the record"
            );
            assert!(!dir.path().join("2026-10-24-225201-9-freeze.log").exists());
            let listed = read_json(&service, wire::RESOURCE);
            assert_eq!(listed["reports"].as_array().unwrap().len(), 1);
            assert!(dir.path().join("2026-10-23-080000-5.log").exists());
        }

        phase a_computer_that_stopped_reads_as_one;
        {
            let dir = dir_with(&[(
                "2026-10-24-225201-4f00-stopped.log",
                "tilo computer stopped\n=====================\nreason:    emergency\n",
            )]);
            let service = CrashReports::in_dir(dir.path());
            let children = service.resources().remove(0).children;
            assert_eq!(children[0].label, "Computer stopped");
            let presentation = children[0]
                .presentation
                .clone()
                .expect("a report says when it happened and what stopped it");
            let at =
                tilo_ui_base::format::DateTime::strptime("%Y-%m-%d %H:%M:%S", "2026-10-24 22:52:01")
                    .expect("the report's time");
            let when = tilo_ui_base::format::timestamp(&at);
            assert_eq!(presentation.detail.as_deref(), Some(when.as_str()));
            assert_eq!(presentation.badge.as_deref(), Some("emergency restart"));
        }

        phase a_reader_with_no_crash_directory_publishes_an_empty_list;
        {
            let service = CrashReports::in_dir(Path::new("/nonexistent/tilo/crashes"));
            assert!(service.resources().remove(0).children.is_empty());
            assert_eq!(
                read_json(&service, wire::RESOURCE),
                serde_json::json!({ "reports": [] })
            );
        }

        phase a_report_is_read_and_never_changed;
        {
            let dir = dir_with(&[("2026-10-24-225201-9-freeze.log", "text\n")]);
            let mut service = CrashReports::in_dir(dir.path());
            assert!(
                service
                    .invoke(
                        "crash-report:2026-10-24-225201-9-freeze.log",
                        "delete",
                        &serde_json::Map::new()
                    )
                    .is_err()
            );
        }
    }
}
