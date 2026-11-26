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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectBinding {
    pub workspace: uuid::Uuid,
    pub host: [u8; 32],
    /// Canonical record identity supplied by the resource host, not its kind.
    pub record: String,
    pub resource: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CheckLimits {
    pub cpu_percent: u32,
    pub memory_bytes: u64,
    pub processes: u32,
    pub wall_seconds: u32,
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
    pub build: PathBuf,
    pub scratch: PathBuf,
    pub runtime_binary: PathBuf,
    pub images: Vec<CheckImage>,
    pub command: Vec<String>,
    pub environment: Vec<(String, String)>,
    pub limits: CheckLimits,
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

pub struct ApprovedChecks {
    binding: ProjectBinding,
    checks: BTreeMap<String, ApprovedCheck>,
}

impl ApprovedChecks {
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

    /// Run by ID only. `authority` must recheck the original `EffectCx` ownership
    /// before external work and while it runs; failure cancels the owned unit.
    pub fn run(
        &self,
        binding: &ProjectBinding,
        id: &str,
        authority: impl Fn() -> Result<(), String>,
    ) -> Result<CheckReceipt, String> {
        self.run_with_reports(binding, id, authority, |_| {})
    }

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
