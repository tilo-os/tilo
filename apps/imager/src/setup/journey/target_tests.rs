//! Destination compatibility for another computer, through the production
//! preview effects rather than a standalone parser.

use super::{Destination, InstallerPreview, InstallerPreviewEffect, Page};
use imager_backend::support_report::{
    Applicability, ComponentKind, ComponentSupport, DestinationIdentity, DeviceBinding, Evidence,
    EvidenceFact, EvidenceScope, REPORT_SCHEMA, Remedy, ReportSource, SupportReason, SupportReport,
    SupportState,
};

fn preview() -> InstallerPreview {
    let mut preview = InstallerPreview {
        live: true,
        linux: true,
        ..Default::default()
    };
    preview.capabilities.prepared_installation = true;
    preview.journey.destination = Some(Destination::Another);
    preview
}

fn component(
    kind: ComponentKind,
    name: &str,
    state: SupportState,
    reason: SupportReason,
    evidence: Vec<Evidence>,
    remedy: Option<Remedy>,
) -> ComponentSupport {
    ComponentSupport {
        kind,
        name: name.into(),
        present: true,
        state,
        applicability: if state == SupportState::Supported {
            Applicability::ReleaseVerified
        } else {
            Applicability::ObservationOnly
        },
        reason,
        evidence,
        remedy,
    }
}

fn evidence(name: &str, driver: &str) -> Evidence {
    Evidence {
        scope: EvidenceScope::Observation,
        fact: EvidenceFact::Device {
            name: name.into(),
            driver: Some(driver.into()),
            firmware: None,
            vendor: None,
            product: None,
            reported_problem: false,
        },
    }
}

fn report(architecture: Option<&str>, components: Vec<ComponentSupport>) -> SupportReport {
    SupportReport {
        schema: REPORT_SCHEMA.into(),
        identity: DestinationIdentity {
            source: ReportSource::LocalScan,
            manufacturer: Some("Prepared Vendor".into()),
            model: Some("Prepared Laptop 14".into()),
            variant: Some("PREPARED-BOARD".into()),
            board: Some("PREPARED-BOARD".into()),
            sku: None,
            firmware: None,
        },
        kernel: Some("7.2.7-test".into()),
        architecture: architecture.map(str::to_string),
        components,
    }
}

fn supported_graphics() -> ComponentSupport {
    component(
        ComponentKind::Graphics,
        "Test graphics",
        SupportState::Supported,
        SupportReason::DriverBound {
            bindings: vec![DeviceBinding {
                device: "Test graphics".into(),
                driver: Some("test".into()),
            }],
        },
        vec![Evidence {
            scope: EvidenceScope::SelectedRelease,
            ..evidence("Test graphics", "test")
        }],
        None,
    )
}

#[test]
fn another_destination_checks_compatibility_before_identity_and_media() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(preview.journey.page, Page::Target);
    assert!(preview.journey.identity.is_none());
    assert!(preview.journey.password.is_empty());
    assert!(preview.storage.facts.is_none());
    assert!(!preview.compatibility.pending);
    assert!(!preview.direct.active);

    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("variant", "PREPARED-BOARD"),
    ] {
        preview
            .apply(InstallerPreviewEffect::SetTargetIdentityField {
                field: field.into(),
                value: value.into(),
            })
            .unwrap();
    }
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(preview.journey.page, Page::Migration);
    assert_eq!(
        preview.journey.history,
        vec![Page::Destination, Page::Target]
    );
    assert!(preview.journey.identity.is_none());
    assert!(preview.journey.password.is_empty());
}

