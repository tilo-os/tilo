//! The early hardware compatibility report on the native setup surface.
//!
//! The backend reports typed facts and outcomes. This view owns every sentence:
//! component names, reasons, evidence and next steps are Fluent messages, while
//! hardware names, driver names and versions are the values those messages
//! carry.

use super::*;
use crate::setup::strings::{t, t_args};
use imager_backend::support_report::{
    Applicability, ComponentKind, ComponentSupport, DeviceBinding, Evidence, EvidenceFact,
    EvidenceScope, Remedy, ReportSource, SupportReason, SupportReport as HardwareReport,
    SupportState,
};

/// Which computer the report describes. The same component presentation is
/// used for this computer and for one prepared elsewhere; only the identity
/// title and the source wording differ.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Placement {
    ThisComputer,
    AnotherComputer,
}

impl SetupApp {
    pub(super) fn hardware_report_sections(&self) -> Vec<Box<dyn Widget>> {
        let Some(report) = &self.support_report else {
            return vec![Box::new(
                Text::new(&t("installer-hardware-checking"))
                    .with_id("setup.hardware.checking")
                    .with_live(Politeness::Polite),
            )];
        };
        sections(report, Placement::ThisComputer)
    }
}

/// The component report for either destination. Important limitations are
/// expanded with their evidence and next step; working components stay a
/// compact list so the page remains scannable.
pub(super) fn sections(report: &HardwareReport, placement: Placement) -> Vec<Box<dyn Widget>> {
    let mut sections = vec![Box::new(identity_section(report, placement)) as Box<dyn Widget>];
    sections.push(Box::new(summary_section(report, placement)));
    if let Some(section) = attention_section(report) {
        sections.push(Box::new(section));
    }
    if let Some(section) = working_section(report) {
        sections.push(Box::new(section));
    }
    if let Some(section) = not_detected_section(report) {
        sections.push(Box::new(section));
    }
    sections
}

fn identity_section(report: &HardwareReport, placement: Placement) -> Section {
    let mut rows: Vec<Box<dyn Widget>> = vec![Box::new(
        ListRow::new("setup.hardware.model", identity_label(report))
            .icon(Icon::Monitor)
            .secondary(source_label(report.identity.source, placement))
            .two_line_secondary(),
    )];
    if let Some(variant) = &report.identity.variant {
        rows.push(fact_row(
            "setup.hardware.variant",
            t("installer-hardware-variant"),
            variant,
        ));
    }
    if let Some(board) = &report.identity.board
        && report.identity.variant.as_deref() != Some(board.as_str())
    {
        rows.push(fact_row(
            "setup.hardware.board",
            t("installer-hardware-board"),
            board,
        ));
    }
    if let Some(sku) = &report.identity.sku {
        rows.push(fact_row(
            "setup.hardware.sku",
            t("installer-hardware-sku"),
            sku,
        ));
    }
    if let Some(firmware) = &report.identity.firmware {
        rows.push(fact_row(
            "setup.hardware.firmware",
            t("installer-hardware-firmware"),
            firmware,
        ));
    }
    if let Some(kernel) = &report.kernel {
        rows.push(fact_row(
            "setup.hardware.kernel",
            t("installer-hardware-running"),
            kernel,
        ));
    }
    if let Some(architecture) = &report.architecture {
        rows.push(fact_row(
            "setup.hardware.architecture",
            t("installer-hardware-architecture"),
            architecture,
        ));
    }
    Section::titled(
        t(match placement {
            Placement::ThisComputer => "installer-hardware-this-computer",
            Placement::AnotherComputer => "installer-hardware-other-computer",
        }),
        rows,
    )
}

fn identity_label(report: &HardwareReport) -> String {
    let identity = &report.identity;
    let parts = [identity.manufacturer.as_deref(), identity.model.as_deref()]
        .into_iter()
        .flatten()
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .collect::<Vec<_>>();
    let label = parts.join(" ");
    if label.is_empty() {
        return t("installer-hardware-unknown-model");
    }
    if let Some(variant) = identity
        .variant
        .as_deref()
        .map(str::trim)
        .filter(|variant| !variant.is_empty())
    {
        return t_args(
            "installer-hardware-model-with-variant",
            &[("model", label.into()), ("variant", variant.into())],
        );
    }
    label
}

