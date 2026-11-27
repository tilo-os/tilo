//! What a guided setup page publishes beneath the setup: its internal
//! installation, Desktop and hardware rows, and how each of its effects stands.

use super::journey::InstallerPreviewEffect;
use tilo_actions::ResourcePresentation;
use tilo_resource::{self, ActionRef, Effect, RowBindings};
use tilo_resource::{EffectStatus, LiveRow};

pub const SETUP_ROW_ID: &str = "imager.setup";

/// Guided setup, as it publishes itself: the review, with the internal
/// installation, the Desktop journey and the hardware beneath it.
#[derive(tilo_resource::Row)]
#[row(kind = "imager.setup", label = "Guided setup")]
pub(crate) struct SetupRoot;

/// The internal installation's downloads, review and install.
#[derive(tilo_resource::Row)]
#[row(
    kind = "imager.setup.internal",
    label = "Internal installation",
    effects = InstallerPreviewEffect
)]
pub(crate) struct InternalSetup;

/// This computer's hardware, as the support report reads it.
#[derive(tilo_resource::Row)]
#[row(kind = "imager.setup.hardware", label = "Hardware")]
pub(crate) struct Hardware;

/// One component's support on this computer.
#[derive(tilo_resource::Row)]
#[row(kind = "imager.setup.hardware.component", label = "Hardware component")]
pub(crate) struct HardwareComponent;

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

#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
pub struct InternalProjection {
    pub readiness: &'static str,
    pub on_review: bool,
    pub download: &'static str,
    pub target_gib: Option<u64>,
    pub min_gib: Option<u64>,
    pub max_gib: Option<u64>,
    pub reviewed: Option<String>,
    pub shrink_bytes: Option<u64>,
    pub staging_bytes: Option<u64>,
    pub blockers: Vec<&'static str>,
    pub run: &'static str,
    pub run_action: Option<&'static str>,
    pub failure: Option<String>,
    pub pending_operation: Option<String>,
    pub availability: InternalAvailability,
}

