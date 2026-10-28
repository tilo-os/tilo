//! What guided setup offers the palette, agents and scripts: its effects and
//! the live rows of the setup in progress.
//!
//! Setup choices describe local navigation and a reviewable plan on this
//! computer. They are effects of the imager resource, never document
//! actions, and never carry folder paths or installation authority into the
//! shared document.

use tilo_actions::{ActionParam, ActionSpec, ParamType, ResourcePresentation};
use ui::framework::{EffectStatus, LiveRow};
use ui::tilo_resource::{self, ActionRef, Effect};

/// The live row of the setup in progress, beneath the imager resource.
pub const SETUP_ROW_ID: &str = "imager.setup";

/// Guided setup's effects. Each answers with the setup's path-free status
/// ([`SetupProjection`]) as it stands afterwards.
#[derive(Clone, Debug, PartialEq, Eq, Effect)]
#[effect(operation = "imager.setup")]
pub enum SetupEffect {
    /// Read setup status.
    #[effect(returns, kind = "read")]
    ReadSetupStatus,
    /// Read this computer's Desktop readiness and current installation phase.
    #[effect(returns, kind = "read")]
    ReadDesktopStatus,
    /// Install the exact reviewed Desktop release after a fresh host probe.
    #[effect(returns)]
    InstallDesktop,
    /// Retry unfinished guided session preparation after verified package installation.
    #[effect(returns)]
    RetryDesktop,
    /// Cancel the current package download; no native transaction is cancelled.
    #[effect(returns)]
    CancelDesktopDownload,
    /// Recheck Desktop availability without installing anything.
    #[effect(returns, kind = "read")]
    RecheckDesktop,
    /// Continue setup.
    #[effect(returns, kind = "read")]
    ContinueSetup,
    /// Download Tilo.
    #[effect(returns)]
    StartInternalDownload,
    /// Pause download.
    #[effect(returns)]
    PauseInternalDownload,
    /// Resume download.
    #[effect(returns)]
    ResumeInternalDownload,
    /// Cancel download.
    #[effect(returns)]
    CancelInternalDownload,
    /// Discard unfinished download data.
    #[effect(returns)]
    DiscardInternalDownload,
    /// Keep download for later.
    #[effect(returns)]
    ReleaseInternalDownload,
    /// Set Tilo size.
    #[effect(returns)]
    SetInternalTargetSize {
        #[param("Tilo size in GiB")]
        gib: i64,
    },
    /// Attach an explicitly granted private migration intent to the next installation.
    #[effect(returns, privileged)]
    UseMigrationIntent {
        #[param(
            secret,
            "Private migration intent directory; never returned or replicated"
        )]
        directory: String,
    },
    /// Install Tilo alongside the current system.
    ///
    /// Installs (or continues) exactly the reviewed plan with this digest.
    #[effect(returns)]
    InstallInternal {
        #[param("Digest of the reviewed installation plan")]
        reviewed: String,
    },
    /// Restart into the Tilo installer.
    #[effect(returns)]
    RestartInternal,
    /// Restart later.
    #[effect(returns)]
    RestartInternalLater,
    /// Start the Tilo installer on the next restart.
    #[effect(returns)]
    RetryInternal {
        #[param("Id of the unfinished installer operation")]
        operation_id: String,
    },
    /// Remove the Tilo installer.
    #[effect(returns)]
    RemoveInternal {
        #[param("Id of the unfinished installer operation")]
        operation_id: String,
    },
    /// Restart to firmware settings.
    #[effect(returns)]
    InternalFirmwareSettings,
    /// Turn off Fast Startup.
    #[effect(returns)]
    InternalDisableFastStartup,
    /// Check this computer again.
    #[effect(returns, kind = "read")]
    RecheckInternal,
}

/// Which Full Tilo installation commands may run now.
#[allow(
    clippy::struct_excessive_bools,
    reason = "one independent availability flag per published action"
)]
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalAvailability {
    pub start_download: bool,
    pub pause_download: bool,
    pub resume_download: bool,
    pub cancel_download: bool,
    pub discard_download: bool,
    pub release_download: bool,
    pub set_target_size: bool,
    pub install: bool,
    pub restart: bool,
    pub restart_later: bool,
    pub retry: bool,
    pub remove: bool,
    pub firmware_settings: bool,
    pub disable_fast_startup: bool,
    pub recheck: bool,
}