fn fact_row(id: &str, label: String, value: &str) -> Box<dyn Widget> {
    Box::new(
        ListRow::new(id, label)
            .secondary(value)
            .two_line_secondary(),
    )
}

fn summary_section(report: &HardwareReport, placement: Placement) -> Section {
    let summary = report.summary();
    let mut text = if report.identity.source == ReportSource::DeclaredModel {
        t("installer-hardware-summary-declared")
    } else if summary.blocking > 0 {
        t_args(
            "installer-hardware-summary-blocking",
            &[
                ("supported", summary.supported.into()),
                ("limited", summary.limited.into()),
                ("unsupported", summary.unsupported.into()),
                ("unverified", summary.unverified.into()),
            ],
        )
    } else {
        t_args(
            "installer-hardware-summary",
            &[
                ("supported", summary.supported.into()),
                ("limited", summary.limited.into()),
                ("unsupported", summary.unsupported.into()),
                ("unverified", summary.unverified.into()),
            ],
        )
    };
    if placement == Placement::AnotherComputer && summary.blocking == 0 {
        text.push(' ');
        text.push_str(&t("installer-hardware-summary-imported-check"));
    }
    if report
        .components
        .iter()
        .any(|component| component.applicability != Applicability::ReleaseVerified)
    {
        text.push(' ');
        text.push_str(&t("installer-hardware-summary-observation"));
    }
    Section::new(vec![Box::new(
        Text::new(&text)
            .with_id("setup.hardware.summary")
            .with_live(Politeness::Polite),
    )])
}

fn attention_section(report: &HardwareReport) -> Option<Section> {
    let components = report
        .components
        .iter()
        .filter(|component| attention(component))
        .collect::<Vec<_>>();
    if components.is_empty() {
        return None;
    }
    let rows = components
        .into_iter()
        .map(|component| Box::new(attention_row(component)) as Box<dyn Widget>)
        .collect::<Vec<_>>();
    let count = rows.len();
    Some(
        Section::new(rows).with_header(
            SectionHeader::heading(t("installer-hardware-needs-attention")).count(count),
        ),
    )
}

fn attention_row(component: &ComponentSupport) -> Column {
    let mut row = ListRow::new(
        format!("setup.hardware.{}", component.kind.key()),
        component_title(component.kind),
    )
    .icon(component_icon(component.kind))
    .secondary(reason_text(component))
    .two_line_secondary();
    if !component.name.is_empty() {
        row = row.tooltip(component.name.clone());
    }
    row = match component.state {
        SupportState::Unsupported => row.error(),
        SupportState::Limited => row.warning(),
        SupportState::Unverified => row.idle(),
        SupportState::Supported => row,
    };
    let mut content = super::page::related().push(Box::new(row));
    if !component.evidence.is_empty() {
        for (index, evidence) in component.evidence.iter().enumerate() {
            content = content.push(Box::new(
                Text::new(&evidence_text(evidence))
                    .with_role(TextRole::Caption)
                    .dimmed()
                    .with_id(format!(
                        "setup.hardware.{}.evidence.{}",
                        component.kind.key(),
                        index
                    )),
            ));
        }
    }
    if let Some(remedy) = component.remedy {
        content = content.push(Box::new(
            Text::new(&remedy_text(component.kind, remedy))
                .with_role(TextRole::Caption)
                .with_id(format!("setup.hardware.{}.remedy", component.kind.key())),
        ));
    }
    if component.applicability != Applicability::ReleaseVerified
        && component.state != SupportState::Supported
    {
        content = content.push(Box::new(
            Text::new(&t("installer-hardware-scope-observation"))
                .with_role(TextRole::Caption)
                .dimmed()
                .with_id(format!("setup.hardware.{}.scope", component.kind.key())),
        ));
    }
    content
}

