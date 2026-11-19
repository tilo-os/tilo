//! The reports as resources: the list as the root's reading, each report's
//! text as its own.
//!
//! The session publishes them to the apps it runs, Settings first among
//! them: an app reads [`wire::RESOURCE`] for every report and
//! [`wire::report`] of one for its text, and so does an agent or a script
//! through the same action API. Nothing here draws, and nothing changes a report.

use std::path::PathBuf;

use serde_json::Value;
use tilo_actions::remote::crash_reports::{
    self as wire, CrashKind, CrashReport, CrashReportText, StopReason,
};
use tilo_actions::{ActionAddress, ApiError, ResourcePresentation, ResourceSpec};
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
        detail: Some(ui::format::timestamp(&ui::format::UnixSecs(report.at))),
        badge: report.reason.map(|reason| reason_label(reason).to_owned()),
        preview: report.build.as_ref().map(|build| format!("build {build}")),
    }
}

/// The listed reports as one resource of a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "session.crash-reports", label = "Crash reports")]
pub struct CrashReportsList;

/// One report as a resource of a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "session.crash-report", label = "Crash report")]
pub struct CrashReportResource;

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

    /// Every report, newest first.
    fn reports(&self) -> Vec<CrashReport> {
        self.dir.as_deref().map_or_else(Vec::new, crate::list)
    }
}

impl Service for CrashReports {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            tilo_resource::typed_resource::<CrashReportsList>(wire::RESOURCE, "Crash reports")
                .with_children(self.reports().iter().map(resource)),
        ]
    }

    /// The root reads as every report's facts, newest first; a report as
    /// its facts and its text, stacks included.
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == wire::RESOURCE {
            return Some(reading(&self.reports()));
        }
        let dir = self.dir.as_deref()?;
        let id = resource.strip_prefix(wire::REPORT_PREFIX)?;
        let text: CrashReportText = crate::read(dir, id).ok()?;
        Some(reading(&text))
    }

    /// A report is read, never changed: there is nothing to invoke.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &serde_json::Map<String, Value>,
    ) -> Result<tilo_service::Started, ApiError> {
        Err(ApiError::Unknown(ActionAddress::new(resource, action)))
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
            let service = CrashReports::in_dir(dir.path());
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
            assert_eq!(listed.as_array().unwrap().len(), 2);
            assert_eq!(listed[0]["kind"], "froze");

            let text = read_json(&service, "crash-report:2026-10-24-225201-9-freeze.log");
            assert_eq!(text["report"]["id"], "2026-10-24-225201-9-freeze.log");
            assert!(text["text"].as_str().unwrap().contains("thread:    412"));
            assert_eq!(text["truncated"], false);
            assert_eq!(service.read("crash-report:no-such.log"), None);
            assert_eq!(service.read("crash-report:../escape.log"), None);
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
                ui::format::DateTime::strptime("%Y-%m-%d %H:%M:%S", "2026-10-24 22:52:01")
                    .expect("the report's time");
            let when = ui::format::timestamp(&at);
            assert_eq!(presentation.detail.as_deref(), Some(when.as_str()));
            assert_eq!(presentation.badge.as_deref(), Some("emergency restart"));
        }

        phase a_reader_with_no_crash_directory_publishes_an_empty_list;
        {
            let service = CrashReports::in_dir(Path::new("/nonexistent/tilo/crashes"));
            assert!(service.resources().remove(0).children.is_empty());
            assert_eq!(
                read_json(&service, wire::RESOURCE),
                Value::Array(Vec::new())
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
