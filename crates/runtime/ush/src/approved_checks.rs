//! Trusted project-host check supervision. An effect selects only an approved
//! ID in its exact binding; commands, images and bounds never come from it.
//! This module schedules no agents and grants no invocation authority.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod execution;
mod reports;
pub use execution::{
    CgroupObservation, CheckOutcome, CheckReceipt, developer_probe, developer_probe_with_reports,
    run_guest, run_init,
};
pub use reports::{CheckReport, CheckReportRefusal, ReportedCheckEvent, ReportedCheckPhase};

/// Full project-host identity supplied by the admitted resource host.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBinding {
    pub workspace: uuid::Uuid,
    pub host: [u8; 32],
    /// Canonical record identity supplied by the resource host, not its kind.
    pub record: String,
    pub resource: String,
}

/// Aggregate CPU bandwidth, memory, process, wall and capture bounds.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckLimits {
    pub cpu_percent: u32,
    pub memory_bytes: u64,
    pub processes: u32,
    pub wall_seconds: u32,
    /// Each output stream has this ceiling; exceeding it fails the job.
    pub output_bytes: usize,
}

impl CheckLimits {
    fn validate(&self) -> Result<(), String> {
        if !(1..=6400).contains(&self.cpu_percent)
            || !(16 * 1024 * 1024..=1024 * 1024 * 1024 * 1024).contains(&self.memory_bytes)
            || !(2..=4096).contains(&self.processes)
            || !(1..=3600).contains(&self.wall_seconds)
            || !(1024..=64 * 1024 * 1024).contains(&self.output_bytes)
        {
            return Err("approved check bounds are outside supported limits".into());
        }
        Ok(())
    }
}

/// An already resolved immutable tool image. Verification/materialisation is
/// the existing Images provider's responsibility, before registration.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckImage {
    pub identity: String,
    pub root: PathBuf,
}

/// Host-only registration input. This is not a model action or wire request.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ApprovedCheck {
    pub id: String,
    pub binding: ProjectBinding,
    pub checkout: PathBuf,
    /// Unique writable build/artifact directory mounted at /project/target.
    pub build: PathBuf,
    /// Dedicated private working directory, outside source and build.
    pub scratch: PathBuf,
    pub runtime_binary: PathBuf,
    pub images: Vec<CheckImage>,
    /// Absolute guest program followed by fixed arguments; no shell shaping.
    pub command: Vec<String>,
    /// Fixed host-reviewed environment overlays. No target-directory override.
    pub environment: Vec<(String, String)>,
    pub limits: CheckLimits,
    /// Optional host-reviewed phase vocabulary; empty disables stderr reports.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reported_phases: Vec<ReportedCheckPhase>,
}

fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
        && !value.starts_with('-')
}

fn exact_directory(path: &Path) -> Result<(), String> {
    if !path.is_absolute()
        || path.canonicalize().map_err(|e| e.to_string())? != path
        || !path.symlink_metadata().map_err(|e| e.to_string())?.is_dir()
    {
        return Err("approved check directory must be an exact absolute directory".into());
    }
    Ok(())
}

fn owned_private_directory(path: &Path) -> Result<(), String> {
    let scratch = path.symlink_metadata().map_err(|e| e.to_string())?;
    if !tilo_files::native::private::private_to_me(&scratch) {
        return Err(
            "approved private scratch must be owned and inaccessible to other users".into(),
        );
    }
    Ok(())
}