fn working_section(report: &HardwareReport) -> Option<Section> {
    let components = report.working().collect::<Vec<_>>();
    if components.is_empty() {
        return None;
    }
    let rows = components
        .into_iter()
        .map(|component| {
            Box::new(
                ListRow::new(
                    format!("setup.hardware.{}", component.kind.key()),
                    component_title(component.kind),
                )
                .icon(Icon::CircleCheck)
                .secondary(t("installer-hardware-state-supported"))
                .tooltip(accessible_component(component)),
            ) as Box<dyn Widget>
        })
        .collect::<Vec<_>>();
    let count = rows.len();
    Some(
        Section::new(rows)
            .with_header(SectionHeader::heading(t("installer-hardware-working")).count(count)),
    )
}

fn not_detected_section(report: &HardwareReport) -> Option<Section> {
    let components = report
        .components
        .iter()
        .filter(|component| {
            !component.present
                && component.state != SupportState::Supported
                && component.state != SupportState::Unsupported
                && !component.kind.critical()
        })
        .collect::<Vec<_>>();
    if components.is_empty() {
        return None;
    }
    let rows = components
        .into_iter()
        .map(|component| {
            Box::new(
                ListRow::new(
                    format!("setup.hardware.{}", component.kind.key()),
                    component_title(component.kind),
                )
                .icon(Icon::Info)
                .secondary(t("installer-hardware-state-not-detected"))
                .tooltip(accessible_component(component)),
            ) as Box<dyn Widget>
        })
        .collect::<Vec<_>>();
    let count = rows.len();
    Some(
        Section::new(rows)
            .with_header(SectionHeader::heading(t("installer-hardware-not-detected")).count(count)),
    )
}

fn attention(component: &ComponentSupport) -> bool {
    component.state != SupportState::Supported
        && (component.present
            || component.state == SupportState::Unsupported
            || component.kind.critical())
}

fn accessible_component(component: &ComponentSupport) -> String {
    let mut text = format!(
        "{}: {}. {}",
        component_title(component.kind),
        state_label(component.state),
        reason_text(component)
    );
    if !component.name.is_empty() {
        text.push(' ');
        text.push_str(&component.name);
        text.push('.');
    }
    for evidence in &component.evidence {
        text.push(' ');
        text.push_str(&evidence_text(evidence));
    }
    if let Some(remedy) = component.remedy {
        text.push(' ');
        text.push_str(&remedy_text(component.kind, remedy));
    }
    text
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

fn state_label(state: SupportState) -> String {
    t(match state {
        SupportState::Supported => "installer-hardware-state-supported",
        SupportState::Limited => "installer-hardware-state-limited",
        SupportState::Unsupported => "installer-hardware-state-unsupported",
        SupportState::Unverified => "installer-hardware-state-unverified",
    })
}

fn reason_text(component: &ComponentSupport) -> String {
    let reason = &component.reason;
    match reason {
        SupportReason::DriverBound { bindings } => {
            if bindings.len() == 1 {
                binding_text("installer-hardware-reason-driver-bound-one", &bindings[0])
            } else {
                t_args(
                    "installer-hardware-reason-driver-bound-many",
                    &[("count", bindings.len().into())],
                )
            }
        }
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
            status,
            health,
        } => {
            let mut text = t_args(
                "installer-hardware-reason-battery",
                &[("device", device.as_str().into())],
            );
            if let Some(capacity) = capacity {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-battery-capacity",
                    &[("capacity", (*capacity).into())],
                ));
            }
            if let Some(status) = status {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-battery-status",
                    &[("status", status.as_str().into())],
                ));
            }
            if let Some(health) = health {
                text.push(' ');
                text.push_str(&t_args(
                    "installer-hardware-reason-battery-health",
                    &[("health", (*health).into())],
                ));
            }
            text
        }
        SupportReason::AcOnly => t("installer-hardware-reason-ac-only"),
        SupportReason::AcpiTables { tables } => t_args(
            "installer-hardware-reason-acpi-tables",
            &[("tables", (*tables).into())],
        ),
        SupportReason::Uefi { secure_boot, tpm } => {
            let mut text = t("installer-hardware-reason-uefi");
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
        SupportReason::LegacyBoot => t("installer-hardware-reason-legacy-boot"),
        SupportReason::StartupUnknown => t("installer-hardware-reason-startup-unknown"),
        SupportReason::InputDevices { count } => t_args(
            "installer-hardware-reason-input-devices",
            &[("count", (*count).into())],
        ),
        SupportReason::NoKeyboard => t("installer-hardware-reason-no-keyboard"),
    }
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
                    "installer-hardware-evidence-firmware-version",
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