/// Path-free state of the Full Tilo installation on this computer.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalProjection {
    /// `gated`, `checking`, `no-release`, `unsupported` or `ready`.
    pub readiness: &'static str,
    pub on_review: bool,
    /// Download worker phase (`none` before any download).
    pub download: &'static str,
    pub target_gib: Option<u64>,
    pub min_gib: Option<u64>,
    pub max_gib: Option<u64>,
    /// Digest `setup.internal.install` must name; changes with the plan.
    pub reviewed: Option<String>,
    pub shrink_bytes: Option<u64>,
    pub staging_bytes: Option<u64>,
    /// Stable blocker IDs from the host probe.
    pub blockers: Vec<&'static str>,
    /// `idle`, `running`, `succeeded` or `failed`.
    pub run: &'static str,
    /// Helper action of the latest run (`stage`, `arm`, `remove`).
    pub run_action: Option<&'static str>,
    pub failure: Option<String>,
    /// Unfinished staging operation the resume card offers.
    pub pending_operation: Option<String>,
    pub availability: InternalAvailability,
}

/// Component-level compatibility as the semantic surface publishes it.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HardwareComponentProjection {
    pub kind: &'static str,
    pub name: String,
    pub present: bool,
    pub status: &'static str,
    /// What the report's evidence can establish for the selected release.
    pub applicability: &'static str,
    /// The typed outcome; the interface localizes it.
    pub reason: imager_backend::support_report::SupportReason,
    pub evidence: Vec<imager_backend::support_report::Evidence>,
    pub remedy: Option<imager_backend::support_report::Remedy>,
    pub critical: bool,
}

/// The destination identity and its component results.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HardwareProjection {
    pub source: &'static str,
    pub manufacturer: Option<String>,
    pub model: Option<String>,
    pub variant: Option<String>,
    pub board: Option<String>,
    pub sku: Option<String>,
    pub firmware: Option<String>,
    pub kernel: Option<String>,
    pub supported: usize,
    pub limited: usize,
    pub unsupported: usize,
    pub unverified: usize,
    pub blocking: bool,
    pub components: Vec<HardwareComponentProjection>,
}