#[test]
fn model_only_never_authorizes_boot_partition_or_protection() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::SetTargetIdentityField {
            field: "manufacturer".into(),
            value: "Prepared Vendor".into(),
        })
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::SetTargetIdentityField {
            field: "model".into(),
            value: "Prepared Laptop 14".into(),
        })
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::SetTargetIdentityField {
            field: "variant".into(),
            value: "PREPARED-BOARD".into(),
        })
        .unwrap();
    let projection = preview.target.projection();
    assert_eq!(projection["ready"], true);
    assert_eq!(projection["source"], "declared-model");
    assert_eq!(projection["destination_check_required"], true);
    assert_eq!(projection["blocks_preparation"], false);
    assert_eq!(projection["report"]["components"], serde_json::json!([]));
    assert!(preview.storage.facts.is_none());

    // The medium cannot be prepared for a destination the journey has not
    // identified, and a model is not a grant for anything on that computer.
    preview.target.clear_report();
    for field in ["manufacturer", "model", "variant"] {
        preview
            .apply(InstallerPreviewEffect::SetTargetIdentityField {
                field: field.into(),
                value: String::new(),
            })
            .unwrap();
    }
    preview.journey.page = Page::Review;
    assert_eq!(
        preview.apply(InstallerPreviewEffect::PrepareUsb),
        Err("installer-preview-target-incomplete")
    );
    assert!(!preview.direct.active);
    assert!(preview.storage.facts.is_none());
}

#[test]
fn imported_report_must_name_a_supported_architecture_and_model() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    let host = std::env::consts::ARCH;
    let other = if host == "aarch64" {
        "x86_64"
    } else {
        "aarch64"
    };
    let bytes = report(Some(host), vec![supported_graphics()])
        .to_json()
        .unwrap();
    preview
        .target
        .import_bytes(Some("prepared.json".into()), &bytes)
        .unwrap();
    let projection = preview.target.projection();
    assert_eq!(projection["source"], "imported");
    assert_eq!(projection["architecture"], host);
    assert_eq!(projection["ready"], true);
    assert_eq!(projection["destination_check_required"], true);

    let missing = report(None, vec![supported_graphics()]).to_json().unwrap();
    assert_eq!(
        preview
            .target
            .import_bytes(Some("old.json".into()), &missing),
        Err("installer-preview-target-report-missing-architecture")
    );
    assert_eq!(
        preview.target.projection()["architecture"],
        serde_json::json!(host),
        "a rejected file leaves the accepted report in place"
    );

    // Another architecture is a target, not an error: the verified catalog
    // selects that payload when the medium is prepared.
    let other_architecture = report(Some(other), vec![supported_graphics()])
        .to_json()
        .unwrap();
    preview
        .target
        .import_bytes(Some("elsewhere.json".into()), &other_architecture)
        .unwrap();
    let projection = preview.target.projection();
    assert_eq!(projection["architecture"], other);
    assert_eq!(projection["ready"], true);
    assert_eq!(projection["destination_check_required"], true);
    assert_eq!(
        preview.apply(InstallerPreviewEffect::SetTargetArchitecture {
            architecture: other.into(),
        }),
        Ok(())
    );
    assert_eq!(
        preview.apply(InstallerPreviewEffect::SetTargetArchitecture {
            architecture: host.into(),
        }),
        Err("installer-preview-target-report-architecture-fixed")
    );

    let unsupported = report(Some("riscv64"), vec![supported_graphics()])
        .to_json()
        .unwrap();
    assert_eq!(
        preview
            .target
            .import_bytes(Some("other.json".into()), &unsupported),
        Err("installer-preview-target-report-unsupported-architecture")
    );
}

#[test]
fn an_unsupported_essential_component_blocks_preparation_before_identity() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    let bytes = report(
        Some(std::env::consts::ARCH),
        vec![component(
            ComponentKind::Graphics,
            "Test graphics",
            SupportState::Unsupported,
            SupportReason::DriverMissing {
                devices: vec!["Test graphics".into()],
            },
            vec![Evidence {
                scope: EvidenceScope::Observation,
                fact: EvidenceFact::Device {
                    name: "Test graphics".into(),
                    driver: None,
                    firmware: None,
                    vendor: None,
                    product: None,
                    reported_problem: false,
                },
            }],
            Some(Remedy::CheckDriver),
        )],
    )
    .to_json()
    .unwrap();
    preview
        .target
        .import_bytes(Some("blocked.json".into()), &bytes)
        .unwrap();
    assert_eq!(preview.target.projection()["blocks_preparation"], true);
    assert_eq!(
        preview.apply(InstallerPreviewEffect::ContinuePreview),
        Err("installer-preview-target-blocking")
    );
    assert_eq!(preview.journey.page, Page::Target);
    assert!(preview.journey.identity.is_none());
}

