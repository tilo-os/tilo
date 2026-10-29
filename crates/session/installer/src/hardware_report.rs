//! The component-by-component hardware report the installer shows before it
//! writes anything.
//!
//! The probe and its report schema are `imager_backend::support_report`,
//! shared with Tilo Imager, so both doors observe a machine the same way and
//! name the same components in the same words. This module owns only what the
//! installer draws: the sentence above the rows (`summary_text`) and
//! the rows themselves (`component_rows`), plus
//! the same facts as data (`component_facts`) for a caller with no screen.
//!
//! The report never decides the install and grants nothing. It is read once,
//! before the plan is written, and shown on Review where the person is
//! already reading what this computer will become. What a running system can
//! establish is narrow: it shows which devices exist and which drivers it
//! bound. It cannot show that the selected Tilo release ships the same
//! kernel, firmware and userspace service for them, so an observed component
//! is never reported as supported — only `imager-backend` marks a component
//! `Supported`, and only with evidence checked against that release.

use imager_backend::support_report::{
    Applicability, ComponentKind, ComponentSupport, DeviceBinding, Evidence, EvidenceFact,
    EvidenceScope, Remedy, ReportSource, SupportReason, SupportReport, SupportState,
};
use ui::icons::Icon;
use ui::tokens::typography::TextRole;
use ui::widget::list_row::ListRow;
use ui::widget::{Text, Widget};

use crate::i18n::{t, t_args};

/// What the installer knows about this computer's components: `Checking`
/// until the probe returns, `Unavailable` when the machine cannot be
/// identified specifically enough for the shared report to validate.
#[derive(Clone, Debug, Default)]
pub(crate) enum HardwareReport {
    /// The probe has not returned yet.
    #[default]
    Checking,
    Ready(Box<SupportReport>),
    Unavailable,
}

impl HardwareReport {
    /// Read this computer. Runs on the caller's thread and touches nothing:
    /// the probe reads sysfs and `/proc`, never mounts, opens a block device
    /// for writing or asks the firmware to change anything.
    pub(crate) fn probe() -> Self {
        Self::from_report(SupportReport::probe())
    }

    /// Hold a report read elsewhere, under the same checks the probe's own
    /// reading passes.
    pub(crate) fn from_report(report: SupportReport) -> Self {
        if report.validate().is_err() || report.components.is_empty() {
            return Self::Unavailable;
        }
        Self::Ready(Box::new(report))
    }

    pub(crate) fn report(&self) -> Option<&SupportReport> {
        match self {
            Self::Ready(report) => Some(report),
            Self::Checking | Self::Unavailable => None,
        }
    }

    /// The summary a caller without a screen reads, or `None` before the
    /// probe has returned.
    pub(crate) fn to_json(&self) -> serde_json::Value {
        let Some(report) = self.report() else {
            return serde_json::json!({
                "state": match self {
                    Self::Checking => "checking",
                    Self::Unavailable | Self::Ready(_) => "unavailable",
                },
            });
        };
        let summary = report.summary();
        serde_json::json!({
            "state": "ready",
            "model": report.identity.label(),
            "source": match report.identity.source {
                ReportSource::LocalScan => "local-scan",
                ReportSource::Imported => "imported",
                ReportSource::DeclaredModel => "declared-model",
            },
            "kernel": report.kernel,
            "architecture": report.architecture,
            "summary": {
                "supported": summary.supported,
                "limited": summary.limited,
                "unsupported": summary.unsupported,
                "unverified": summary.unverified,
            },
            "blocks_install": report.blocks_install(),
            "components": report
                .components
                .iter()
                .map(component_facts)
                .collect::<Vec<_>>(),
        })
    }
}

/// The report's headline: what the probe established, in one sentence, and
/// what it does not.
pub(crate) fn summary_text(report: &SupportReport) -> String {
    let summary = report.summary();
    let counts: &[(&str, ui::i18n::ArgValue)] = &[
        ("supported", summary.supported.into()),
        ("limited", summary.limited.into()),
        ("unsupported", summary.unsupported.into()),
        ("unverified", summary.unverified.into()),
    ];
    let mut text = if summary.blocking > 0 {
        t_args("installer-hardware-summary-blocking", counts)
    } else {
        t_args("installer-hardware-summary", counts)
    };
    if report
        .components
        .iter()
        .any(|component| component.applicability != Applicability::ReleaseVerified)
    {
        text.push(' ');
        text.push_str(&t("installer-hardware-summary-observation"));
    }
    text
}

