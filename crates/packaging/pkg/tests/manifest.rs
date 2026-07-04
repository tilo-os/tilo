//! Manifest-level tests: TOML round-trip, schema validation,
//! service-req parsing.

mod fixtures;

use pkg::manifest::{Manifest, PortalPolicy, Requirement, ServiceReq, ServiceReqParseError};
use pkg::validate::{ManifestError, validate_manifest};

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
    m.package.kind = pkg::Kind::FormatProvider;
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
fn portal_policy_parses_via_host_and_deny() {
    let src = render_with_dummy_key();
    // The sample template only declares `file-chooser = "via-host"`.
    // Extend it here to also cover `deny`, then round-trip.
    let extended = src.replace(
        "[sandbox.portals]\nfile-chooser = \"via-host\"",
        "[sandbox.portals]\nfile-chooser = \"via-host\"\nnotifications = \"deny\"",
    );
    let m = Manifest::from_toml_str(&extended).expect("decode");
    assert_eq!(
        m.sandbox.portals.get("file-chooser").copied(),
        Some(PortalPolicy::ViaHost)
    );
    assert_eq!(
        m.sandbox.portals.get("notifications").copied(),
        Some(PortalPolicy::Deny)
    );

    // Round-trip preserves the enum.
    let reencoded = m.to_toml_string().expect("encode");
    let again = Manifest::from_toml_str(&reencoded).expect("decode-2");
    assert_eq!(m.sandbox.portals, again.sandbox.portals);
}

#[test]
fn portal_policy_rejects_typo() {
    let src = render_with_dummy_key();
    let bad = src.replace("\"via-host\"", "\"via-hst\"");
    // A misspelling now surfaces at manifest-decode time (rather
    // than silently being accepted as an opaque string).
    assert!(Manifest::from_toml_str(&bad).is_err());
}

#[test]
fn cli_block_roundtrips() {
    let src = fixtures::render_template(
        fixtures::CLI_APP_MANIFEST_TEMPLATE,
        &"aa".repeat(32),
    );
    let m = Manifest::from_toml_str(&src).expect("decode");
    let ep = m.entrypoint.as_ref().expect("entrypoint");
    assert_eq!(ep.r#type, pkg::EntrypointType::Cli);
    let cli = m.cli.as_ref().expect("[cli] block present");
    assert_eq!(cli.commands, vec!["notes".to_string()]);
    // round-trip preserves both.
    let re = m.to_toml_string().expect("encode");
    let m2 = Manifest::from_toml_str(&re).expect("decode-2");
    assert_eq!(m, m2, "cli manifest round-trips");
}

#[test]
fn entrypoint_type_both_prefers_split_bins() {
    let src = fixtures::render_template(
        fixtures::BOTH_APP_MANIFEST_TEMPLATE,
        &"aa".repeat(32),
    );
    let m = Manifest::from_toml_str(&src).expect("decode");
    let ep = m.entrypoint.as_ref().expect("entrypoint");
    assert_eq!(ep.r#type, pkg::EntrypointType::Both);
    assert_eq!(
        ep.gui_bin_path().map(|p| p.to_string_lossy().into_owned()),
        Some("bin/notes-gui".to_string())
    );
    assert_eq!(
        ep.cli_bin_path().map(|p| p.to_string_lossy().into_owned()),
        Some("bin/notes".to_string())
    );
    // Absent `[cli]` even on `type = both` is allowed — the CLI
    // just publishes zero PATH commands.
    let cli = m.cli.as_ref().expect("[cli] present in template");
    assert_eq!(cli.commands, vec!["notes".to_string()]);
    let re = m.to_toml_string().expect("encode");
    let m2 = Manifest::from_toml_str(&re).expect("decode-2");
    assert_eq!(m, m2, "both-flavour manifest round-trips");
}

#[test]
fn entrypoint_type_defaults_to_gui() {
    // The original SAMPLE_APP_MANIFEST_TEMPLATE has no `type` — it
    // should decode as Gui and pass validation.
    let src = fixtures::render_template(
        fixtures::SAMPLE_APP_MANIFEST_TEMPLATE,
        &"aa".repeat(32),
    );
    let m = Manifest::from_toml_str(&src).expect("decode");
    let ep = m.entrypoint.as_ref().expect("entrypoint");
    assert_eq!(ep.r#type, pkg::EntrypointType::Gui);
    assert!(m.cli.is_none());
    validate_manifest(&m).expect("gui manifest still validates");
}

#[test]
fn cli_manifest_validates() {
    let src = fixtures::render_template(
        fixtures::CLI_APP_MANIFEST_TEMPLATE,
        &"aa".repeat(32),
    );
    let m = Manifest::from_toml_str(&src).expect("decode");
    validate_manifest(&m).expect("cli manifest validates");
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
