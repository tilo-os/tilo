//! The records as resources: the list as the root's reading, each record's
//! text as its own.
//!
//! Settings reads them through the host's door
//! ([`tilo_actions::remote::crash_records`]) and so does an agent or a
//! script; the session's own reader reads the same facts as a service.
//! Nothing here draws.

use std::path::PathBuf;

use serde_json::Value;
use tilo_actions::remote::crash_records::{CrashKind, CrashRecord, CrashRecordText, StopReason};
use tilo_actions::{ActionAddress, DoorError, ResourcePresentation, ResourceSpec};
use tilo_service::{Service, reading};

/// The resource every record is listed under.
pub const CRASH_RECORDS_RESOURCE: &str = "crash-records";
/// The kind of [`CRASH_RECORDS_RESOURCE`].
pub const CRASH_RECORDS_KIND: &str = "session.crash-records";
/// The kind of each record's resource.
pub const CRASH_RECORD_KIND: &str = "session.crash-record";

/// What a record's resource id starts with, before its name.
const RECORD_PREFIX: &str = "crash-record:";

/// The resource id the record `id` is published as.
#[must_use]
pub fn record_resource(id: &str) -> String {
    format!("{RECORD_PREFIX}{id}")
}

/// What a person calls a record of `kind`, for a reader with no words of its
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

/// A record's facts as its resource's presentation: when it happened, and
/// what stopped the computer as its badge.
fn present(record: &CrashRecord) -> ResourcePresentation {
    ResourcePresentation {
        detail: Some(ui::format::timestamp(&ui::format::UnixSecs(record.at))),
        badge: record.reason.map(|reason| reason_label(reason).to_owned()),
        preview: record.build.as_ref().map(|build| format!("build {build}")),
    }
}

/// What a record's resource is called to a reader with no words of its own.
fn resource(record: &CrashRecord) -> ResourceSpec {
    ResourceSpec::new(
        record_resource(&record.id),
        CRASH_RECORD_KIND,
        label(record.kind),
    )
    .with_presentation(present(record))
}

/// The records in the crash directory, listed and read as resources. The
/// directory is [`crate::directory`]'s unless [`Self::in_dir`] names one.
#[derive(Clone, Debug, Default)]
pub struct CrashRecords {
    dir: Option<PathBuf>,
}

impl CrashRecords {
    /// The records in the person's crash directory.
    #[must_use]
    pub fn new() -> Self {
        Self {
            dir: crate::directory(),
        }
    }

    /// The records in `dir`.
    #[must_use]
    pub fn in_dir(dir: impl Into<PathBuf>) -> Self {
        Self {
            dir: Some(dir.into()),
        }
    }

    /// Every record, newest first.
    fn records(&self) -> Vec<CrashRecord> {
        self.dir.as_deref().map_or_else(Vec::new, crate::list)
    }
}

impl Service for CrashRecords {
    fn resources(&self) -> Vec<ResourceSpec> {
        vec![
            ResourceSpec::new(CRASH_RECORDS_RESOURCE, CRASH_RECORDS_KIND, "Crash records")
                .with_children(self.records().iter().map(resource)),
        ]
    }

    /// The root reads as every record's facts, newest first; a record as
    /// its facts and its text, stacks included.
    fn read(&self, resource: &str) -> Option<Value> {
        if resource == CRASH_RECORDS_RESOURCE {
            return Some(reading(&self.records()));
        }
        let dir = self.dir.as_deref()?;
        let id = resource.strip_prefix(RECORD_PREFIX)?;
        let text: CrashRecordText = crate::read(dir, id).ok()?;
        Some(reading(&text))
    }

    /// A record is read, never changed: there is nothing to invoke.
    fn invoke(
        &mut self,
        resource: &str,
        action: &str,
        _arguments: &serde_json::Map<String, Value>,
    ) -> Result<tilo_service::Started, DoorError> {
        Err(DoorError::Unknown(ActionAddress::new(resource, action)))
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
            crate::write_record(dir.path(), name, body).unwrap();
        }
        dir
    }

    fn read_json(service: &CrashRecords, resource: &str) -> Value {
        service.read(resource).unwrap()
    }

    #[test]
    fn the_root_lists_the_records_and_each_one_reads_as_its_text() {
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
        let service = CrashRecords::in_dir(dir.path());
        let resources = service.resources();
        assert_eq!(resources.len(), 1);
        let children = &resources[0].children;
        assert_eq!(children.len(), 2);
        assert_eq!(
            children[0].id.as_str(),
            "crash-record:2026-10-24-225201-9-freeze.log"
        );
        assert_eq!(children[0].label, "Desktop stopped responding");
        assert_eq!(children[1].label, "Desktop stopped unexpectedly");

        let listed = read_json(&service, CRASH_RECORDS_RESOURCE);
        assert_eq!(listed.as_array().unwrap().len(), 2);
        assert_eq!(listed[0]["kind"], "froze");

        let text = read_json(&service, "crash-record:2026-10-24-225201-9-freeze.log");
        assert_eq!(text["record"]["id"], "2026-10-24-225201-9-freeze.log");
        assert!(text["text"].as_str().unwrap().contains("thread:    412"));
        assert_eq!(text["truncated"], false);
        assert_eq!(service.read("crash-record:no-such.log"), None);
        assert_eq!(service.read("crash-record:../escape.log"), None);
    }

    #[test]
    fn a_computer_that_stopped_reads_as_one() {
        let dir = dir_with(&[(
            "2026-10-24-225201-4f00-stopped.log",
            "tilo computer stopped\n=====================\nreason:    emergency\n",
        )]);
        let service = CrashRecords::in_dir(dir.path());
        let children = service.resources().remove(0).children;
        assert_eq!(children[0].label, "Computer stopped");
        let presentation = children[0]
            .presentation
            .clone()
            .expect("a record says when it happened and what stopped it");
        let at =
            ui::format::NaiveDateTime::parse_from_str("2026-10-24 22:52:01", "%Y-%m-%d %H:%M:%S")
                .expect("the record's time");
        let when = ui::format::timestamp(&at);
        assert_eq!(presentation.detail.as_deref(), Some(when.as_str()));
        assert_eq!(presentation.badge.as_deref(), Some("emergency restart"));
    }

    #[test]
    fn a_reader_with_no_crash_directory_publishes_an_empty_list() {
        let service = CrashRecords::in_dir(Path::new("/nonexistent/tilo/crashes"));
        assert!(service.resources().remove(0).children.is_empty());
        assert_eq!(
            read_json(&service, CRASH_RECORDS_RESOURCE),
            Value::Array(Vec::new())
        );
    }

    #[test]
    fn a_record_is_read_and_never_changed() {
        let dir = dir_with(&[("2026-10-24-225201-9-freeze.log", "text\n")]);
        let mut service = CrashRecords::in_dir(dir.path());
        assert!(
            service
                .invoke(
                    "crash-record:2026-10-24-225201-9-freeze.log",
                    "delete",
                    &serde_json::Map::new()
                )
                .is_err()
        );
    }
}