/// The headline row: the sentence above, or the state the probe is still in.
pub(crate) fn summary_row(report: &HardwareReport) -> Box<dyn Widget> {
    match report {
        HardwareReport::Checking => Box::new(
            Text::new(&t("installer-hardware-checking"))
                .with_id("installer.hardware.checking")
                .with_role(TextRole::Caption)
                .with_live(ui::a11y::Politeness::Polite),
        ),
        HardwareReport::Unavailable => Box::new(
            Text::new(&t("installer-hardware-unavailable"))
                .with_id("installer.hardware.unavailable")
                .with_role(TextRole::Caption),
        ),
        HardwareReport::Ready(report) => Box::new(
            Text::new(&summary_text(report))
                .with_id("installer.hardware.summary")
                .with_role(TextRole::Body)
                .with_live(ui::a11y::Politeness::Polite),
        ),
    }
}

/// Every component the probe covered, essential classes first: a person
/// reads what the desktop needs (graphics, display, startup, storage) before
/// what is nice to have (a fingerprint reader).
pub(crate) fn component_rows(report: &SupportReport) -> Vec<Box<dyn Widget>> {
    let mut components = report.components.iter().collect::<Vec<_>>();
    components.sort_by_key(|component| (order(component.kind), component.kind.key()));
    components.into_iter().map(component_row).collect()
}

/// The order rows appear in: the four classes a usable desktop needs, then
/// the rest in the order the report names them.
fn order(kind: ComponentKind) -> u8 {
    match kind {
        ComponentKind::Boot => 0,
        ComponentKind::Storage => 1,
        ComponentKind::Graphics => 2,
        ComponentKind::Display => 3,
        ComponentKind::Input => 4,
        ComponentKind::Power => 5,
        ComponentKind::Wifi => 6,
        ComponentKind::Ethernet => 7,
        ComponentKind::Audio => 8,
        ComponentKind::Microphone => 9,
        ComponentKind::Bluetooth => 10,
        ComponentKind::Camera => 11,
        ComponentKind::Fingerprint => 12,
        ComponentKind::Acpi => 13,
    }
}

fn component_row(component: &ComponentSupport) -> Box<dyn Widget> {
    let key = component.kind.key();
    let mut row = ListRow::new(
        format!("installer.hardware.{key}"),
        component_title(component.kind),
    )
    .icon(component_icon(component.kind))
    .secondary(reason_text(component))
    .two_line_secondary();
    if !component.name.trim().is_empty() {
        row = row.tooltip(component.name.clone());
    }
    row = match component.state {
        SupportState::Supported => row,
        SupportState::Limited => row.warning(),
        SupportState::Unsupported => row.error(),
        SupportState::Unverified => row.idle(),
    };
    let mut lines: Vec<Box<dyn Widget>> = vec![Box::new(row)];
    if let Some(remedy) = component.remedy {
        lines.push(Box::new(
            Text::new(&remedy_text(component.kind, remedy))
                .with_id(format!("installer.hardware.{key}.next"))
                .with_role(TextRole::Caption),
        ));
    }
    if component.applicability != Applicability::ReleaseVerified
        && component.state != SupportState::Supported
    {
        lines.push(Box::new(
            Text::new(&t("installer-hardware-scope-observation"))
                .with_id(format!("installer.hardware.{key}.scope"))
                .with_role(TextRole::Caption),
        ));
    }
    if lines.len() == 1 {
        return lines.remove(0);
    }
    let mut detail = ui::widget::Column::new()
        .with_id(format!("installer.hardware.{key}.detail"))
        .with_intrinsic_height()
        .with_gap(0.0);
    for line in lines {
        detail = detail.push(line);
    }
    Box::new(detail)
}

/// One component as data: the same class, state, reason, evidence and next
/// step the row draws, in the words the row shows.
pub(crate) fn component_facts(component: &ComponentSupport) -> serde_json::Value {
    serde_json::json!({
        "component": component.kind.key(),
        "name": component.name,
        "present": component.present,
        "state": component.state.key(),
        "applicability": match component.applicability {
            Applicability::ReleaseVerified => "release-verified",
            Applicability::ObservationOnly => "observation-only",
            Applicability::Unknown => "unknown",
        },
        "reason": reason_text(component),
        "evidence": component.evidence.iter().map(evidence_text).collect::<Vec<_>>(),
        "next": component.remedy.map(|remedy| remedy_text(component.kind, remedy)),
        "blocks_install": component.blocks_install(),
    })
}