fn source_label(source: ReportSource, placement: Placement) -> String {
    t(match (source, placement) {
        (ReportSource::LocalScan, Placement::AnotherComputer) => {
            "installer-hardware-source-this-computer-not-other"
        }
        (ReportSource::LocalScan, Placement::ThisComputer) => {
            "installer-hardware-source-this-computer"
        }
        (ReportSource::Imported, _) => "installer-hardware-source-imported",
        (ReportSource::DeclaredModel, _) => "installer-hardware-source-declared",
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
mod tests {
    use super::*;

    fn has_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().into_iter().any(|child| has_id(child, id))
    }

    #[test]
    fn report_sections_expand_limitations_and_keep_working_rows_compact() {
        let report =
            super::super::super::journey::main_story_tests::fixture_support_report().unwrap();
        let mut app = SetupApp::for_full_os_check(ui::LocalState::new(None));
        app.support_report = Some(report);
        let sections = app.hardware_report_sections();
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.model"))
        );
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.summary"))
        );
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.fingerprint"))
        );
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.wifi"))
        );
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.camera"))
        );
    }

    #[test]
    fn semantic_projection_carries_the_identity_statuses_and_evidence() {
        let report =
            super::super::super::journey::main_story_tests::fixture_support_report().unwrap();
        let mut app = SetupApp::for_full_os_check(ui::LocalState::new(None));
        app.support_report = Some(report);
        let hardware = app.setup_projection().hardware.unwrap();
        assert_eq!(hardware.source, "local-scan");
        assert_eq!(hardware.model.as_deref(), Some("Test Laptop 15"));
        assert_eq!(hardware.variant.as_deref(), Some("TESTBOARD"));
        assert!(hardware.supported >= 4);
        assert_eq!(hardware.limited, 1);
        assert!(!hardware.blocking);
        let graphics = hardware
            .components
            .iter()
            .find(|component| component.kind == "graphics")
            .unwrap();
        assert_eq!(graphics.status, "supported");
        let evidence = serde_json::to_value(&graphics.evidence).unwrap();
        assert!(evidence.to_string().contains("amdgpu"));
        let fingerprint = hardware
            .components
            .iter()
            .find(|component| component.kind == "fingerprint")
            .unwrap();
        assert_eq!(fingerprint.status, "limited");
        assert_eq!(
            serde_json::to_value(fingerprint.remedy).unwrap(),
            serde_json::json!("password-sign-in")
        );
    }

    #[test]
    fn checking_report_stays_visible_until_the_probe_reports() {
        let app = SetupApp::for_full_os_check(ui::LocalState::new(None));
        assert!(app.support_report.is_none());
        assert!(
            app.hardware_report_sections()
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.checking"))
        );
    }

    #[test]
    fn another_computer_sections_expand_limitations_and_keep_working_rows_compact() {
        let report =
            super::super::super::journey::main_story_tests::fixture_support_report().unwrap();
        let sections = sections(&report, Placement::AnotherComputer);
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.model"))
        );
        // The limited fingerprint reader is expanded with its evidence and
        // remedy; the supported Wi-Fi adapter is a compact working row.
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.fingerprint.evidence.0"))
        );
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.fingerprint.remedy"))
        );
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.wifi"))
        );
        assert!(
            !sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.wifi.evidence.0"))
        );
    }

    #[test]
    fn a_named_model_has_no_component_results_and_says_what_is_missing() {
        let report = HardwareReport::declared_model_with(
            "Prepared Vendor",
            "Prepared Laptop 14",
            "PREPARED-BOARD",
            Some("PREPARED-BOARD"),
            None,
        )
        .unwrap();
        let sections = sections(&report, Placement::AnotherComputer);
        assert!(
            sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.summary"))
        );
        assert!(
            !sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.graphics"))
        );
        assert!(
            !sections
                .iter()
                .any(|widget| has_id(widget.as_ref(), "setup.hardware.wifi"))
        );
    }
}