impl HardwareProjection {
    #[must_use]
    pub fn from_report(report: &imager_backend::support_report::SupportReport) -> Self {
        use imager_backend::support_report::ReportSource;
        let summary = report.summary();
        Self {
            source: match report.identity.source {
                ReportSource::LocalScan => "local-scan",
                ReportSource::Imported => "imported",
                ReportSource::DeclaredModel => "declared-model",
            },
            manufacturer: report.identity.manufacturer.clone(),
            model: report.identity.model.clone(),
            variant: report.identity.variant.clone(),
            board: report.identity.board.clone(),
            sku: report.identity.sku.clone(),
            firmware: report.identity.firmware.clone(),
            kernel: report.kernel.clone(),
            supported: summary.supported,
            limited: summary.limited,
            unsupported: summary.unsupported,
            unverified: summary.unverified,
            blocking: summary.blocking > 0,
            components: report
                .components
                .iter()
                .map(|component| HardwareComponentProjection {
                    kind: component.kind.key(),
                    name: component.name.clone(),
                    present: component.present,
                    status: component.state.key(),
                    applicability: match component.applicability {
                        imager_backend::support_report::Applicability::ReleaseVerified => {
                            "release-verified"
                        }
                        imager_backend::support_report::Applicability::ObservationOnly => {
                            "observation-only"
                        }
                        imager_backend::support_report::Applicability::Unknown => "unknown",
                    },
                    reason: component.reason.clone(),
                    evidence: component.evidence.clone(),
                    remedy: component.remedy,
                    critical: component.kind.critical(),
                })
                .collect(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct SetupProjection {
    /// The reviewed experience: `desktop` or `full_os`.
    pub experience: &'static str,
    /// Whether the read-only computer check may continue.
    pub next: bool,
    pub internal: InternalProjection,
    pub hardware: Option<HardwareProjection>,
    pub desktop: super::desktop_journey::DesktopProjection,
}

impl SetupProjection {
    #[must_use]
    pub fn value(&self) -> serde_json::Value {
        serde_json::to_value(self).unwrap_or_else(|_| serde_json::json!({}))
    }
}

/// An effect that cannot run now: `code` names why for programs, `reason`
/// for people.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Blocked {
    pub id: &'static str,
    pub code: &'static str,
    pub reason: String,
}

fn block(
    blocked: &mut Vec<Blocked>,
    open: bool,
    id: &'static str,
    code: &'static str,
    reason: String,
) {
    if !open {
        blocked.push(Blocked { id, code, reason });
    }
}

/// Why the Full Tilo commands that need the review are unavailable.
fn review_gate(state: &InternalProjection) -> (&'static str, &'static str) {
    if state.on_review {
        (
            "internal_unavailable",
            "imager-setup-internal-error-command-unavailable",
        )
    } else {
        (
            "full_os_review_only",
            "imager-setup-internal-error-not-review",
        )
    }
}

/// Each Full Tilo command: its effect id, its label, whether it may run now,
/// and the code and reason (a message key) when it may not.
fn internal_commands(
    state: &InternalProjection,
) -> [(&'static str, &'static str, bool, &'static str, &'static str); 16] {
    let a = state.availability;
    let (review_code, review_reason) = review_gate(state);
    [
        (
            SetupEffect::USE_MIGRATION_INTENT.id,
            "imager-setup-action-use-migration-intent",
            cfg!(target_os = "linux")
                && state.on_review
                && state.run == "idle"
                && state.pending_operation.is_none(),
            "migration_intent_unavailable",
            "imager-setup-error-migration-intent-unavailable",
        ),
        (
            SetupEffect::START_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-start-download",
            a.start_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::PAUSE_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-pause-download",
            a.pause_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::RESUME_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-resume-download",
            a.resume_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::CANCEL_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-cancel-download",
            a.cancel_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::DISCARD_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-discard-download",
            a.discard_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::RELEASE_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-release-download",
            a.release_download,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::SET_INTERNAL_TARGET_SIZE.id,
            "imager-setup-action-internal-set-target-size",
            a.set_target_size,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::INSTALL_INTERNAL.id,
            "imager-setup-action-internal-install",
            a.install,
            review_code,
            review_reason,
        ),
        (
            SetupEffect::RESTART_INTERNAL.id,
            "imager-setup-action-internal-restart",
            a.restart,
            "internal_not_staged",
            "imager-setup-internal-error-no-restart",
        ),
        (
            SetupEffect::RESTART_INTERNAL_LATER.id,
            "imager-setup-action-internal-restart-later",
            a.restart_later,
            "internal_not_staged",
            "imager-setup-internal-error-no-restart",
        ),
        (
            SetupEffect::RETRY_INTERNAL.id,
            "imager-setup-action-internal-retry",
            a.retry,
            "internal_no_pending",
            "imager-setup-internal-error-no-pending",
        ),
        (
            SetupEffect::REMOVE_INTERNAL.id,
            "imager-setup-action-internal-remove",
            a.remove,
            "internal_no_pending",
            "imager-setup-internal-error-no-pending",
        ),
        (
            SetupEffect::INTERNAL_FIRMWARE_SETTINGS.id,
            "imager-setup-action-internal-firmware-settings",
            a.firmware_settings,
            "remedy_unavailable",
            "imager-setup-internal-error-remedy-unavailable",
        ),
        (
            SetupEffect::INTERNAL_DISABLE_FAST_STARTUP.id,
            "imager-setup-action-internal-disable-fast-startup",
            a.disable_fast_startup,
            "remedy_unavailable",
            "imager-setup-internal-error-remedy-unavailable",
        ),
        (
            SetupEffect::RECHECK_INTERNAL.id,
            "imager-setup-action-internal-recheck",
            a.recheck,
            "internal_unavailable",
            "imager-setup-internal-error-unavailable",
        ),
    ]
}

/// The setup effects that cannot run in `state`, with why.
#[must_use]
pub fn blocked(state: &SetupProjection) -> Vec<Blocked> {
    let mut blocked = Vec::new();
    block(
        &mut blocked,
        state.next,
        SetupEffect::CONTINUE_SETUP.id,
        "setup_cannot_continue",
        "Complete the current setup choice before continuing".into(),
    );
    for (id, _, open) in super::desktop_journey::commands(&state.desktop) {
        block(
            &mut blocked,
            open,
            id,
            "desktop_command_unavailable",
            super::strings::t("imager-setup-desktop-command-unavailable"),
        );
    }
    for (id, _, open, code, reason) in internal_commands(&state.internal) {
        block(&mut blocked, open, id, code, super::strings::t(reason));
    }
    blocked
}

/// The setup effects as callers see them now: what is unavailable, and the
/// Full Tilo commands in the person's language.
#[must_use]
pub fn statuses(state: &SetupProjection) -> Vec<EffectStatus> {
    let effect = |id| ActionRef::<SetupEffect, ()>::new(id, &[], SetupEffect::kind());
    blocked(state)
        .into_iter()
        .map(|blocked| EffectStatus::unavailable(effect(blocked.id), blocked.code, blocked.reason))
        .chain(
            internal_commands(&state.internal)
                .into_iter()
                .map(|(id, label, ..)| {
                    EffectStatus::labelled(effect(id), super::strings::t(label))
                }),
        )
        .chain(
            super::desktop_journey::commands(&state.desktop)
                .into_iter()
                .map(|(id, label, _)| EffectStatus::labelled(effect(id), super::strings::t(label))),
        )
        .collect()
}

fn gated(spec: ActionSpec, available: bool, code: &str, reason: &str) -> ActionSpec {
    if available {
        spec
    } else {
        spec.unavailable(code, reason)
    }
}

/// `effect` bound to the row it is offered on, as `label`.
fn bound(effect: &SetupEffect, label: impl Into<String>) -> ActionSpec {
    let mut spec = effect.bound();
    spec.type_mut().label = label.into();
    spec
}

/// `effect` offered on a row as `label`, with what the row knows already
/// bound and `param` asked of the caller.
fn asking(effect: &SetupEffect, label: impl Into<String>, param: ActionParam) -> ActionSpec {
    let mut spec = effect.bound_asking_params(vec![param]);
    spec.type_mut().label = label.into();
    spec
}

fn internal_row(state: &InternalProjection) -> LiveRow {
    let operation = state.pending_operation.clone().unwrap_or_default();
    let mut row = LiveRow::new(
        format!("{SETUP_ROW_ID}.internal"),
        "imager.setup.internal",
        super::strings::t("imager-setup-resource-internal"),
    )
    .presentation(ResourcePresentation {
        detail: Some(super::strings::t_args(
            "imager-setup-detail-internal",
            &[
                ("readiness", state.readiness.into()),
                ("download", state.download.into()),
                ("run", state.run.into()),
                (
                    "blockers",
                    if state.blockers.is_empty() {
                        "none".to_string()
                    } else {
                        state.blockers.join(", ")
                    }
                    .into(),
                ),
            ],
        )),
        badge: Some(super::strings::t("imager-setup-badge-internal")),
        preview: None,
    });
    for (id, label, open, code, reason) in internal_commands(state) {
        let label = super::strings::t(label);
        let spec = match id {
            id if id == SetupEffect::USE_MIGRATION_INTENT.id => asking(
                &SetupEffect::UseMigrationIntent {
                    directory: String::new(),
                },
                label,
                ActionParam::required_owned(
                    "directory",
                    ParamType::Secret,
                    super::strings::t("imager-setup-param-migration-intent"),
                ),
            ),
            id if id == SetupEffect::SET_INTERNAL_TARGET_SIZE.id => asking(
                &SetupEffect::SetInternalTargetSize { gib: -1 },
                label,
                ActionParam::required_owned(
                    "gib",
                    ParamType::Integer,
                    super::strings::t("imager-setup-param-internal-target-gib"),
                ),
            ),
            id if id == SetupEffect::INSTALL_INTERNAL.id => bound(
                &SetupEffect::InstallInternal {
                    reviewed: state.reviewed.clone().unwrap_or_default(),
                },
                label,
            ),
            id if id == SetupEffect::RETRY_INTERNAL.id => bound(
                &SetupEffect::RetryInternal {
                    operation_id: operation.clone(),
                },
                label,
            ),
            id if id == SetupEffect::REMOVE_INTERNAL.id => bound(
                &SetupEffect::RemoveInternal {
                    operation_id: operation.clone(),
                },
                label,
            ),
            id => match SetupEffect::decode(id, &serde_json::Map::new()) {
                Ok(effect) => bound(&effect, label),
                Err(_) => continue,
            },
        };
        row = row.operation(gated(spec, open, code, &super::strings::t(reason)));
    }
    row
}

fn hardware_row(hardware: &HardwareProjection) -> LiveRow {
    let mut row = LiveRow::new(
        format!("{SETUP_ROW_ID}.hardware"),
        "imager.setup.hardware",
        "Hardware compatibility",
    )
    .presentation(ResourcePresentation {
        detail: Some(format!(
            "{}: {} supported, {} limited, {} unsupported, {} unverified.",
            hardware.model.as_deref().unwrap_or("Unknown model"),
            hardware.supported,
            hardware.limited,
            hardware.unsupported,
            hardware.unverified,
        )),
        badge: Some("Hardware".into()),
        preview: None,
    });
    for component in &hardware.components {
        row = row.row(
            LiveRow::new(
                format!("{SETUP_ROW_ID}.hardware.{}", component.kind),
                "imager.setup.hardware.component",
                format!("{}: {}", component.kind, component.status),
            )
            .presentation(ResourcePresentation {
                detail: (!component.name.is_empty()).then(|| component.name.clone()),
                badge: Some(component.status.into()),
                preview: None,
            }),
        );
    }
    row
}

/// The setup in progress as one live row beneath the imager resource: the
/// selection with its folders, the suggested folders, the review, what may
/// run now, and the Full Tilo installation.
#[must_use]
pub fn row(state: &SetupProjection) -> LiveRow {
    let experience = match state.experience {
        "desktop" => "Tilo Desktop",
        _ => "Full Tilo",
    };
    let mut root = LiveRow::new(SETUP_ROW_ID, "imager.setup", "Guided Tilo setup")
        .presentation(ResourcePresentation {
            detail: Some(format!("Review, {experience}")),
            badge: Some("Review".into()),
            preview: None,
        })
        .row(internal_row(&state.internal))
        .row(super::desktop_journey::row(&state.desktop));
    if let Some(hardware) = &state.hardware {
        root = root.row(hardware_row(hardware));
    }
    root
}

/// A delivery retry writes the prepared account capsule, so it is distinct
/// from the read-only native package recheck.
pub(super) fn desktop_preparation_row(retry: bool) -> LiveRow {
    LiveRow::new(
        "imager.setup.desktop-preparation",
        "imager.setup.desktop-preparation",
        super::strings::t("installer-desktop-resource"),
    )
    .operation(gated(
        bound(
            &SetupEffect::RetryDesktop,
            super::strings::t("installer-desktop-retry"),
        ),
        retry,
        "desktop_preparation_unavailable",
        &super::strings::t("installer-desktop-unavailable"),
    ))
}

pub(super) fn desktop_preparation_status(retry: bool) -> EffectStatus {
    if retry {
        EffectStatus::labelled(
            SetupEffect::RETRY_DESKTOP,
            super::strings::t("installer-desktop-retry"),
        )
    } else {
        EffectStatus::unavailable(
            SetupEffect::RETRY_DESKTOP,
            "desktop_preparation_unavailable",
            super::strings::t("installer-desktop-unavailable"),
        )
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn migration_intent_is_private_and_available_only_before_full_os_staging() {
        let mut state = InternalProjection {
            on_review: true,
            run: "idle",
            ..InternalProjection::default()
        };
        let actions = internal_actions(&projection(state.clone()));
        let action = actions
            .iter()
            .find(|action| action.id == SetupEffect::USE_MIGRATION_INTENT.id)
            .unwrap();
        assert_eq!(action.params()[0].ty, ParamType::Secret);
        assert_eq!(
            action.contract.availability.is_available(),
            cfg!(target_os = "linux")
        );
        let encoded = decoded(
            action,
            &serde_json::Map::from_iter([(
                "directory".into(),
                serde_json::Value::String("/private/reviewed-intent".into()),
            )]),
        );
        assert_eq!(
            encoded,
            SetupEffect::UseMigrationIntent {
                directory: "/private/reviewed-intent".into()
            }
        );
        for (review, run, pending) in [
            (false, "idle", None),
            (true, "running", None),
            (true, "idle", Some("staged".into())),
        ] {
            state.on_review = review;
            state.run = run;
            state.pending_operation = pending;
            let actions = internal_actions(&projection(state.clone()));
            assert!(
                !actions
                    .iter()
                    .find(|action| action.id == SetupEffect::USE_MIGRATION_INTENT.id)
                    .unwrap()
                    .contract
                    .availability
                    .is_available()
            );
        }
    }

    use super::*;
    use tilo_actions::OperationKind;

    fn projection(internal: InternalProjection) -> SetupProjection {
        SetupProjection {
            experience: "full_os",
            next: false,
            internal,
            hardware: None,
            desktop: crate::setup::desktop_journey::DesktopProjection::default(),
        }
    }

    fn internal_actions(state: &SetupProjection) -> Vec<ActionSpec> {
        row(state)
            .into_resource()
            .children
            .into_iter()
            .find(|child| child.kind == "imager.setup.internal")
            .expect("internal row")
            .actions
    }

    fn code(action: &ActionSpec) -> Option<&str> {
        match &action.contract.availability {
            tilo_actions::Availability::Available => None,
            tilo_actions::Availability::Unavailable { code, .. } => Some(code),
        }
    }

    fn decoded(
        action: &ActionSpec,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> SetupEffect {
        let encoded = action.build_json(arguments).unwrap();
        assert_eq!(encoded.type_id, SetupEffect::OPERATION);
        SetupEffect::from_payload(&encoded.payload).unwrap()
    }

    /// Internal install row commands, availability and payload.
    #[test]
    fn internal_row_offers_gated_commands_bound_to_the_reviewed_digest() {
        // Catches an install command reachable only by clicking (spec §8).
        let actions = internal_actions(&projection(InternalProjection::default()));
        for (id, ..) in internal_commands(&InternalProjection::default()) {
            let action = actions
                .iter()
                .find(|action| action.id == id)
                .unwrap_or_else(|| panic!("missing {id}"));
            assert_eq!(action.kind, OperationKind::Effect, "{id}");
            assert!(!action.contract.availability.is_available(), "{id}");
        }
        let install = actions
            .iter()
            .find(|action| action.id == "install-internal")
            .unwrap();
        assert_eq!(code(install), Some("full_os_review_only"));
        let blocked = blocked(&projection(InternalProjection::default()));
        assert!(
            blocked
                .iter()
                .any(|blocked| blocked.id == "install-internal"
                    && blocked.code == "full_os_review_only"),
            "the declared effect is gated like the row's"
        );

        let actions = internal_actions(&projection(InternalProjection {
            on_review: true,
            availability: InternalAvailability {
                install: true,
                firmware_settings: true,
                ..InternalAvailability::default()
            },
            ..InternalProjection::default()
        }));
        for action in &actions {
            let expected = matches!(
                action.id.as_str(),
                "install-internal" | "internal-firmware-settings"
            );
            assert_eq!(
                action.contract.availability.is_available(),
                expected,
                "{}",
                action.id
            );
        }
        let restart = actions
            .iter()
            .find(|action| action.id == "restart-internal")
            .unwrap();
        assert_eq!(code(restart), Some("internal_not_staged"));

        // Catches an install or removal that does not name what the invoker
        // saw, which is what lets the view refuse a stale invocation.
        let state = projection(InternalProjection {
            on_review: true,
            reviewed: Some("a".repeat(64)),
            pending_operation: Some("op-7".into()),
            ..InternalProjection::default()
        });
        let actions = internal_actions(&state);
        let action = |id: &str| actions.iter().find(|action| action.id == id).unwrap();
        let none = serde_json::Map::new();
        assert_eq!(
            decoded(action("install-internal"), &none),
            SetupEffect::InstallInternal {
                reviewed: "a".repeat(64)
            }
        );
        assert_eq!(
            decoded(action("remove-internal"), &none),
            SetupEffect::RemoveInternal {
                operation_id: "op-7".into()
            }
        );
        let size = action("set-internal-target-size");
        let mut args = serde_json::Map::new();
        args.insert("gib".into(), serde_json::json!(80));
        assert_eq!(
            decoded(size, &args),
            SetupEffect::SetInternalTargetSize { gib: 80 }
        );
        args.insert("gib".into(), serde_json::json!("eighty"));
        assert!(size.build_json(&args).is_err());
    }
}