fn component_title(kind: ComponentKind) -> String {
    t(match kind {
        ComponentKind::Graphics => "installer-hardware-component-graphics",
        ComponentKind::Display => "installer-hardware-component-display",
        ComponentKind::Wifi => "installer-hardware-component-wifi",
        ComponentKind::Ethernet => "installer-hardware-component-ethernet",
        ComponentKind::Bluetooth => "installer-hardware-component-bluetooth",
        ComponentKind::Audio => "installer-hardware-component-audio",
        ComponentKind::Microphone => "installer-hardware-component-microphone",
        ComponentKind::Camera => "installer-hardware-component-camera",
        ComponentKind::Fingerprint => "installer-hardware-component-fingerprint",
        ComponentKind::Power => "installer-hardware-component-power",
        ComponentKind::Acpi => "installer-hardware-component-acpi",
        ComponentKind::Boot => "installer-hardware-component-boot",
        ComponentKind::Storage => "installer-hardware-component-storage",
        ComponentKind::Input => "installer-hardware-component-input",
    })
}

fn reason_text(component: &ComponentSupport) -> String {
    match &component.reason {
        SupportReason::DriverBound { bindings } => match bindings.as_slice() {
            [only] => binding_text("installer-hardware-reason-driver-bound-one", only),
            _ => t_args(
                "installer-hardware-reason-driver-bound-many",
                &[("count", bindings.len().into())],
            ),
        },
        SupportReason::HybridGraphics { primary, .. } => {
            binding_text("installer-hardware-reason-hybrid-graphics", primary)
        }
        SupportReason::DriverMissing { devices } => t_args(
            "installer-hardware-reason-driver-missing",
            &[("count", devices.len().into())],
        ),
        SupportReason::VirtualAdapter { device } => t_args(
            "installer-hardware-reason-virtual-adapter",
            &[("device", device.as_str().into())],
        ),
        SupportReason::OpenNvidia { device } => t_args(
            "installer-hardware-reason-open-nvidia",
            &[("device", device.as_str().into())],
        ),
        SupportReason::PlatformPipeline { device } => t_args(
            "installer-hardware-reason-platform-pipeline",
            &[("device", device.as_str().into())],
        ),
        SupportReason::NotDetected => t("installer-hardware-reason-not-detected"),
        SupportReason::DisplayDisconnected => t("installer-hardware-reason-display-disconnected"),
        SupportReason::DisplayInactive { device } => t_args(
            "installer-hardware-reason-display-inactive",
            &[("device", device.as_str().into())],
        ),
        SupportReason::DisplaysActive {
            internal, external, ..
        } => t_args(
            "installer-hardware-reason-displays-active",
            &[
                ("internal", (*internal).into()),
                ("external", (*external).into()),
            ],
        ),
        SupportReason::NoPlayback { device } => t_args(
            "installer-hardware-reason-no-playback",
            &[("device", device.as_str().into())],
        ),
        SupportReason::NoCapture { device } => t_args(
            "installer-hardware-reason-no-capture",
            &[("device", device.as_str().into())],
        ),
        SupportReason::FingerprintUnverified { device } => t_args(
            "installer-hardware-reason-fingerprint-unverified",
            &[("device", device.as_str().into())],
        ),
        SupportReason::SuspendUnavailable => t("installer-hardware-reason-suspend-unavailable"),
        SupportReason::Battery {
            device,
            capacity,
            health,
            ..
        } => battery_text(device, *capacity, *health),
        SupportReason::AcOnly => t("installer-hardware-reason-ac-only"),
        SupportReason::AcpiTables { tables } => t_args(
            "installer-hardware-reason-acpi-tables",
            &[("tables", (*tables).into())],
        ),
        SupportReason::Uefi { secure_boot, tpm } => uefi_text(*secure_boot, tpm.as_deref()),
        SupportReason::LegacyBoot => t("installer-hardware-reason-legacy-boot"),
        SupportReason::StartupUnknown => t("installer-hardware-reason-startup-unknown"),
        SupportReason::InputDevices { count } => t_args(
            "installer-hardware-reason-input-devices",
            &[("count", (*count).into())],
        ),
        SupportReason::NoKeyboard => t("installer-hardware-reason-no-keyboard"),
        SupportReason::StartsHere { .. }
        | SupportReason::OtherArchitecture { .. }
        | SupportReason::BoardWithoutDevicetree { .. } => startup_text(&component.reason),
    }
}

