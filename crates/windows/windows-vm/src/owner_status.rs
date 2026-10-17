//! The unprivileged owner's read-only appliance summary. The appliance state
//! is group-readable by the normal Tilo owner; callers without that ordinary
//! read access receive a typed unavailable result, never a guess.
use std::fs::{File, Metadata, OpenOptions};
use std::io::{self, Read as _};
use std::os::unix::fs::{MetadataExt as _, OpenOptionsExt as _};
use std::path::Path;

use serde::Serialize;
use serde_json::Value;

const PHASES: [&str; 5] = [
    "not_provisioned",
    "installing",
    "awaiting_guest_bridge",
    "ready",
    "repair_required",
];
const LICENSES: [&str; 6] = [
    "unknown",
    "digitally_activated",
    "product_key_activated",
    "organisation_managed",
    "evaluation",
    "unlicensed",
];
const POWER_STATES: [&str; 5] = ["running", "suspended", "paused", "shut_down", "unknown"];
const EGRESS_PURPOSES: [&str; 2] = ["activation", "windows_update"];

/// Fields are in sorted order: the summary's wire form sorts its keys.
#[derive(Debug, Serialize)]
struct OwnerStatus {
    catalog_generation: u64,
    configured: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    detail: Option<String>,
    license: String,
    phase: String,
    power_state: String,
    published_applications: usize,
    recovery_available: bool,
    schema_version: u8,
    temporary_egress: Option<TemporaryEgress>,
}

#[derive(Debug, Serialize)]
struct TemporaryEgress {
    expires_at_unix: u64,
    purpose: String,
}

enum Failure {
    /// A missing file is an absent appliance or runtime, not an error.
    Missing,
    Invalid(String),
}

impl From<&str> for Failure {
    fn from(message: &str) -> Self {
        Self::Invalid(message.to_string())
    }
}

fn os_error(error: &io::Error, path: &Path) -> Failure {
    if error.kind() == io::ErrorKind::NotFound {
        return Failure::Missing;
    }
    let text = error.to_string();
    match error.raw_os_error() {
        Some(code) => {
            let reason = text
                .strip_suffix(&format!(" (os error {code})"))
                .unwrap_or(&text);
            Failure::Invalid(format!("[Errno {code}] {reason}: '{}'", path.display()))
        }
        None => Failure::Invalid(text),
    }
}

/// Open `path` without following a final link and parse it as JSON, refusing
/// a file swapped between the inspection and the open.
fn read_json(path: &Path, inspected: &Metadata, changed: &str) -> Result<Value, Failure> {
    let mut file: File = OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(path)
        .map_err(|error| os_error(&error, path))?;
    let opened = file.metadata().map_err(|error| os_error(&error, path))?;
    if opened.dev() != inspected.dev() || opened.ino() != inspected.ino() {
        return Err(changed.into());
    }
    let mut bytes = Vec::new();
    file.read_to_end(&mut bytes)
        .map_err(|error| os_error(&error, path))?;
    serde_json::from_slice(&bytes).map_err(|error| Failure::Invalid(error.to_string()))
}

/// A unit-variant name, or the single key of a tagged variant.
fn variant<'a>(value: &'a Value, allowed: &[&str], invalid: &str) -> Result<&'a str, Failure> {
    let name = match value {
        Value::String(name) => name.as_str(),
        Value::Object(tagged) if tagged.len() == 1 => {
            tagged.keys().next().map_or("", String::as_str)
        }
        _ => "",
    };
    if allowed.contains(&name) {
        Ok(name)
    } else {
        Err(invalid.into())
    }
}

fn is_truthy(value: &Value) -> bool {
    match value {
        Value::Null => false,
        Value::Bool(flag) => *flag,
        Value::Number(number) => number.as_f64() != Some(0.0),
        Value::String(text) => !text.is_empty(),
        Value::Array(items) => !items.is_empty(),
        Value::Object(fields) => !fields.is_empty(),
    }
}

