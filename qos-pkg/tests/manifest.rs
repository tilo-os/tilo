//! Manifest-level tests: TOML round-trip, schema validation,
//! service-req parsing.

mod fixtures;

use qos_pkg::manifest::{Manifest, Requirement, ServiceReq, ServiceReqParseError};
use qos_pkg::validate::{ManifestError, validate_manifest};

fn render_with_dummy_key() -> String {
    fixtures::render_template(
        fixtures::SAMPLE_APP_MANIFEST_TEMPLATE,
        &"aa".repeat(32),
    )
}

#[test]
fn manifest_roundtrip() {
    let src = render_with_dummy_key();
    let m = Manifest::from_toml_str(&src).expect("decode");
    let reencoded = m.to_toml_string().expect("encode");
    let again = Manifest::from_toml_str(&reencoded).expect("decode-2");
    assert_eq!(m, again, "round-trip preserves manifest");

    // Spot-check a few fields that are easy to drop in a custom
    // de/encode path.
    assert_eq!(m.package.id, "dev.qos.editor");
    assert_eq!(m.package.version, "1.4.2");
    assert_eq!(
        m.entrypoint
            .as_ref()
            .unwrap()
            .splash_segments
            .first()
            .unwrap()
            .byte_range,
        Some((0, 0x300000))
    );
    let svc = m.sandbox.services.get("collab").unwrap();
    assert_eq!(svc.requirement, Requirement::Required);
    assert_eq!(svc.version, 1);
    assert_eq!(svc.protocol, "collab");
}

#[test]
fn app_without_entrypoint_fails_validation() {
    let mut m = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
    m.entrypoint = None;
    assert!(matches!(
        validate_manifest(&m),
        Err(ManifestError::AppMissingEntrypoint)
    ));
}

#[test]
fn format_provider_without_mime_fails_validation() {
    let mut m = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
    m.package.kind = qos_pkg::Kind::FormatProvider;
    m.providers.mime.clear();
    // entrypoint is irrelevant for FormatProvider, but the
    // manifest still has one from the sample template; that's
    // fine — validate_manifest's FormatProvider branch only
    // checks `providers.mime`.
    assert!(matches!(
        validate_manifest(&m),
        Err(ManifestError::FormatProviderMissingMime)
    ));
}

#[test]
fn service_req_parse_required_at_1() {
    let r = ServiceReq::parse("collab", "required@1").unwrap();
    assert_eq!(r.requirement, Requirement::Required);
    assert_eq!(r.version, 1);
    assert_eq!(r.protocol, "collab");
}

#[test]
fn service_req_parse_optional_at_2() {
    let r = ServiceReq::parse("spellcheck", "optional@2").unwrap();
    assert_eq!(r.requirement, Requirement::Optional);
    assert_eq!(r.version, 2);
}

#[test]
fn service_req_parse_required_defaults_to_v1() {
    let r = ServiceReq::parse("collab", "required").unwrap();
    assert_eq!(r.requirement, Requirement::Required);
    assert_eq!(r.version, 1);
}

#[test]
fn service_req_parse_rejects_garbage() {
    let e = ServiceReq::parse("collab", "Garbage").unwrap_err();
    assert!(
        matches!(e, ServiceReqParseError::BadSeverity { .. }),
        "got {e:?}"
    );
    let e2 = ServiceReq::parse("collab", "required@abc").unwrap_err();
    assert!(
        matches!(e2, ServiceReqParseError::BadVersion { .. }),
        "got {e2:?}"
    );
}
