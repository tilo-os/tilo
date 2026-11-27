//! The hardware compatibility report's sections: the same rows for the setup
//! window, the live installer and any other surface that shows it.

// Each surface reads what the other one cannot: a build carrying only the setup window has no
// live installer to publish the report to a console caller, and one carrying only the live
// installer reads no report of another computer.
#![cfg_attr(
    not(feature = "destination"),
    allow(
        dead_code,
        reason = "the live installer is the surface that reads the report to a console caller"
    )
)]
#![cfg_attr(
    not(feature = "installation"),
    allow(
        dead_code,
        reason = "the setup window is the surface that reads a report of another computer"
    )
)]

use crate::page;
use crate::setup::hardware_text::{
    component_icon, component_title, evidence_text, reason_text, remedy_text, state_label,
};
use crate::setup::strings::{t, t_args};
use hardware_report::{
    Applicability, ComponentKind, ComponentSupport, ReportSource, SupportReport as HardwareReport,
    SupportState,
};
use tilo_ui_base::a11y::Politeness;
use tilo_ui_base::icons::Icon;
use tilo_ui_base::tokens::typography::TextRole;
use tilo_ui_base::widget::Widget;
use ui::widget::{Column, ListRow, Section, SectionHeader, Text};

/// Where the report is shown, which decides how the surface names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Placement {
    /// This computer, read where the person runs the setup.
    ThisComputer,
    /// Another computer, whose report this one carries.
    AnotherComputer,
    /// The live installer's medium: it reads, and installs on, this computer.
    LiveMedium,
}

impl Placement {
    /// The report is of the computer the person is looking at.
    fn this_computer(self) -> bool {
        matches!(self, Self::ThisComputer | Self::LiveMedium)
    }
}

/// The report a probe produced, when it identifies this computer and carries results: a report
/// that cannot say either is reported as unreadable, never guessed at.
pub(crate) fn checked(report: HardwareReport) -> Option<HardwareReport> {
    (report.validate().is_ok() && !report.components.is_empty()).then_some(report)
}

/// The report could not be read: say so, and guess nothing.
pub(crate) fn unavailable() -> Box<dyn Widget> {
    Box::new(
        Text::new(&t("installer-hardware-unavailable"))
            .with_id("setup.hardware.unavailable")
            .with_role(TextRole::Caption),
    )
}

/// The probe has not answered yet.
#[cfg(feature = "installation")]
pub(crate) fn checking() -> Box<dyn Widget> {
    Box::new(
        Text::new(&t("installer-hardware-checking"))
            .with_id("setup.hardware.checking")
            .with_live(Politeness::Polite),
    )
}

/// The report's facts, for a caller without a screen.
pub(crate) fn facts(report: Option<&HardwareReport>) -> serde_json::Value {
    let Some(report) = report else {
        return serde_json::json!({ "state": "unavailable" });
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

/// One component's facts, as the report drew its row.
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

/// The order a person reads the report in: what decides whether an installation can go
/// ahead first, then the rest from the outside in.
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

/// The same components, in the order the report reads.
fn sorted<'a>(mut components: Vec<&'a ComponentSupport>) -> Vec<&'a ComponentSupport> {
    components.sort_by_key(|component| (order(component.kind), component.kind.key()));
    components
}

pub(crate) fn sections(report: &HardwareReport, placement: Placement) -> Vec<Box<dyn Widget>> {
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
        if placement.this_computer() {
            t("installer-hardware-this-computer")
        } else {
            t("installer-hardware-other-computer")
        },
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
    let components = sorted(
        report
            .components
            .iter()
            .filter(|component| attention(component))
            .collect(),
    );
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
    let mut content = page::related().push(Box::new(row));
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
    let components = sorted(report.working().collect());
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
    let components = sorted(
        report
            .components
            .iter()
            .filter(|component| {
                !component.present
                    && component.state != SupportState::Supported
                    && component.state != SupportState::Unsupported
                    && !component.kind.critical()
            })
            .collect(),
    );
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
    if source == ReportSource::LocalScan && !placement.this_computer() {
        return t("installer-hardware-source-this-computer-not-other");
    }
    t(match source {
        ReportSource::LocalScan => "installer-hardware-source-this-computer",
        ReportSource::Imported => "installer-hardware-source-imported",
        ReportSource::DeclaredModel => "installer-hardware-source-declared",
    })
}