/// The startup facts a person meets before anything else, told apart from the
/// components that are merely present: which architecture the image starts on,
/// the board it was built for, and when either makes starting impossible.
fn startup_text(reason: &SupportReason) -> String {
    match reason {
        SupportReason::StartsHere {
            architecture,
            board,
            devicetree,
            secure_boot,
            tpm,
        } => {
            let mut text = t_args(
                "installer-hardware-reason-starts-here",
                &[("architecture", architecture.as_str().into())],
            );
            if let Some(board) = board {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-starts-board",
                    &[("board", board.as_str().into())],
                ));
            }
            if let Some(devicetree) = devicetree {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-starts-devicetree",
                    &[("devicetree", devicetree.as_str().into())],
                ));
            }
            if let Some(secure_boot) = secure_boot {
                text.push(' ');
                text.push_str(&t(if *secure_boot {
                    "installer-hardware-reason-secure-boot-on"
                } else {
                    "installer-hardware-reason-secure-boot-off"
                }));
            }
            if let Some(tpm) = tpm {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-tpm",
                    &[("tpm", tpm.as_str().into())],
                ));
            }
            text
        }
        SupportReason::OtherArchitecture {
            architecture,
            running,
        } => t_args(
            "installer-hardware-reason-other-architecture",
            &[
                ("architecture", architecture.as_str().into()),
                ("running", running.as_str().into()),
            ],
        ),
        SupportReason::BoardWithoutDevicetree {
            board,
            architecture,
        } => t_args(
            "installer-hardware-reason-board-without-devicetree",
            &[
                ("board", board.as_str().into()),
                ("architecture", architecture.as_str().into()),
            ],
        ),
        // Only the three startup reasons reach this function; anything else
        // would leave a row with no reason, which a person must never read
        // as a good sign, so it says the probe did not describe it.
        _ => t("installer-hardware-reason-not-detected"),
    }
}

/// A battery says its name, and the two numbers a person can act on when the
/// probe read them. Either number may be missing; the sentence stays whole.
fn battery_text(device: &str, capacity: Option<u32>, health: Option<u32>) -> String {
    let mut text = t_args(
        "installer-hardware-reason-battery",
        &[("device", device.into())],
    );
    if let Some(capacity) = capacity {
        text.push(' ');
        text.push_str(&t_args(
            "installer-hardware-reason-battery-capacity",
            &[("capacity", capacity.into())],
        ));
    }
    if let Some(health) = health {
        text.push(' ');
        text.push_str(&t_args(
            "installer-hardware-reason-battery-health",
            &[("health", health.into())],
        ));
    }
    text
}

/// How this computer starts: UEFI, the state of Secure Boot when the probe
/// could read it, and the TPM it found. Each fact is dropped when unknown
/// rather than guessed.
fn uefi_text(secure_boot: Option<bool>, tpm: Option<&str>) -> String {
    let mut text = t("installer-hardware-reason-uefi");
    if let Some(secure_boot) = secure_boot {
        text.push(' ');
        text.push_str(&t(if secure_boot {
            "installer-hardware-reason-secure-boot-on"
        } else {
            "installer-hardware-reason-secure-boot-off"
        }));
    }
    if let Some(tpm) = tpm {
        text.push(' ');
        text.push_str(&t_args(
            "installer-hardware-reason-tpm",
            &[("tpm", tpm.into())],
        ));
    }
    text
}

fn binding_text(key: &str, binding: &DeviceBinding) -> String {
    match &binding.driver {
        Some(driver) => t_args(
            key,
            &[
                ("device", binding.device.as_str().into()),
                ("driver", driver.as_str().into()),
            ],
        ),
        None => t_args(
            &format!("{key}-no-driver"),
            &[("device", binding.device.as_str().into())],
        ),
    }
}

