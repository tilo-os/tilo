//! Machine-readable summaries behind `tilo-app --json`.
//!
//! Every command reports one JSON document. Paths inside an app are relative
//! to its root, which is reported as an absolute `app_dir`.

use std::path::Path;

use serde_json::{Value, json};

use crate::portability::PortabilityReport;
use crate::standalone::SdkSource;
use crate::{
    AddOutcome, CreateOutcome, DoctorReport, DoctorStatus, LocaleSyncOutcome, PackageOutcome,
    ScenarioSuiteOutcome,
};

fn shown(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

fn absolute(path: &Path) -> String {
    std::path::absolute(path)
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .into_owned()
}

fn sdk_json(source: &SdkSource) -> Value {
    match source {
        SdkSource::Checkout(root) => json!({"checkout": root}),
        SdkSource::Git { url, rev } => json!({"git": url, "rev": rev}),
        SdkSource::Registry { version } => json!({"version": version}),
    }
}

impl CreateOutcome {
    /// The created (or, for a dry run, planned) app and its files.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "app_dir": absolute(&self.destination),
            "package": self.package_name,
            "app_id": self.app_id,
            "dry_run": self.dry_run,
            "workspace_member": self.workspace_member,
            "sdk": self.sdk.as_ref().map(sdk_json),
            "git_initialized": self.git_initialized,
            "files": self.files.iter().map(|path| shown(&self.destination, path)).collect::<Vec<_>>(),
            "next": {
                "dev": format!("tilo-app dev {}", absolute(&self.destination)),
                "check": format!("tilo-app check {}", absolute(&self.destination)),
            },
        })
    }
}

impl AddOutcome {
    /// Each file the artifact created or updated.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let files = self
            .files
            .iter()
            .map(|path| {
                json!({
                    "path": shown(&self.app_dir, path),
                    "action": if self.updated.contains(path) { "update" } else { "create" },
                })
            })
            .collect::<Vec<_>>();
        json!({
            "app_dir": absolute(&self.app_dir),
            "dry_run": self.dry_run,
            "files": files,
            "next": self.next_command,
        })
    }
}

impl LocaleSyncOutcome {
    /// The catalogs checked and rewritten, and differences left for a translator.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "app_dir": absolute(&self.app_dir),
            "dry_run": self.dry_run,
            "checked": self.checked,
            "files": self.files.iter().map(|path| shown(&self.app_dir, path)).collect::<Vec<_>>(),
            "unresolved": self.unresolved,
        })
    }
}

impl DoctorReport {
    /// Every check with its status; `ready` is false when any check failed.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let checks = self
            .checks
            .iter()
            .map(|check| {
                json!({
                    "label": check.label,
                    "status": match check.status {
                        DoctorStatus::Pass => "pass",
                        DoctorStatus::Warning => "warning",
                        DoctorStatus::Fail => "fail",
                    },
                    "detail": check.detail,
                })
            })
            .collect::<Vec<_>>();
        json!({
            "app_dir": absolute(&self.app_dir),
            "ready": self.is_ready(),
            "checks": checks,
        })
    }
}

impl ScenarioSuiteOutcome {
    /// Passed scenario names and each failure with its source and detail.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "passed": self.passed,
            "failed": self.failed.iter().map(|failure| json!({
                "name": failure.name,
                "source": absolute(&failure.source),
                "detail": failure.detail,
            })).collect::<Vec<_>>(),
        })
    }
}

impl PackageOutcome {
    /// The verified package and the payload paths of its entrypoints.
    #[must_use]
    pub fn to_json(&self) -> Value {
        json!({
            "output": absolute(&self.output),
            "entrypoints": self.entrypoints.iter().map(|path| absolute(path)).collect::<Vec<_>>(),
            "size": self.size.to_json(),
        })
    }
}

impl PortabilityReport {
    /// Every finding with its location, kind and remedy.
    #[must_use]
    pub fn to_json(&self) -> Value {
        let findings = |findings: &[crate::portability::PortabilityFinding]| {
            findings
                .iter()
                .map(|finding| {
                    json!({
                        "path": absolute(&finding.path),
                        "line": finding.line,
                        "kind": format!("{:?}", finding.kind),
                        "rule": finding.kind.rule(),
                        "severity": format!("{:?}", finding.severity),
                        "what": finding.what,
                        "help": finding.help,
                    })
                })
                .collect::<Vec<_>>()
        };
        json!({
            "app_dir": absolute(&self.app_dir),
            "lint": crate::portability::LINT,
            "passed": self.passed(),
            "platforms": self.platforms.iter().map(|platform| platform.as_str()).collect::<Vec<_>>(),
            "platform_reason": self.platform_reason,
            "exempt": self.exempt,
            "files": self.files,
            "findings": findings(&self.findings),
            "web_capable": self.web_capable(),
            "web_findings": findings(&self.web),
        })
    }
}