fn read_appliance(status: &mut OwnerStatus, state_path: &Path) -> Result<(), Failure> {
    let info =
        std::fs::symlink_metadata(state_path).map_err(|error| os_error(&error, state_path))?;
    if !info.file_type().is_file() {
        return Err("state is not a regular file".into());
    }
    if info.mode() & 0o007 != 0 {
        return Err("state is world-accessible".into());
    }
    let source = read_json(state_path, &info, "state changed while being read")?;
    if source.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("unsupported state schema".into());
    }
    let phase = match source.get("phase") {
        None => "not_provisioned",
        Some(phase) => variant(phase, &PHASES, "invalid appliance phase")?,
    };
    let license = match source.get("license") {
        None => "unknown",
        Some(license) => variant(license, &LICENSES, "invalid licence state")?,
    };
    let generation = match source.get("catalog_generation") {
        None => 0,
        Some(generation) => generation.as_u64().ok_or("invalid catalogue generation")?,
    };
    let applications = match source
        .get("cached_catalog")
        .filter(|catalog| is_truthy(catalog))
    {
        Some(Value::Object(catalog)) => match catalog.get("applications") {
            None => 0,
            Some(Value::Array(applications)) => applications.len(),
            Some(_) => return Err("invalid cached catalogue".into()),
        },
        _ => 0,
    };
    let configured = phase != "not_provisioned";
    status.configured = configured;
    status.phase = phase.to_string();
    status.license = license.to_string();
    status.catalog_generation = generation;
    status.published_applications = applications;
    status.recovery_available = configured;
    Ok(())
}

fn read_runtime(
    status: &mut OwnerStatus,
    status_path: &Path,
    root_uid: u32,
) -> Result<(), Failure> {
    let info =
        std::fs::symlink_metadata(status_path).map_err(|error| os_error(&error, status_path))?;
    if !info.file_type().is_file() || info.uid() != root_uid || info.mode() & 0o007 != 0 {
        return Err("Windows runtime status summary is unsafe".into());
    }
    let domain = read_json(
        status_path,
        &info,
        "Windows runtime status changed while being read",
    )?;
    let power_state = domain
        .get("power_state")
        .and_then(Value::as_str)
        .filter(|state| POWER_STATES.contains(state));
    let (Some(1), Some(power_state)) = (
        domain.get("schema_version").and_then(Value::as_u64),
        power_state,
    ) else {
        return Err("invalid authoritative Windows runtime status".into());
    };
    status.power_state = power_state.to_string();
    match domain.get("temporary_egress") {
        None | Some(Value::Null) => {}
        Some(lease) => {
            let purpose = lease
                .get("purpose")
                .and_then(Value::as_str)
                .filter(|purpose| EGRESS_PURPOSES.contains(purpose));
            let expires = lease.get("expires_at_unix").and_then(Value::as_u64);
            let (Some(purpose), Some(expires_at_unix)) = (purpose, expires) else {
                return Err("invalid temporary Windows egress status".into());
            };
            status.temporary_egress = Some(TemporaryEgress {
                expires_at_unix,
                purpose: purpose.to_string(),
            });
        }
    }
    Ok(())
}