impl ApprovedCheck {
    fn validate(&self) -> Result<(), String> {
        self.limits.validate()?;
        reports::validate_phases(&self.reported_phases)?;
        if !identifier(&self.id)
            || self.binding.workspace.is_nil()
            || self.binding.host == [0; 32]
            || self.binding.record.is_empty()
            || self.binding.record.len() > 8192
            || self.binding.record.contains('\0')
            || self.binding.resource.is_empty()
            || self.binding.resource.len() > 1024
            || self.binding.resource.contains('\0')
        {
            return Err("approved check project binding or ID is invalid".into());
        }
        for path in [&self.checkout, &self.build, &self.scratch] {
            exact_directory(path)?;
        }
        if [&self.checkout, &self.build, &self.scratch]
            .iter()
            .enumerate()
            .any(|(i, a)| {
                [&self.checkout, &self.build, &self.scratch]
                    .iter()
                    .enumerate()
                    .any(|(j, b)| i != j && a.starts_with(b))
            })
        {
            return Err("approved source, build and private scratch must be disjoint".into());
        }
        exact_directory(&self.checkout.join("target"))?;
        if std::fs::read_dir(self.checkout.join("target"))
            .map_err(|e| e.to_string())?
            .next()
            .is_some()
        {
            return Err("approved source target mount point must be empty".into());
        }
        owned_private_directory(&self.scratch)?;
        if !self.runtime_binary.is_absolute()
            || self
                .runtime_binary
                .canonicalize()
                .map_err(|e| e.to_string())?
                != self.runtime_binary
            || !self
                .runtime_binary
                .symlink_metadata()
                .map_err(|e| e.to_string())?
                .is_file()
            || self.images.is_empty()
            || self.images.len() > 32
        {
            return Err("approved runtime or images are invalid".into());
        }
        for image in &self.images {
            exact_directory(&image.root)?;
            if !image
                .identity
                .strip_prefix("sha256:")
                .is_some_and(|digest| {
                    digest.len() == 64
                        && digest
                            .bytes()
                            .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase())
                })
            {
                return Err("approved image must have an immutable SHA-256 identity".into());
            }
        }
        self.validate_command()
    }

    fn validate_command(&self) -> Result<(), String> {
        let Some(program) = self.command.first() else {
            return Err("approved command is empty".into());
        };
        if !Path::new(program).is_absolute()
            || !program.starts_with("/usr/")
            || Path::new(program)
                .components()
                .any(|part| matches!(part, std::path::Component::ParentDir))
            || self.command.len() > 128
            || self.command.iter().any(|a| a.contains('\0'))
            || self.command.iter().map(String::len).sum::<usize>() > 64 * 1024
        {
            return Err("approved command is invalid or too large".into());
        }
        let mut keys = std::collections::BTreeSet::new();
        if self.environment.len() > 32
            || self.environment.iter().any(|(key, value)| {
                key.is_empty()
                    || !key
                        .bytes()
                        .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                    || value.contains('\0')
                    || value.len() > 4096
                    || !keys.insert(key)
                    || !matches!(
                        key.as_str(),
                        "CARGO_HOME"
                            | "RUSTUP_HOME"
                            | "RUSTFLAGS"
                            | "CARGO_NET_OFFLINE"
                            | "LANG"
                            | "LC_ALL"
                            | "TZ"
                    )
            })
        {
            return Err(
                "approved environment contains duplicate, forbidden or malformed settings".into(),
            );
        }
        Ok(())
    }
}

/// A host-created registry scoped to one admitted project. No global ID lookup.
pub struct ApprovedChecks {
    binding: ProjectBinding,
    checks: BTreeMap<String, ApprovedCheck>,
}

impl ApprovedChecks {
    /// Construct from trusted host configuration, refusing foreign bindings.
    ///
    /// # Errors
    /// Returns invalid registrations, foreign bindings or duplicate IDs.
    pub fn new(binding: ProjectBinding, checks: Vec<ApprovedCheck>) -> Result<Self, String> {
        if checks.len() > 128 {
            return Err("too many approved project checks".into());
        }
        let mut registry = Self {
            binding,
            checks: BTreeMap::new(),
        };
        for check in checks {
            check.validate()?;
            if check.binding != registry.binding {
                return Err("approved check belongs to another project host".into());
            }
            if registry.checks.insert(check.id.clone(), check).is_some() {
                return Err("duplicate approved check ID".into());
            }
        }
        Ok(registry)
    }

    /// Run by ID only. `authority` must recheck the original `EffectCx` custody
    /// before external work and while it runs; failure cancels the owned unit.
    ///
    /// # Errors
    /// Returns invalid binding, registration, authority or supervision setup.
    pub fn run(
        &self,
        binding: &ProjectBinding,
        id: &str,
        authority: impl Fn() -> Result<(), String>,
    ) -> Result<CheckReceipt, String> {
        self.run_with_reports(binding, id, authority, |_| {})
    }