#[test]
fn recheck_stays_on_the_target_and_never_probes_this_computer() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::SetTargetIdentityField {
            field: "manufacturer".into(),
            value: "Prepared Vendor".into(),
        })
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::SetTargetIdentityField {
            field: "model".into(),
            value: "Prepared Laptop 14".into(),
        })
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::SetTargetIdentityField {
            field: "variant".into(),
            value: "PREPARED-BOARD".into(),
        })
        .unwrap();
    preview
        .apply(InstallerPreviewEffect::RecheckTargetHardware)
        .unwrap();
    assert_eq!(preview.journey.page, Page::Target);
    assert!(!preview.compatibility.pending);
    assert!(preview.storage.facts.is_none());
    assert!(preview.target.ready());
    assert!(preview.target.projection()["error"].is_null());
}

#[test]
fn an_incomplete_target_refuses_continue_and_reveals_the_first_field() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(
        preview.apply(InstallerPreviewEffect::ContinuePreview),
        Err("installer-preview-target-incomplete")
    );
    assert_eq!(preview.journey.page, Page::Target);
    assert!(preview.journey.identity.is_none());
    assert_eq!(
        preview.target.first_invalid_focus().as_deref(),
        Some("installer.target.manufacturer")
    );
}

#[test]
fn going_back_from_the_target_keeps_the_destination_and_what_was_entered() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("variant", "PREPARED-BOARD"),
    ] {
        preview
            .apply(InstallerPreviewEffect::SetTargetIdentityField {
                field: field.into(),
                value: value.into(),
            })
            .unwrap();
    }
    preview
        .apply(InstallerPreviewEffect::BackInPreview)
        .unwrap();
    assert_eq!(preview.journey.page, Page::Destination);
    assert_eq!(preview.journey.destination, Some(Destination::Another));
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    assert_eq!(preview.journey.page, Page::Target);
    assert_eq!(
        preview.target.projection()["report"]["manufacturer"],
        serde_json::json!("Prepared Vendor")
    );
}

#[test]
fn a_board_or_sku_distinguishes_the_model_without_a_variant_name() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("board", "PREPARED-BOARD"),
    ] {
        preview
            .apply(InstallerPreviewEffect::SetTargetIdentityField {
                field: field.into(),
                value: value.into(),
            })
            .unwrap();
    }
    assert!(preview.target.ready());
    assert_eq!(preview.target.first_invalid_focus(), None);
    assert_eq!(
        preview.target.projection()["report"]["board"],
        serde_json::json!("PREPARED-BOARD")
    );
}

#[test]
fn a_medium_for_another_architecture_never_falls_back_to_this_ones_payload() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    let host = std::env::consts::ARCH;
    let other = if host == "aarch64" {
        "x86_64"
    } else {
        "aarch64"
    };
    let bytes = report(Some(other), vec![supported_graphics()])
        .to_json()
        .unwrap();
    preview
        .target
        .import_bytes(Some("elsewhere.json".into()), &bytes)
        .unwrap();
    assert_eq!(preview.target.architecture(), other);
    // The catalog publishes no payload for that architecture, so a
    // cross-architecture medium is refused here instead of quietly writing
    // this computer's image. The empty test catalog also keeps this test
    // deterministic while another test holds the environment lock.
    let catalog = tempfile::tempdir().unwrap();
    let path = catalog.path().join("catalog.json");
    std::fs::write(&path, r#"{"schema":1,"tag":"unreleased","releases":[]}"#).unwrap();
    let mut environment = tilo_test_env::ScopedEnv::new();
    environment.set("TILO_STAGING_TEST_CATALOG", &path);
    assert!(
        crate::release::bundled_releases_for_arch(other)
            .unwrap()
            .is_empty()
    );
    assert!(
        crate::release::bundled_internal_releases_for_arch(other)
            .unwrap()
            .is_empty()
    );
    preview.journey.page = Page::Review;
    assert_eq!(
        preview.apply(InstallerPreviewEffect::PrepareUsb),
        Err("installer-preview-target-payload-missing")
    );
}