#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
pub struct HardwareComponentProjection {
    pub kind: &'static str,
    pub name: String,
    pub present: bool,
    pub status: &'static str,
    pub applicability: &'static str,
    pub reason: hardware_report::SupportReason,
    pub evidence: Vec<hardware_report::Evidence>,
    pub remedy: Option<hardware_report::Remedy>,
    pub critical: bool,
}

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
    pub fn from_report(report: &hardware_report::SupportReport) -> Self {
        use hardware_report::ReportSource;
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
                        hardware_report::Applicability::ReleaseVerified => "release-verified",
                        hardware_report::Applicability::ObservationOnly => "observation-only",
                        hardware_report::Applicability::Unknown => "unknown",
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
    pub experience: &'static str,
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

fn review_check(state: &InternalProjection) -> (&'static str, &'static str) {
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

fn internal_commands(
    state: &InternalProjection,
) -> [(&'static str, &'static str, bool, &'static str, &'static str); 15] {
    let a = state.availability;
    let (review_code, review_reason) = review_check(state);
    [
        (
            InstallerPreviewEffect::START_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-start-download",
            a.start_download,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::PAUSE_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-pause-download",
            a.pause_download,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::RESUME_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-resume-download",
            a.resume_download,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::CANCEL_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-cancel-download",
            a.cancel_download,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::DISCARD_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-discard-download",
            a.discard_download,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::RELEASE_INTERNAL_DOWNLOAD.id,
            "imager-setup-action-release-download",
            a.release_download,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::SET_INTERNAL_TARGET_SIZE.id,
            "imager-setup-action-internal-set-target-size",
            a.set_target_size,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::INSTALL_INTERNAL.id,
            "imager-setup-action-internal-install",
            a.install,
            review_code,
            review_reason,
        ),
        (
            InstallerPreviewEffect::RESTART_INTERNAL.id,
            "imager-setup-action-internal-restart",
            a.restart,
            "internal_not_staged",
            "imager-setup-internal-error-no-restart",
        ),
        (
            InstallerPreviewEffect::RESTART_INTERNAL_LATER.id,
            "imager-setup-action-internal-restart-later",
            a.restart_later,
            "internal_not_staged",
            "imager-setup-internal-error-no-restart",
        ),
        (
            InstallerPreviewEffect::RETRY_INTERNAL.id,
            "imager-setup-action-internal-retry",
            a.retry,
            "internal_no_pending",
            "imager-setup-internal-error-no-pending",
        ),
        (
            InstallerPreviewEffect::REMOVE_INTERNAL.id,
            "imager-setup-action-internal-remove",
            a.remove,
            "internal_no_pending",
            "imager-setup-internal-error-no-pending",
        ),
        (
            InstallerPreviewEffect::INTERNAL_FIRMWARE_SETTINGS.id,
            "imager-setup-action-internal-firmware-settings",
            a.firmware_settings,
            "remedy_unavailable",
            "imager-setup-internal-error-remedy-unavailable",
        ),
        (
            InstallerPreviewEffect::INTERNAL_DISABLE_FAST_STARTUP.id,
            "imager-setup-action-internal-disable-fast-startup",
            a.disable_fast_startup,
            "remedy_unavailable",
            "imager-setup-internal-error-remedy-unavailable",
        ),
        (
            InstallerPreviewEffect::RECHECK_INTERNAL.id,
            "imager-setup-action-internal-recheck",
            a.recheck,
            "internal_unavailable",
            "imager-setup-internal-error-unavailable",
        ),
    ]
}

#[must_use]
pub fn blocked(state: &SetupProjection) -> Vec<Blocked> {
    let mut blocked = Vec::new();
    block(
        &mut blocked,
        state.next,
        InstallerPreviewEffect::CONTINUE_PREVIEW.id,
        "setup_cannot_continue",
        "Complete the current setup choice before continuing".into(),
    );
    for (command, _, open) in super::desktop_journey::commands(&state.desktop) {
        block(
            &mut blocked,
            open,
            command.id,
            "desktop_command_unavailable",
            super::strings::t("imager-setup-desktop-command-unavailable"),
        );
    }
    for (id, _, open, code, reason) in internal_commands(&state.internal) {
        block(&mut blocked, open, id, code, super::strings::t(reason));
    }
    blocked
}

/// How each effect of a setup page stands: the ones `offers` names refused
/// with the reason the projection gives, and the internal and Desktop
/// commands labelled in the person's language.
pub fn statuses(state: &SetupProjection, offers: impl Fn(&str) -> bool) -> Vec<EffectStatus> {
    blocked(state)
        .into_iter()
        .filter(|blocked| {
            offers(blocked.id) || blocked.id == InstallerPreviewEffect::CONTINUE_PREVIEW.id
        })
        .map(|blocked| {
            EffectStatus::unavailable(effect_ref(blocked.id), blocked.code, blocked.reason)
        })
        .chain(
            internal_commands(&state.internal)
                .into_iter()
                .map(|(id, label, ..)| {
                    EffectStatus::labelled(effect_ref(id), super::strings::t(label))
                }),
        )
        .chain(
            super::desktop_journey::commands(&state.desktop)
                .into_iter()
                .map(|(command, label, _)| {
                    EffectStatus::labelled(command, super::strings::t(label))
                }),
        )
        .collect()
}

/// How each published row's effects stand, by the row's id.
pub fn row_statuses(state: &SetupProjection) -> Vec<(String, Vec<EffectStatus>)> {
    let mut by_row = vec![
        (
            format!("{SETUP_ROW_ID}.internal"),
            internal_row_status(&state.internal),
        ),
        (
            "imager.setup.desktop".to_owned(),
            super::desktop_journey::row_status(&state.desktop),
        ),
    ];
    by_row.retain(|(_, statuses)| !statuses.is_empty());
    by_row
}

fn effect_ref(id: &'static str) -> ActionRef<InstallerPreviewEffect, ()> {
    ActionRef::new(id, &[], InstallerPreviewEffect::kind())
}

/// The internal installation's commands, each with what the reviewed
/// projection knows bound: the reviewed plan's digest, the pending
/// operation, and the defaults the two asked commands open with.
fn internal_bindings(state: &InternalProjection) -> RowBindings {
    let operation = state.pending_operation.clone().unwrap_or_default();
    let mut bindings = RowBindings::new();
    for (id, ..) in internal_commands(state) {
        bindings = match id {
            id if id == InstallerPreviewEffect::INSTALL_INTERNAL.id => bindings.bind(
                InstallerPreviewEffect::INSTALL_INTERNAL,
                [(
                    "reviewed",
                    serde_json::Value::from(state.reviewed.clone().unwrap_or_default()),
                )],
            ),
            id if id == InstallerPreviewEffect::RETRY_INTERNAL.id => bindings.bind(
                InstallerPreviewEffect::RETRY_INTERNAL,
                [("operation_id", serde_json::Value::from(operation.clone()))],
            ),
            id if id == InstallerPreviewEffect::REMOVE_INTERNAL.id => bindings.bind(
                InstallerPreviewEffect::REMOVE_INTERNAL,
                [("operation_id", serde_json::Value::from(operation.clone()))],
            ),
            id => bindings.offer_id(id),
        };
    }
    bindings
}

/// Every internal command this projection cannot start now: its effect's id,
/// the code a program reads and the reason a person reads.
fn internal_refusals(state: &InternalProjection) -> Vec<(&'static str, &'static str, String)> {
    internal_commands(state)
        .into_iter()
        .filter(|(_, _, open, ..)| !open)
        .map(|(id, _, _, code, reason)| (id, code, super::strings::t(reason)))
        .collect()
}

/// What each internal command cannot do now, by the row's effects, with the
/// reason its projection gives.
fn internal_row_status(state: &InternalProjection) -> Vec<EffectStatus> {
    internal_refusals(state)
        .into_iter()
        .map(|(id, code, reason)| EffectStatus::unavailable(effect_ref(id), code, reason))
        .collect()
}

fn internal_row(state: &InternalProjection) -> LiveRow {
    let mut row = LiveRow::typed_with::<InternalSetup>(
        format!("{SETUP_ROW_ID}.internal"),
        super::strings::t("imager-setup-resource-internal"),
        &internal_bindings(state),
    )
    .prefilled(
        InstallerPreviewEffect::SET_INTERNAL_TARGET_SIZE,
        [("gib", serde_json::Value::from(-1))],
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
    for (id, label, ..) in internal_commands(state) {
        row = row.labelled(effect_ref(id), super::strings::t(label));
    }
    row
}

fn hardware_row(hardware: &HardwareProjection) -> LiveRow {
    let model = hardware
        .model
        .clone()
        .unwrap_or_else(|| super::strings::t("installer-hardware-unknown-model"));
    let summary = super::strings::t_args(
        if hardware.blocking {
            "installer-hardware-summary-blocking"
        } else {
            "installer-hardware-summary"
        },
        &[
            ("supported", hardware.supported.into()),
            ("limited", hardware.limited.into()),
            ("unsupported", hardware.unsupported.into()),
            ("unverified", hardware.unverified.into()),
        ],
    );
    let mut row = LiveRow::typed::<Hardware>(format!("{SETUP_ROW_ID}.hardware"), model)
        .presentation(ResourcePresentation {
            detail: Some(summary),
            badge: None,
            preview: None,
        });
    for component in &hardware.components {
        row = row.row(
            LiveRow::typed::<HardwareComponent>(
                format!("{SETUP_ROW_ID}.hardware.{}", component.kind),
                super::strings::t(&format!("installer-hardware-component-{}", component.kind)),
            )
            .presentation(ResourcePresentation {
                detail: (!component.name.is_empty()).then(|| component.name.clone()),
                badge: Some(super::strings::t(&format!(
                    "installer-hardware-state-{}",
                    component.status
                ))),
                preview: None,
            }),
        );
    }
    row
}

#[must_use]
pub fn row(state: &SetupProjection) -> LiveRow {
    let experience = match state.experience {
        "desktop" => "Tilo Desktop",
        _ => "Full Tilo",
    };
    let mut root = LiveRow::typed::<SetupRoot>(
        SETUP_ROW_ID,
        super::strings::t("imager-setup-resource-root"),
    )
    .presentation(ResourcePresentation {
        detail: Some(format!("Review, {experience}")),
        badge: Some(super::strings::t("imager-setup-badge-review")),
        preview: None,
    })
    .row(internal_row(&state.internal))
    .row(super::desktop_journey::row(&state.desktop));
    if let Some(hardware) = &state.hardware {
        root = root.row(hardware_row(hardware));
    }
    root
}

#[cfg(test)]
mod tests {
    use super::*;
    use tilo_actions::{ActionSpec, OperationKind};

    fn projection(internal: InternalProjection) -> SetupProjection {
        SetupProjection {
            experience: "full_os",
            next: false,
            internal,
            hardware: None,
            desktop: crate::setup::desktop_journey::DesktopProjection::default(),
        }
    }

    /// The internal row's actions as the host publishes them: the
    /// projection's guarded commands, each refused with the reason it gives.
    fn internal_actions(state: &SetupProjection) -> Vec<ActionSpec> {
        let mut actions = row(state)
            .into_resource()
            .children
            .into_iter()
            .find(|child| child.kind == "imager.setup.internal")
            .expect("internal row")
            .actions;
        for (id, _, open, code, reason) in internal_commands(&state.internal) {
            if open {
                continue;
            }
            if let Some(at) = actions.iter().position(|action| action.id == id) {
                let spec = actions.remove(at);
                actions.insert(at, spec.unavailable(code, crate::setup::strings::t(reason)));
            }
        }
        actions
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
    ) -> InstallerPreviewEffect {
        let encoded = action.build_json(arguments).unwrap();
        assert_eq!(encoded.type_id, InstallerPreviewEffect::OPERATION);
        InstallerPreviewEffect::from_payload(&encoded.payload).unwrap()
    }

    #[test]
    fn internal_row_offers_guarded_commands_bound_to_the_reviewed_digest() {
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
            "the declared effect is guarded like the row's"
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
            InstallerPreviewEffect::InstallInternal {
                reviewed: "a".repeat(64)
            }
        );
        assert_eq!(
            decoded(action("remove-internal"), &none),
            InstallerPreviewEffect::RemoveInternal {
                operation_id: "op-7".into()
            }
        );
        let size = action("set-internal-target-size");
        let mut args = serde_json::Map::new();
        args.insert("gib".into(), serde_json::json!(80));
        assert_eq!(
            decoded(size, &args),
            InstallerPreviewEffect::SetInternalTargetSize { gib: 80 }
        );
        args.insert("gib".into(), serde_json::json!("eighty"));
        assert!(size.build_json(&args).is_err());
    }
}
