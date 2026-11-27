//! The early hardware compatibility report on the native setup surface.

use super::*;
use crate::setup::hardware_text::{
    component_icon, component_title, evidence_text, reason_text, remedy_text, state_label,
};
use crate::setup::strings::{t, t_args};
use hardware_report::{
    Applicability, ComponentSupport, ReportSource, SupportReport as HardwareReport, SupportState,
};

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

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    /// A computer whose report has a working, a limited and an unsupported part.
    fn fixture_support_report() -> hardware_report::SupportReport {
        use hardware_report::{
            Applicability, ComponentKind, ComponentSupport, DestinationIdentity, DeviceBinding,
            Evidence, EvidenceFact, EvidenceScope, REPORT_SCHEMA, Remedy, ReportSource,
            SupportReason, SupportReport, SupportState,
        };
        let device = |name: &str, driver: Option<&str>, scope| Evidence {
            scope,
            fact: EvidenceFact::Device {
                name: name.into(),
                driver: driver.map(str::to_string),
                firmware: None,
                vendor: None,
                product: None,
                reported_problem: false,
            },
        };
        let component = |kind,
                         name: &str,
                         present,
                         state,
                         applicability,
                         reason,
                         evidence: Vec<Evidence>,
                         remedy: Option<Remedy>| {
            ComponentSupport {
                kind,
                name: name.into(),
                present,
                state,
                applicability,
                reason,
                evidence,
                remedy,
            }
        };
        SupportReport {
            schema: REPORT_SCHEMA.into(),
            identity: DestinationIdentity {
                source: ReportSource::LocalScan,
                manufacturer: Some("Test Vendor".into()),
                model: Some("Test Laptop 15".into()),
                variant: Some("TESTBOARD".into()),
                board: Some("TESTBOARD".into()),
                sku: Some("SKU-1".into()),
                firmware: Some("Test Firmware 1.2.3".into()),
            },
            kernel: Some("7.2.7-test".into()),
            architecture: Some("aarch64".into()),
            components: vec![
                component(
                    ComponentKind::Graphics,
                    "AMD graphics, NVIDIA graphics",
                    true,
                    SupportState::Supported,
                    Applicability::ReleaseVerified,
                    SupportReason::HybridGraphics {
                        primary: DeviceBinding {
                            device: "AMD graphics".into(),
                            driver: Some("amdgpu".into()),
                        },
                        additional: vec![DeviceBinding {
                            device: "NVIDIA graphics".into(),
                            driver: Some("nvidia".into()),
                        }],
                    },
                    vec![
                        device(
                            "AMD graphics",
                            Some("amdgpu"),
                            EvidenceScope::SelectedRelease,
                        ),
                        device(
                            "NVIDIA graphics",
                            Some("nvidia"),
                            EvidenceScope::SelectedRelease,
                        ),
                    ],
                    None,
                ),
                component(
                    ComponentKind::Display,
                    "eDP-1, DP-1",
                    true,
                    SupportState::Supported,
                    Applicability::ReleaseVerified,
                    SupportReason::DisplaysActive {
                        internal: 1,
                        external: 1,
                        devices: vec!["eDP-1".into(), "DP-1".into()],
                    },
                    vec![
                        Evidence {
                            scope: EvidenceScope::SelectedRelease,
                            fact: EvidenceFact::Display {
                                name: "eDP-1".into(),
                                connected: true,
                                active: true,
                            },
                        },
                        Evidence {
                            scope: EvidenceScope::SelectedRelease,
                            fact: EvidenceFact::Display {
                                name: "DP-1".into(),
                                connected: true,
                                active: true,
                            },
                        },
                    ],
                    None,
                ),
                component(
                    ComponentKind::Wifi,
                    "wlp4s0",
                    true,
                    SupportState::Supported,
                    Applicability::ReleaseVerified,
                    SupportReason::DriverBound {
                        bindings: vec![DeviceBinding {
                            device: "wlp4s0".into(),
                            driver: Some("iwlwifi".into()),
                        }],
                    },
                    vec![device(
                        "wlp4s0",
                        Some("iwlwifi"),
                        EvidenceScope::SelectedRelease,
                    )],
                    None,
                ),
                component(
                    ComponentKind::Fingerprint,
                    "Fingerprint Sensor",
                    true,
                    SupportState::Limited,
                    Applicability::ObservationOnly,
                    SupportReason::FingerprintUnverified {
                        device: "Fingerprint Sensor".into(),
                    },
                    vec![Evidence {
                        scope: EvidenceScope::Observation,
                        fact: EvidenceFact::Device {
                            name: "Fingerprint Sensor".into(),
                            driver: None,
                            firmware: None,
                            vendor: Some("1234".into()),
                            product: Some("5678".into()),
                            reported_problem: false,
                        },
                    }],
                    Some(Remedy::PasswordSignIn),
                ),
                component(
                    ComponentKind::Camera,
                    "",
                    false,
                    SupportState::Unverified,
                    Applicability::Unknown,
                    SupportReason::NotDetected,
                    Vec::new(),
                    Some(Remedy::ExternalCamera),
                ),
                component(
                    ComponentKind::Boot,
                    "",
                    true,
                    SupportState::Unverified,
                    Applicability::ObservationOnly,
                    SupportReason::Uefi {
                        secure_boot: Some(true),
                        tpm: Some("TPM 2.0".into()),
                    },
                    vec![device("UEFI", None, EvidenceScope::Observation)],
                    None,
                ),
                component(
                    ComponentKind::Storage,
                    "Test NVMe",
                    true,
                    SupportState::Supported,
                    Applicability::ReleaseVerified,
                    SupportReason::DriverBound {
                        bindings: vec![DeviceBinding {
                            device: "Test NVMe".into(),
                            driver: Some("nvme".into()),
                        }],
                    },
                    vec![device(
                        "Test NVMe",
                        Some("nvme"),
                        EvidenceScope::SelectedRelease,
                    )],
                    None,
                ),
            ],
        }
    }

    fn has_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().into_iter().any(|child| has_id(child, id))
    }

    tilo_test_env::scenario! {
        fn the_support_report_shows_limitations_and_evidence;

        phase report_sections_expand_limitations_and_keep_working_rows_compact;
        {
            let report =
                fixture_support_report();
            let mut app = SetupApp::for_full_os_check(tilo_ui_base::LocalState::new(None));
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

        phase semantic_projection_carries_the_identity_statuses_and_evidence;
        {
            let report =
                fixture_support_report();
            let mut app = SetupApp::for_full_os_check(tilo_ui_base::LocalState::new(None));
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

        phase checking_report_stays_visible_until_the_probe_reports;
        {
            let app = SetupApp::for_full_os_check(tilo_ui_base::LocalState::new(None));
            assert!(app.support_report.is_none());
            assert!(
                app.hardware_report_sections()
                    .iter()
                    .any(|widget| has_id(widget.as_ref(), "setup.hardware.checking"))
            );
        }

        phase another_computer_sections_expand_limitations_and_keep_working_rows_compact;
        {
            let report =
                fixture_support_report();
            let sections = sections(&report, Placement::AnotherComputer);
            assert!(
                sections
                    .iter()
                    .any(|widget| has_id(widget.as_ref(), "setup.hardware.model"))
            );
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

        phase a_named_model_has_no_component_results_and_says_what_is_missing;
        {
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
}