#[test]
fn a_saved_target_choice_restores_the_same_model_and_report_without_a_path() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("variant", "PREPARED-BOARD"),
    ] {
        preview
            .apply(InstallerPreviewEffect::SetTargetIdentityField {
                field: field.into(),
                value: value.into(),
            })
            .unwrap();
    }
    let host = std::env::consts::ARCH;
    let other = if host == "aarch64" {
        "x86_64"
    } else {
        "aarch64"
    };
    let bytes = report(Some(other), vec![supported_graphics()])
        .to_json()
        .unwrap();
    preview
        .target
        .import_bytes(Some("private-destination.json".into()), &bytes)
        .unwrap();
    let draft = preview.target.draft_choices();
    let encoded = serde_json::to_vec(&draft).unwrap();
    // The private restart record carries the selection and the evidence, not
    // the source path or file name the person picked.
    assert!(!String::from_utf8_lossy(&encoded).contains("private-destination"));
    assert!(!String::from_utf8_lossy(&encoded).contains("Prepared Setup"));
    let restored_draft: super::draft::TargetDraft = serde_json::from_slice(&encoded).unwrap();
    let mut restored = super::target::Target::default();
    restored.restore(Some(&restored_draft));
    assert_eq!(restored.architecture(), other);
    let projection = restored.projection();
    assert_eq!(projection["source"], "imported");
    assert_eq!(projection["ready"], true);
    assert_eq!(projection["architecture"], other);
    assert_eq!(
        projection["report"]["model"],
        serde_json::json!("Prepared Laptop 14")
    );
    assert_eq!(projection["destination_check_required"], true);
    assert!(
        !serde_json::to_string(&projection)
            .unwrap()
            .contains("private-destination"),
        "the public status carries no file hint"
    );
}

#[test]
fn a_declared_model_keeps_the_selected_architecture_across_a_restart() {
    let mut preview = preview();
    preview
        .apply(InstallerPreviewEffect::ContinuePreview)
        .unwrap();
    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("board", "PREPARED-BOARD"),
    ] {
        preview
            .apply(InstallerPreviewEffect::SetTargetIdentityField {
                field: field.into(),
                value: value.into(),
            })
            .unwrap();
    }
    let host = std::env::consts::ARCH;
    let other = if host == "aarch64" {
        "x86_64"
    } else {
        "aarch64"
    };
    preview
        .apply(InstallerPreviewEffect::SetTargetArchitecture {
            architecture: other.into(),
        })
        .unwrap();
    let draft = preview.target.draft_choices();
    let restored_draft: super::draft::TargetDraft =
        serde_json::from_slice(&serde_json::to_vec(&draft).unwrap()).unwrap();
    let mut restored = super::target::Target::default();
    restored.restore(Some(&restored_draft));
    assert_eq!(restored.architecture(), other);
    let projection = restored.projection();
    assert_eq!(projection["source"], "declared-model");
    assert_eq!(projection["ready"], true);
    assert_eq!(projection["report"]["components"], serde_json::json!([]));
}

#[cfg(unix)]
#[test]
fn production_surface_paints_the_target_page_and_routes_its_semantic_effects() {
    use crate::resource::GuidedInstallerShell;
    use ui::framework::testing::TestApp;

    let mut app = TestApp::<GuidedInstallerShell>::launched(&["--installer"]).unwrap();
    app.settle();
    app.with_widget::<InstallerPreview, _>(|preview| {
        preview.live = true;
        preview.capabilities.prepared_installation = true;
        preview.restart = super::restart_controller::Restart::default();
        preview.journey.destination = Some(Destination::Another);
        preview.journey.page = Page::Target;
        preview.dirty = true;
    })
    .unwrap();
    app.settle();
    for (field, value) in [
        ("manufacturer", "Prepared Vendor"),
        ("model", "Prepared Laptop 14"),
        ("variant", "PREPARED-BOARD"),
    ] {
        app.effect(&InstallerPreviewEffect::SetTargetIdentityField {
            field: field.into(),
            value: value.into(),
        })
        .unwrap();
    }
    app.settle();
    let inspection = app.harness().inspection_json().unwrap();
    for id in [
        "installer.target.manufacturer",
        "installer.target.model",
        "installer.target.variant",
        "installer.target.import",
        "installer.target.recheck",
    ] {
        assert!(inspection.contains(id), "missing {id}");
    }
    let status = app
        .effect(&InstallerPreviewEffect::ReadPreview)
        .unwrap()
        .value
        .unwrap();
    assert_eq!(status["page"], "target");
    assert_eq!(status["target"]["source"], "declared-model");
    assert_eq!(status["target"]["destination_check_required"], true);
}