/// The owner's typed summary as one compact JSON line with sorted keys.
///
/// The appliance state must be a regular, non-world-accessible file; the
/// runtime status must also be owned by `root_uid` (root in production). A
/// missing file leaves the defaults; anything unsafe or malformed reports the
/// phase `unavailable` with a `detail`, keeping what was already read.
#[must_use]
pub fn owner_status_json(state_path: &Path, status_path: &Path, root_uid: u32) -> String {
    let mut status = OwnerStatus {
        catalog_generation: 0,
        configured: false,
        detail: None,
        license: "unknown".into(),
        phase: "not_provisioned".into(),
        power_state: "unknown".into(),
        published_applications: 0,
        recovery_available: false,
        schema_version: 1,
        temporary_egress: None,
    };
    let outcome = read_appliance(&mut status, state_path)
        .and_then(|()| read_runtime(&mut status, status_path, root_uid));
    if let Err(Failure::Invalid(detail)) = outcome {
        status.phase = "unavailable".into();
        status.detail = Some(detail);
    }
    // Every field is a plain string, number or bool, so encoding cannot fail.
    serde_json::to_string(&status).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt as _;

    struct Fixture {
        dir: tempfile::TempDir,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                dir: tempfile::tempdir().expect("fixture directory"),
            }
        }

        fn state(&self) -> std::path::PathBuf {
            self.dir.path().join("state.json")
        }

        fn runtime(&self) -> std::path::PathBuf {
            self.dir.path().join("status.json")
        }

        fn write(path: &Path, contents: &str, mode: u32) {
            std::fs::write(path, contents).expect("write fixture");
            std::fs::set_permissions(path, std::fs::Permissions::from_mode(mode))
                .expect("fixture mode");
        }

        fn summary(&self) -> String {
            owner_status_json(
                &self.state(),
                &self.runtime(),
                rustix::process::geteuid().as_raw(),
            )
        }

        fn summary_owned_by(&self, uid: u32) -> String {
            owner_status_json(&self.state(), &self.runtime(), uid)
        }
    }

    const READY: &str = r#"{"schema_version":1,"phase":"ready","license":{"digitally_activated":{"since":1}},"catalog_generation":4,"cached_catalog":{"applications":[{},{}]}}"#;
    const READY_SUMMARY: &str = r#"{"catalog_generation":4,"configured":true,"license":"digitally_activated","phase":"ready","power_state":"unknown","published_applications":2,"recovery_available":true,"schema_version":1,"temporary_egress":null}"#;

    #[test]
    fn missing_state_is_an_unprovisioned_appliance() {
        let fixture = Fixture::new();
        assert_eq!(
            fixture.summary(),
            r#"{"catalog_generation":0,"configured":false,"license":"unknown","phase":"not_provisioned","power_state":"unknown","published_applications":0,"recovery_available":false,"schema_version":1,"temporary_egress":null}"#
        );
    }

    #[test]
    fn missing_runtime_status_keeps_management_state_with_unknown_power() {
        let fixture = Fixture::new();
        Fixture::write(&fixture.state(), READY, 0o640);
        assert_eq!(fixture.summary(), READY_SUMMARY);
    }

    #[test]
    fn runtime_status_supplies_power_and_temporary_egress() {
        let fixture = Fixture::new();
        Fixture::write(&fixture.state(), r#"{"schema_version":1}"#, 0o600);
        Fixture::write(
            &fixture.runtime(),
            r#"{"schema_version":1,"power_state":"running","temporary_egress":{"purpose":"windows_update","expires_at_unix":1700000000}}"#,
            0o640,
        );
        assert_eq!(
            fixture.summary(),
            r#"{"catalog_generation":0,"configured":false,"license":"unknown","phase":"not_provisioned","power_state":"running","published_applications":0,"recovery_available":false,"schema_version":1,"temporary_egress":{"expires_at_unix":1700000000,"purpose":"windows_update"}}"#
        );
    }

    #[test]
    fn unsafe_state_files_are_refused_with_a_typed_detail() {
        let unavailable = |detail: &str| {
            format!(
                r#"{{"catalog_generation":0,"configured":false,"detail":"{detail}","license":"unknown","phase":"unavailable","power_state":"unknown","published_applications":0,"recovery_available":false,"schema_version":1,"temporary_egress":null}}"#
            )
        };
        let fixture = Fixture::new();
        Fixture::write(&fixture.state(), READY, 0o644);
        assert_eq!(fixture.summary(), unavailable("state is world-accessible"));

        let fixture = Fixture::new();
        let target = fixture.dir.path().join("elsewhere.json");
        Fixture::write(&target, READY, 0o600);
        std::os::unix::fs::symlink(&target, fixture.state()).expect("symlink");
        assert_eq!(
            fixture.summary(),
            unavailable("state is not a regular file")
        );

        let fixture = Fixture::new();
        std::fs::create_dir(fixture.state()).expect("directory");
        assert_eq!(
            fixture.summary(),
            unavailable("state is not a regular file")
        );

        for (state, detail) in [
            (r#"{"schema_version":2}"#, "unsupported state schema"),
            ("[1]", "unsupported state schema"),
            (
                r#"{"schema_version":1,"phase":"exploded"}"#,
                "invalid appliance phase",
            ),
            (
                r#"{"schema_version":1,"phase":{"ready":1,"installing":2}}"#,
                "invalid appliance phase",
            ),
            (
                r#"{"schema_version":1,"license":null}"#,
                "invalid licence state",
            ),
            (
                r#"{"schema_version":1,"catalog_generation":-1}"#,
                "invalid catalogue generation",
            ),
            (
                r#"{"schema_version":1,"cached_catalog":{"applications":{}}}"#,
                "invalid cached catalogue",
            ),
        ] {
            let fixture = Fixture::new();
            Fixture::write(&fixture.state(), state, 0o600);
            assert_eq!(fixture.summary(), unavailable(detail), "{state}");
        }
    }

    #[test]
    fn unreadable_state_reports_the_operating_system_error() {
        if rustix::process::geteuid().is_root() {
            return; // root reads a mode-000 file regardless
        }
        let fixture = Fixture::new();
        Fixture::write(&fixture.state(), READY, 0o000);
        let summary: Value = serde_json::from_str(&fixture.summary()).expect("JSON");
        assert_eq!(summary["phase"], "unavailable");
        assert_eq!(
            summary["detail"],
            format!(
                "[Errno 13] Permission denied: '{}'",
                fixture.state().display()
            )
        );
    }

    #[test]
    fn unsafe_runtime_status_marks_the_summary_unavailable_after_the_state() {
        let fixture = Fixture::new();
        Fixture::write(&fixture.state(), READY, 0o640);
        let unavailable = |detail: &str| {
            READY_SUMMARY
                .replace(r#""license""#, &format!(r#""detail":"{detail}","license""#))
                .replace(r#""phase":"ready""#, r#""phase":"unavailable""#)
        };
        Fixture::write(
            &fixture.runtime(),
            r#"{"schema_version":1,"power_state":"running"}"#,
            0o640,
        );
        let other_owner = rustix::process::geteuid().as_raw().wrapping_add(1);
        assert_eq!(
            fixture.summary_owned_by(other_owner),
            unavailable("Windows runtime status summary is unsafe")
        );
        Fixture::write(
            &fixture.runtime(),
            r#"{"schema_version":1,"power_state":"running"}"#,
            0o644,
        );
        assert_eq!(
            fixture.summary(),
            unavailable("Windows runtime status summary is unsafe")
        );
        for runtime in [
            r#"{"schema_version":1,"power_state":"hibernating"}"#,
            r#"{"power_state":"running"}"#,
        ] {
            Fixture::write(&fixture.runtime(), runtime, 0o640);
            let expected = unavailable("invalid authoritative Windows runtime status");
            assert_eq!(fixture.summary(), expected, "{runtime}");
        }
        // The validated power state is kept when only the lease is invalid.
        for runtime in [
            r#"{"schema_version":1,"power_state":"running","temporary_egress":{"purpose":"anything","expires_at_unix":1}}"#,
            r#"{"schema_version":1,"power_state":"running","temporary_egress":{"purpose":"activation","expires_at_unix":-1}}"#,
            r#"{"schema_version":1,"power_state":"running","temporary_egress":"activation"}"#,
        ] {
            Fixture::write(&fixture.runtime(), runtime, 0o640);
            let expected = unavailable("invalid temporary Windows egress status")
                .replace(r#""power_state":"unknown""#, r#""power_state":"running""#);
            assert_eq!(fixture.summary(), expected, "{runtime}");
        }
    }
}