fn evidence_text(evidence: &Evidence) -> String {
    let text = match &evidence.fact {
        EvidenceFact::Device {
            name,
            driver,
            firmware,
            vendor,
            product,
            reported_problem,
        } => {
            let mut parts = vec![name.clone()];
            if let Some(driver) = driver {
                parts.push(t_args(
                    "installer-hardware-evidence-driver",
                    &[("driver", driver.as_str().into())],
                ));
            }
            if let Some(firmware) = firmware {
                parts.push(t_args(
                    "installer-hardware-evidence-firmware",
                    &[("firmware", firmware.as_str().into())],
                ));
            }
            if let Some(vendor) = vendor {
                parts.push(t_args(
                    "installer-hardware-evidence-vendor",
                    &[("vendor", vendor.as_str().into())],
                ));
            }
            if let Some(product) = product {
                parts.push(t_args(
                    "installer-hardware-evidence-product",
                    &[("product", product.as_str().into())],
                ));
            }
            if *reported_problem {
                parts.push(t("installer-hardware-evidence-problem"));
            }
            parts.join(", ")
        }
        EvidenceFact::Display {
            name,
            connected,
            active,
        } => {
            let state = if *active {
                t("installer-hardware-evidence-display-active")
            } else if *connected {
                t("installer-hardware-evidence-display-connected")
            } else {
                t("installer-hardware-evidence-display-disconnected")
            };
            t_args(
                "installer-hardware-evidence-display",
                &[("name", name.as_str().into()), ("state", state.into())],
            )
        }
        EvidenceFact::Kernel { version } => t_args(
            "installer-hardware-evidence-kernel",
            &[("version", version.as_str().into())],
        ),
        EvidenceFact::Acpi { tables } => t_args(
            "installer-hardware-evidence-acpi",
            &[("tables", (*tables).into())],
        ),
        EvidenceFact::Suspend { states } => t_args(
            "installer-hardware-evidence-suspend",
            &[("states", states.join(", ").into())],
        ),
        EvidenceFact::Architecture { name } => t_args(
            "installer-hardware-evidence-architecture",
            &[("architecture", name.as_str().into())],
        ),
        EvidenceFact::Virtualization { names } => t_args(
            "installer-hardware-evidence-virtualization",
            &[("names", names.join(", ").into())],
        ),
        EvidenceFact::Board { id, devicetree } => {
            let mut text = t_args(
                "installer-hardware-evidence-board",
                &[("board", id.as_str().into())],
            );
            if let Some(devicetree) = devicetree {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-evidence-devicetree",
                    &[("devicetree", devicetree.as_str().into())],
                ));
            }
            text
        }
    };
    if evidence.scope == EvidenceScope::SelectedRelease {
        t_args(
            "installer-hardware-evidence-release",
            &[("fact", text.into())],
        )
    } else {
        text
    }
}

fn remedy_text(kind: ComponentKind, remedy: Remedy) -> String {
    t(match remedy {
        Remedy::RunOnDestination => "installer-hardware-remedy-run-on-destination",
        Remedy::Connect => match kind {
            ComponentKind::Display => "installer-hardware-remedy-connect-display",
            ComponentKind::Audio | ComponentKind::Microphone => {
                "installer-hardware-remedy-connect-audio"
            }
            _ => "installer-hardware-remedy-connect",
        },
        Remedy::CheckDriver => "installer-hardware-remedy-check-driver",
        Remedy::PasswordSignIn => "installer-hardware-remedy-password",
        Remedy::ExternalCamera => "installer-hardware-remedy-external-camera",
        Remedy::ExternalKeyboard => "installer-hardware-remedy-external-keyboard",
        Remedy::PowerSettings => "installer-hardware-remedy-power-settings",
        Remedy::DisplaySettings => "installer-hardware-remedy-display-settings",
        Remedy::UefiBoot => "installer-hardware-remedy-uefi",
        Remedy::PowerAdapter => "installer-hardware-remedy-power-adapter",
    })
}

fn component_icon(kind: ComponentKind) -> Icon {
    match kind {
        ComponentKind::Graphics | ComponentKind::Display => Icon::Monitor,
        ComponentKind::Wifi => Icon::Wifi,
        ComponentKind::Ethernet => Icon::EthernetPort,
        ComponentKind::Bluetooth => Icon::Bluetooth,
        ComponentKind::Audio => Icon::Volume2,
        ComponentKind::Microphone => Icon::Mic,
        ComponentKind::Camera => Icon::Camera,
        ComponentKind::Fingerprint => Icon::ScanLine,
        ComponentKind::Power => Icon::Battery,
        ComponentKind::Acpi => Icon::Power,
        ComponentKind::Boot => Icon::Shield,
        ComponentKind::Storage => Icon::HardDrive,
        ComponentKind::Input => Icon::Keyboard,
    }
}

#[cfg(test)]
mod tests;