#[cfg(all(test, target_os = "linux"))]
pub(crate) mod tests {
    use super::*;
    use hardware_report::{
        DestinationIdentity, DeviceBinding, Evidence, EvidenceFact, EvidenceScope, REPORT_SCHEMA,
        Remedy, SupportReason, SupportSummary,
    };

    /// A computer whose report has a working, a limited and an unsupported part.
    pub(crate) fn fixture_support_report() -> HardwareReport {
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

    fn binding(device: &str, driver: Option<&str>) -> DeviceBinding {
        DeviceBinding {
            device: device.into(),
            driver: driver.map(str::to_owned),
        }
    }

    fn component(
        kind: ComponentKind,
        state: SupportState,
        reason: SupportReason,
        remedy: Option<Remedy>,
    ) -> ComponentSupport {
        ComponentSupport {
            kind,
            name: "Observed 9000".into(),
            present: true,
            state,
            applicability: Applicability::ObservationOnly,
            reason,
            evidence: Vec::new(),
            remedy,
        }
    }

    fn report(components: Vec<ComponentSupport>) -> HardwareReport {
        HardwareReport {
            schema: REPORT_SCHEMA.into(),
            identity: DestinationIdentity {
                source: ReportSource::LocalScan,
                manufacturer: Some("Example".into()),
                model: Some("Laptop 14".into()),
                variant: Some("BOARD-1".into()),
                board: None,
                sku: None,
                firmware: None,
            },
            kernel: Some("6.17.0".into()),
            architecture: Some("x86_64".into()),
            components,
        }
    }

    fn observed_graphics() -> ComponentSupport {
        component(
            ComponentKind::Graphics,
            SupportState::Unverified,
            SupportReason::DriverBound {
                bindings: vec![binding("card0", Some("amdgpu"))],
            },
            None,
        )
    }

    fn has_id(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().into_iter().any(|child| has_id(child, id))
    }

    fn any_has_id(sections: &[Box<dyn Widget>], id: &str) -> bool {
        sections.iter().any(|widget| has_id(widget.as_ref(), id))
    }

    /// Every test id the tree carries, in the order a person reads it.
    fn ids_in_order(widget: &dyn Widget) -> Vec<String> {
        let mut ids = widget
            .test_id()
            .map(str::to_owned)
            .into_iter()
            .collect::<Vec<_>>();
        for child in widget.children() {
            ids.extend(ids_in_order(child));
        }
        ids
    }

    /// The words the tree draws, its accessibility names included.
    fn drawn(widget: &dyn Widget) -> Vec<String> {
        let mut found = Vec::new();
        let mut stack: Vec<&dyn Widget> = vec![widget];
        while let Some(widget) = stack.pop() {
            if let Some(node) = widget.accessibility() {
                found.extend(node.name);
                found.extend(node.description);
                found.extend(node.value);
            }
            stack.extend(widget.children());
        }
        found
    }

    fn drawn_sections(sections: &[Box<dyn Widget>]) -> Vec<String> {
        sections
            .iter()
            .flat_map(|widget| drawn(widget.as_ref()))
            .collect()
    }

    tilo_test_env::scenario! {
        fn the_report_draws_what_the_probe_found_and_never_more;

        phase sections_expand_limitations_and_keep_working_rows_compact;
        {
            let sections = sections(&fixture_support_report(), Placement::ThisComputer);
            for id in [
                "setup.hardware.model",
                "setup.hardware.summary",
                "setup.hardware.fingerprint",
                "setup.hardware.wifi",
                "setup.hardware.camera",
            ] {
                assert!(any_has_id(&sections, id), "{id} is drawn");
            }
            assert!(
                any_has_id(&sections, "setup.hardware.fingerprint.evidence.0"),
                "what the limitation rests on is drawn"
            );
            assert!(
                any_has_id(&sections, "setup.hardware.fingerprint.remedy"),
                "what to do about it is drawn"
            );
            assert!(
                !any_has_id(&sections, "setup.hardware.wifi.evidence.0"),
                "a working component stays compact"
            );
        }

        phase another_computer_sections_expand_limitations_and_keep_working_rows_compact;
        {
            let sections = sections(&fixture_support_report(), Placement::AnotherComputer);
            assert!(any_has_id(&sections, "setup.hardware.model"));
            assert!(any_has_id(&sections, "setup.hardware.fingerprint.evidence.0"));
            assert!(any_has_id(&sections, "setup.hardware.fingerprint.remedy"));
            assert!(any_has_id(&sections, "setup.hardware.wifi"));
            assert!(!any_has_id(&sections, "setup.hardware.wifi.evidence.0"));
        }

        phase the_live_medium_reads_the_computer_it_installs_on;
        {
            let sections = sections(&fixture_support_report(), Placement::LiveMedium);
            let text = drawn_sections(&sections);
            assert!(
                text.iter()
                    .any(|line| line.contains(&t("installer-hardware-this-computer"))),
                "{text:?}"
            );
            assert!(
                text.iter()
                    .any(|line| line.contains(&t("installer-hardware-source-this-computer"))),
                "{text:?}"
            );
            assert!(any_has_id(&sections, "setup.hardware.model"));
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
            assert!(any_has_id(&sections, "setup.hardware.summary"));
            assert!(!any_has_id(&sections, "setup.hardware.graphics"));
            assert!(!any_has_id(&sections, "setup.hardware.wifi"));
        }

        phase a_component_the_probe_observed_is_never_reported_as_supported;
        {
            let report = report(vec![observed_graphics()]);
            let sections = sections(&report, Placement::ThisComputer);
            let text = drawn_sections(&sections);
            assert!(
                text.iter()
                    .any(|line| line.contains(&t("installer-hardware-summary-observation"))),
                "{text:?}"
            );
            let facts = report.components.iter().map(component_facts).collect::<Vec<_>>();
            assert_eq!(facts[0]["state"], "unverified");
            assert_eq!(facts[0]["applicability"], "observation-only");
        }

        phase every_component_the_probe_covered_gets_a_row_with_its_reason;
        {
            let report = report(vec![
                component(
                    ComponentKind::Fingerprint,
                    SupportState::Limited,
                    SupportReason::FingerprintUnverified {
                        device: "Goodix reader".into(),
                    },
                    Some(Remedy::PasswordSignIn),
                ),
                observed_graphics(),
                component(
                    ComponentKind::Wifi,
                    SupportState::Unsupported,
                    SupportReason::DriverMissing {
                        devices: vec!["wlp1s0".into()],
                    },
                    Some(Remedy::Connect),
                ),
            ]);
            let sections = sections(&report, Placement::ThisComputer);
            let text = drawn_sections(&sections);
            for component in &report.components {
                let named = format!("{}, {}", component_title(component.kind), reason_text(component));
                assert!(text.contains(&named), "{named} in {text:?}");
            }
            assert!(text.iter().any(|line| line.contains("amdgpu")));
            assert!(text.iter().any(|line| line.contains("Goodix reader")));
            assert!(text.contains(&t("installer-hardware-remedy-password")));
            assert!(text.contains(&t("installer-hardware-remedy-connect")));
        }

        phase essential_components_are_read_before_optional_ones;
        {
            let report = report(vec![
                component(
                    ComponentKind::Fingerprint,
                    SupportState::Unverified,
                    SupportReason::FingerprintUnverified {
                        device: "reader".into(),
                    },
                    None,
                ),
                observed_graphics(),
                component(
                    ComponentKind::Boot,
                    SupportState::Unverified,
                    SupportReason::LegacyBoot,
                    Some(Remedy::UefiBoot),
                ),
            ]);
            let mut ids = Vec::new();
            for section in sections(&report, Placement::ThisComputer) {
                ids.extend(ids_in_order(section.as_ref()));
            }
            let at = |id: &str| {
                ids.iter()
                    .position(|drawn| drawn == id)
                    .unwrap_or_else(|| panic!("{id} is drawn: {ids:?}"))
            };
            assert!(
                at("setup.hardware.boot") < at("setup.hardware.graphics"),
                "{ids:?}"
            );
            assert!(
                at("setup.hardware.boot") < at("setup.hardware.fingerprint"),
                "{ids:?}"
            );
        }

        phase a_component_with_no_next_step_still_says_the_release_was_not_checked;
        {
            let report = report(vec![component(
                ComponentKind::Ethernet,
                SupportState::Unverified,
                SupportReason::DriverBound {
                    bindings: vec![binding("enp0s31f6", Some("e1000e"))],
                },
                None,
            )]);
            let sections = sections(&report, Placement::ThisComputer);
            let text = drawn_sections(&sections);
            assert!(
                text.iter()
                    .all(|line| !line.contains(&t("installer-hardware-remedy-check-driver")))
            );
            assert!(any_has_id(&sections, "setup.hardware.ethernet.scope"));
            assert!(text.contains(&t("installer-hardware-scope-observation")));
        }

        phase a_component_needing_attention_says_what_it_rests_on;
        {
            let report = report(vec![component(
                ComponentKind::Camera,
                SupportState::Limited,
                SupportReason::PlatformPipeline {
                    device: "ipu6".into(),
                },
                Some(Remedy::ExternalCamera),
            )]);
            let sections = sections(&report, Placement::ThisComputer);
            assert!(any_has_id(&sections, "setup.hardware.camera.scope"));
            assert!(any_has_id(&sections, "setup.hardware.camera.remedy"));
            let text = drawn_sections(&sections);
            assert!(text.contains(&t("installer-hardware-scope-observation")));
            assert!(text.contains(&t("installer-hardware-remedy-external-camera")));
        }

        phase a_missing_essential_component_says_the_install_cannot_continue;
        {
            let report = report(vec![component(
                ComponentKind::Storage,
                SupportState::Unsupported,
                SupportReason::NotDetected,
                None,
            )]);
            assert!(report.blocks_install());
            let sections = sections(&report, Placement::ThisComputer);
            let summary = t_args(
                "installer-hardware-summary-blocking",
                &[
                    ("supported", SupportSummary::default().supported.into()),
                    ("limited", 0.into()),
                    ("unsupported", 1.into()),
                    ("unverified", 0.into()),
                ],
            );
            let text = drawn_sections(&sections);
            assert!(
                text.iter().any(|line| line.starts_with(&summary)),
                "{summary} in {text:?}"
            );
        }

        phase a_caller_without_a_screen_reads_the_same_facts_as_the_rows_draw;
        {
            let report = report(vec![component(
                ComponentKind::Fingerprint,
                SupportState::Limited,
                SupportReason::FingerprintUnverified {
                    device: "Goodix reader".into(),
                },
                Some(Remedy::PasswordSignIn),
            )]);
            let facts = facts(Some(&report));
            assert_eq!(facts["state"], "ready");
            assert_eq!(facts["source"], "local-scan");
            assert_eq!(facts["blocks_install"], false);
            let component = &facts["components"][0];
            assert_eq!(component["component"], "fingerprint");
            assert_eq!(component["state"], "limited");
            assert_eq!(component["applicability"], "observation-only");
            assert!(
                component["reason"]
                    .as_str()
                    .is_some_and(|reason| reason.contains("Goodix reader"))
            );
            assert_eq!(
                component["next"],
                serde_json::Value::String(t("installer-hardware-remedy-password"))
            );
        }

        phase a_report_that_could_not_identify_this_computer_says_so_instead_of_guessing;
        {
            let facts = facts(None);
            assert_eq!(facts["state"], "unavailable");
            assert!(facts.get("components").is_none());
            let rows = drawn(unavailable().as_ref());
            assert!(rows.contains(&t("installer-hardware-unavailable")));
            assert!(checked(report(Vec::new())).is_none());
        }

        phase a_report_checked_against_the_release_names_that_apart_from_an_observation;
        {
            let mut verified = observed_graphics();
            verified.state = SupportState::Supported;
            verified.applicability = Applicability::ReleaseVerified;
            verified.evidence = vec![Evidence {
                scope: EvidenceScope::SelectedRelease,
                fact: EvidenceFact::Kernel {
                    version: "6.17.0".into(),
                },
            }];
            assert!(verified.blocks_install().eq(&false));
            assert_eq!(component_facts(&verified)["applicability"], "release-verified");
        }
    }
}
