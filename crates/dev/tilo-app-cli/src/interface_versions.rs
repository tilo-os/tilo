//! The interface versions an app's manifest speaks, against the ones Tilo
//! serves now.
//!
//! Tilo serves exactly one version of each interface and keeps no code for
//! an older one (docs/architecture/compatibility.md): a build whose
//! `[sandbox.services]` names another version of any service it uses does not
//! run, and devices wait for its developer's next build. `tilo-app check`
//! says so before publishing, with the line to write.

use registry_types::ProvidedInterfaces;
use tilo_app_package::{Manifest, Requirement};

use crate::{DoctorCheck, DoctorStatus};

/// The interfaces the Tilo this SDK belongs to serves: the image's own
/// declaration, from the same commit.
const CURRENT: &str = include_str!("../../../../os/usr/share/tilo/system-interfaces.toml");

/// The interfaces Tilo serves now.
///
/// # Panics
///
/// Never in a built SDK: the declaration is part of the repository and is
/// checked by `tools/policy/check_interfaces.py`.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the embedded declaration is a checked repository file"
)]
pub fn current() -> ProvidedInterfaces {
    toml::from_str(CURRENT).expect("os/usr/share/tilo/system-interfaces.toml parses")
}

/// One service whose version differs from what Tilo serves, and its fix.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Mismatch {
    pub service: String,
    /// What the manifest says, e.g. `optional@1`.
    pub written: String,
    /// The version Tilo serves of it, `None` when it serves no such service.
    pub served: Option<u32>,
    /// The exact fix.
    pub fix: String,
}

/// Every service `manifest` names at another version than `provided` serves,
/// in the manifest's order (by name).
#[must_use]
pub fn mismatches(manifest: &Manifest, provided: &ProvidedInterfaces) -> Vec<Mismatch> {
    manifest
        .sandbox
        .services
        .iter()
        .filter_map(|(service, requirement)| {
            let served = provided.version(service);
            if served == Some(requirement.version) {
                return None;
            }
            let need = match requirement.requirement {
                Requirement::Required => "required",
                Requirement::Optional => "optional",
            };
            let written = format!("{need}@{}", requirement.version);
            let fix = match served {
                Some(version) => format!(
                    "set `{service} = \"{need}@{version}\"` under [sandbox.services] in \
                     tilo-app.toml, build against the current SDK and publish the build"
                ),
                None => format!(
                    "Tilo serves no `{service}` service: remove `{service}` from \
                     [sandbox.services] in tilo-app.toml, or use the service that replaced it"
                ),
            };
            Some(Mismatch {
                service: service.clone(),
                written,
                served,
                fix,
            })
        })
        .collect()
}

/// The doctor's check against the interfaces Tilo serves now.
pub(crate) fn check(manifest: &Manifest, checks: &mut Vec<DoctorCheck>) {
    check_against(manifest, &current(), checks);
}

/// Pass when every service fits `provided`, fail with each fix.
fn check_against(
    manifest: &Manifest,
    provided: &ProvidedInterfaces,
    checks: &mut Vec<DoctorCheck>,
) {
    let found = mismatches(manifest, provided);
    if found.is_empty() {
        checks.push(DoctorCheck {
            status: DoctorStatus::Pass,
            label: "interface versions",
            detail: "every service in [sandbox.services] speaks the version Tilo serves".into(),
        });
        return;
    }
    let lines = found
        .iter()
        .map(|mismatch| {
            let served = mismatch.served.map_or_else(
                || "Tilo serves none".to_owned(),
                |version| format!("Tilo serves {}@{version}", mismatch.service),
            );
            format!(
                "{} = \"{}\" ({served}): {}",
                mismatch.service, mismatch.written, mismatch.fix
            )
        })
        .collect::<Vec<_>>()
        .join("; ");
    checks.push(DoctorCheck {
        status: DoctorStatus::Fail,
        label: "interface versions",
        detail: format!(
            "this build does not run on current Tilo, which serves one version of each \
             interface; devices show it as waiting for an update from you. {lines}"
        ),
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn manifest(services: &str) -> Manifest {
        Manifest::from_toml_str(&format!(
            "[package]\nid = \"so.example.errands\"\nversion = \"0.1.0\"\nkind = \"app\"\n\n\
             [publisher]\nidentity = \"ed25519:{}\"\nsignature = \"\"\n\n\
             [sandbox]\nnamespaces = [\"user\"]\nseccomp = \"tilo-default\"\nlandlock = \"strict\"\n\n\
             [sandbox.net]\nmode = \"off\"\n\n\
             [sandbox.services]\n{services}\n",
            "0".repeat(64)
        ))
        .expect("manifest")
    }

    // USER REQUIREMENT (2026-10-18): "it's fine if we break compat with old versions every few days, people just need to update"
    #[test]
    fn a_manifest_at_another_service_version_is_told_the_exact_fix() {
        let tilo = ProvidedInterfaces::new([("runtime", 3), ("system", 1)], ["collab", "tilo-fs"]);
        let fits = manifest("collab = \"optional@3\"\ntilo-fs = \"required@3\"");
        assert!(mismatches(&fits, &tilo).is_empty());
        let mut checks = Vec::new();
        check_against(&fits, &tilo, &mut checks);
        assert_eq!(checks[0].status, DoctorStatus::Pass);

        let behind =
            manifest("collab = \"optional@2\"\nretired = \"optional@1\"\ntilo-fs = \"required@4\"");
        let found = mismatches(&behind, &tilo);
        assert_eq!(
            found
                .iter()
                .map(|m| (m.service.as_str(), m.written.as_str(), m.served))
                .collect::<Vec<_>>(),
            [
                ("collab", "optional@2", Some(3)),
                ("retired", "optional@1", None),
                ("tilo-fs", "required@4", Some(3)),
            ]
        );
        assert!(
            found[0]
                .fix
                .starts_with("set `collab = \"optional@3\"` under [sandbox.services]"),
            "{}",
            found[0].fix
        );
        assert!(
            found[1].fix.contains("remove `retired`"),
            "{}",
            found[1].fix
        );
        assert!(
            found[2].fix.starts_with("set `tilo-fs = \"required@3\"`"),
            "{}",
            found[2].fix
        );

        let mut checks = Vec::new();
        check_against(&behind, &tilo, &mut checks);
        assert_eq!(checks[0].status, DoctorStatus::Fail);
        assert!(
            checks[0].detail.contains(
                "collab = \"optional@2\" (Tilo serves collab@3): set `collab = \"optional@3\"`"
            ),
            "{}",
            checks[0].detail
        );
    }

    #[test]
    fn the_sdk_knows_the_interfaces_tilo_serves_now() {
        let tilo = current();
        for interface in ["runtime", "shell-ui", "system"] {
            assert!(
                tilo.version(interface).is_some(),
                "the embedded declaration serves {interface}"
            );
        }
    }
}
