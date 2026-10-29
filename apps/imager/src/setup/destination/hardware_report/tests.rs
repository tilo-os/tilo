//! The report says what the probe found and never more. These tests hold the
//! two properties a person relies on: an observation is never called
//! supported, and every component they might notice is named with a reason
//! and a next step.

use super::*;
use imager_backend::support_report::{DestinationIdentity, REPORT_SCHEMA, SupportSummary};

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

fn report(components: Vec<ComponentSupport>) -> SupportReport {
    SupportReport {
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

fn drawn(widget: &dyn Widget) -> Vec<String> {
    let mut found = Vec::new();
    let mut stack: Vec<&dyn Widget> = vec![widget];
    while let Some(widget) = stack.pop() {
        if let Some(node) = widget.accessibility() {
            // A row says its class in `name` and its reason in `value`; both
            // reach a screen reader, so the test reads both.
            found.extend(node.name);
            found.extend(node.description);
            found.extend(node.value);
        }
        stack.extend(widget.children());
    }
    found
}

#[test]
fn a_component_the_probe_observed_is_never_reported_as_supported() {
    let report = report(vec![observed_graphics()]);
    let summary = summary_text(&report);
    // The count of supported components is zero, and the sentence that says
    // what an observation can establish follows the counts.
    assert!(summary.contains(&t("installer-hardware-summary-observation")));
    let facts = report
        .components
        .iter()
        .map(component_facts)
        .collect::<Vec<_>>();
    assert_eq!(facts[0]["state"], "unverified");
    assert_eq!(facts[0]["applicability"], "observation-only");
}

#[test]
fn every_component_the_probe_covered_gets_a_row_with_its_reason() {
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
    let rows = component_rows(&report);
    assert_eq!(rows.len(), 3, "every component class is a row of its own");
    let text = rows
        .iter()
        .flat_map(|row| drawn(row.as_ref()))
        .collect::<Vec<_>>();
    // The class is named, so a person can find the part that will not work.
    for kind in [
        ComponentKind::Graphics,
        ComponentKind::Fingerprint,
        ComponentKind::Wifi,
    ] {
        assert!(
            text.contains(&component_title(kind)),
            "{} is named",
            kind.key()
        );
    }
    // The observation's own evidence reaches the row: the driver bound here
    // and the reader's own name.
    assert!(text.iter().any(|line| line.contains("amdgpu")));
    assert!(text.iter().any(|line| line.contains("Goodix reader")));
    // A limitation says what the person can do instead of failing silently.
    assert!(text.contains(&t("installer-hardware-remedy-password")));
    assert!(text.contains(&t("installer-hardware-remedy-connect")));
}

#[test]
fn essential_components_are_read_before_optional_ones() {
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
    let rows = component_rows(&report);
    let first = drawn(rows[0].as_ref());
    assert!(first.contains(&component_title(ComponentKind::Boot)));
}

#[test]
fn a_component_with_no_next_step_still_says_the_release_was_not_checked() {
    let report = report(vec![component(
        ComponentKind::Ethernet,
        SupportState::Unverified,
        SupportReason::DriverBound {
            bindings: vec![binding("enp0s31f6", Some("e1000e"))],
        },
        None,
    )]);
    let rows = component_rows(&report);
    assert_eq!(rows.len(), 1, "one component is one row");
    let text = drawn(rows[0].as_ref());
    // Nothing to do, so no next step is invented.
    assert!(
        text.iter()
            .all(|line| !line.contains(&t("installer-hardware-remedy-check-driver")))
    );
    // But the scope is always stated: this driver was seen here, it was not
    // checked against the release, and a person must not read the row as
    // support.
    assert!(text.contains(&t("installer-hardware-scope-observation")));
}

#[test]
fn a_component_needing_attention_says_what_it_rests_on() {
    let report = report(vec![component(
        ComponentKind::Camera,
        SupportState::Limited,
        SupportReason::PlatformPipeline {
            device: "ipu6".into(),
        },
        Some(Remedy::ExternalCamera),
    )]);
    let rows = component_rows(&report);
    let text = drawn(rows[0].as_ref());
    assert!(text.contains(&t("installer-hardware-scope-observation")));
    assert!(text.contains(&t("installer-hardware-remedy-external-camera")));
}

#[test]
fn a_missing_essential_component_says_the_install_cannot_continue() {
    let report = report(vec![component(
        ComponentKind::Storage,
        SupportState::Unsupported,
        SupportReason::NotDetected,
        None,
    )]);
    assert!(report.blocks_install());
    let summary = summary_text(&report);
    assert!(summary.starts_with(&t_args(
        "installer-hardware-summary-blocking",
        &[
            ("supported", SupportSummary::default().supported.into()),
            ("limited", 0.into()),
            ("unsupported", 1.into()),
            ("unverified", 0.into()),
        ],
    )));
}

#[test]
fn a_caller_without_a_screen_reads_the_same_facts_as_the_rows_draw() {
    let report = report(vec![component(
        ComponentKind::Fingerprint,
        SupportState::Limited,
        SupportReason::FingerprintUnverified {
            device: "Goodix reader".into(),
        },
        Some(Remedy::PasswordSignIn),
    )]);
    let state = HardwareReport::Ready(Box::new(report));
    let facts = state.to_json();
    assert_eq!(facts["state"], "ready");
    assert_eq!(facts["source"], "local-scan");
    assert_eq!(facts["blocks_install"], false);
    let component = &facts["components"][0];
    assert_eq!(component["component"], "fingerprint");
    assert_eq!(component["state"], "limited");
    assert_eq!(component["applicability"], "observation-only");
    // The words the row shows are the words the caller reads: an agent and a
    // screen reader are answered by one implementation.
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

#[test]
fn a_report_that_could_not_identify_this_computer_says_so_instead_of_guessing() {
    let unavailable = HardwareReport::Unavailable;
    let facts = unavailable.to_json();
    assert_eq!(facts["state"], "unavailable");
    assert!(facts.get("components").is_none());
    let rows = drawn(summary_row(&unavailable).as_ref());
    assert!(rows.contains(&t("installer-hardware-unavailable")));
}

#[test]
fn a_report_checked_against_the_release_names_that_apart_from_an_observation() {
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
    assert_eq!(
        crate::setup::destination::hardware_report::component_facts(&verified)["applicability"],
        "release-verified"
    );
}