    /// Run the same owned check with bounded informational command reports.
    /// Reports cannot determine execution success, cleanup or evidence validity.
    /// The callback runs on the existing host worker after a fresh custody check.
    ///
    /// # Errors
    /// Returns the same binding, authority and supervision failures as [`Self::run`].
    pub fn run_with_reports(
        &self,
        binding: &ProjectBinding,
        id: &str,
        authority: impl Fn() -> Result<(), String>,
        report: impl Fn(ReportedCheckEvent),
    ) -> Result<CheckReceipt, String> {
        if binding != &self.binding {
            return Err("approved check project host changed".into());
        }
        let check = self
            .checks
            .get(id)
            .ok_or("check ID was not approved by this project host")?;
        authority()?;
        check.validate()?;
        authority()?;
        execution::run(check, authority, report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    fn check(root: &Path) -> ApprovedCheck {
        for dir in ["source/target", "build", "scratch", "image/usr/bin"] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
        }
        std::fs::set_permissions(root.join("scratch"), std::fs::Permissions::from_mode(0o700))
            .unwrap();
        ApprovedCheck {
            id: "unit-check".into(),
            binding: ProjectBinding {
                workspace: uuid::Uuid::new_v4(),
                host: [7; 32],
                record: "file:/fixture/project.tilo".into(),
                resource: "project:one".into(),
            },
            checkout: root.join("source"),
            build: root.join("build"),
            scratch: root.join("scratch"),
            runtime_binary: PathBuf::from("/usr/bin/true").canonicalize().unwrap(),
            images: vec![CheckImage {
                identity: format!("sha256:{}", "a".repeat(64)),
                root: root.join("image"),
            }],
            command: vec!["/usr/bin/true".into()],
            environment: vec![],
            reported_phases: vec![],
            limits: CheckLimits {
                cpu_percent: 100,
                memory_bytes: 64 * 1024 * 1024,
                processes: 32,
                wall_seconds: 5,
                output_bytes: 4096,
            },
        }
    }

    #[test]
    fn registry_rejects_foreign_host_workspace_and_project_before_execution() {
        let dir = tempfile::tempdir().unwrap();
        let entry = check(dir.path());
        let binding = entry.binding.clone();
        let registry = ApprovedChecks::new(binding.clone(), vec![entry.clone()]).unwrap();
        for foreign in [
            ProjectBinding {
                host: [8; 32],
                ..binding.clone()
            },
            ProjectBinding {
                workspace: uuid::Uuid::new_v4(),
                ..binding.clone()
            },
            ProjectBinding {
                record: "file:/fixture/another-project.tilo".into(),
                ..binding.clone()
            },
            ProjectBinding {
                resource: "project:two".into(),
                ..binding.clone()
            },
        ] {
            assert!(ApprovedChecks::new(foreign.clone(), vec![entry.clone()]).is_err());
            assert!(
                registry
                    .run(&foreign, "unit-check", || panic!(
                        "must not reach authority or manager"
                    ))
                    .is_err()
            );
        }
        assert!(
            registry
                .run(&binding, "other-check", || panic!(
                    "unapproved ID must not execute"
                ))
                .is_err()
        );
        assert!(
            registry
                .run(&binding, "unit-check", || Err("caller revoked".into()))
                .unwrap_err()
                .contains("caller revoked")
        );
    }

    #[test]
    fn registry_refuses_duplicate_checks_source_links_and_shared_builds() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = check(dir.path());
        assert!(
            ApprovedChecks::new(entry.binding.clone(), vec![entry.clone(), entry.clone()]).is_err()
        );
        entry.build = entry.checkout.join("target");
        assert!(entry.validate().unwrap_err().contains("disjoint"));
        entry = check(dir.path());
        let source = dir.path().join("source-link");
        std::os::unix::fs::symlink(&entry.checkout, &source).unwrap();
        entry.checkout = source;
        assert!(entry.validate().is_err());
    }

    #[test]
    fn registry_refuses_target_override_preconfinement_loader_and_duplicate_environment() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = check(dir.path());
        for environment in [
            vec![("CARGO_TARGET_DIR".into(), "/shared".into())],
            vec![("LD_PRELOAD".into(), "/project/malicious.so".into())],
            vec![
                ("CARGO_HOME".into(), "/project/target/cache".into()),
                ("CARGO_HOME".into(), "/elsewhere".into()),
            ],
        ] {
            entry.environment = environment;
            assert!(entry.validate().is_err());
        }
        entry.environment = vec![
            ("CARGO_HOME".into(), "/project/target/cache".into()),
            ("CARGO_NET_OFFLINE".into(), "true".into()),
        ];
        assert!(entry.validate().is_ok());
    }

    #[test]
    fn malformed_limits_and_unknown_configuration_fields_fail() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = check(dir.path());
        let mut json = serde_json::to_value(&entry).unwrap();
        json["raw_shell"] = "echo unsafe".into();
        assert!(serde_json::from_value::<ApprovedCheck>(json).is_err());
        entry.limits.memory_bytes = 0;
        assert!(entry.validate().is_err());
        entry = check(dir.path());
        entry.images[0].identity = "latest".into();
        assert!(entry.validate().is_err());
    }

    #[test]
    fn source_build_contents_unsafe_scratch_and_tool_path_escape_fail_registration() {
        let dir = tempfile::tempdir().unwrap();
        let mut entry = check(dir.path());
        std::fs::write(
            entry.checkout.join("target/prior-output"),
            b"foreign build artifact",
        )
        .unwrap();
        assert!(
            entry
                .validate()
                .unwrap_err()
                .contains("mount point must be empty")
        );
        std::fs::remove_file(entry.checkout.join("target/prior-output")).unwrap();
        std::fs::set_permissions(&entry.scratch, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert!(
            entry
                .validate()
                .unwrap_err()
                .contains("inaccessible to other users")
        );
        std::fs::set_permissions(&entry.scratch, std::fs::Permissions::from_mode(0o700)).unwrap();
        entry.command[0] = "/usr/../project/target/agent-controlled-helper".into();
        assert!(entry.validate().unwrap_err().contains("command is invalid"));
    }
}
