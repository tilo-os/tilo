//! Manifest-level tests: TOML round-trip, schema validation,
//! service-req parsing.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "a test that unwraps is stating an invariant"
)]

use crate::fixtures;
use pkg::manifest::{Manifest, PortalPolicy, Requirement, ServiceReq, ServiceReqParseError};
use pkg::validate::{ManifestError, validate_manifest};

fn render_with_dummy_key() -> String {
    fixtures::render_template(fixtures::SAMPLE_APP_MANIFEST_TEMPLATE, &"aa".repeat(32))
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
fn provider_claims_normalize_and_roundtrip_canonically() {
    let src = render_with_dummy_key().replace(
        "ext  = [\"odt\"]",
        "ext = [\".ODT\", \" odt \"]\nschemes = [\"HTTPS:\", \"https\"]",
    );
    let manifest = Manifest::from_toml_str(&src).expect("decode claims");
    assert_eq!(manifest.providers.ext, vec!["odt"]);
    assert_eq!(manifest.providers.schemes, vec!["https"]);
    validate_manifest(&manifest).expect("normalized claims validate");

    let canonical = manifest.to_toml_string().expect("canonical TOML");
    assert!(canonical.contains("schemes = ["));
    assert!(!canonical.contains("uri-schemes"));
    assert_eq!(
        Manifest::from_toml_str(&canonical).unwrap().providers,
        manifest.providers
    );
}

#[test]
fn legacy_extension_only_provider_remains_valid() {
    let manifest = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
    assert!(manifest.providers.schemes.is_empty());
    validate_manifest(&manifest).expect("legacy manifest remains valid");
}

#[test]
fn transitional_uri_scheme_key_migrates_to_canonical_schemes() {
    let src = render_with_dummy_key().replace(
        "ext  = [\"odt\"]",
        "ext = [\"odt\"]\nuri-schemes = [\"WEBCAL:\"]",
    );
    let manifest = Manifest::from_toml_str(&src).unwrap();
    assert_eq!(manifest.providers.schemes, vec!["webcal"]);
    let encoded = manifest.to_toml_string().unwrap();
    assert!(encoded.contains("schemes = ["));
    assert!(!encoded.contains("uri-schemes"));
}

#[test]
fn malformed_and_active_content_claims_are_rejected() {
    let cases = [
        ("mime", "text/html; charset=utf-8"),
        ("mime", "*/json"),
        ("ext", "../../sh"),
        ("scheme", "javascript"),
        ("scheme", "file"),
        ("scheme", "https --flag"),
    ];
    for (kind, value) in cases {
        let mut manifest = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
        match kind {
            "mime" => manifest.providers.mime = vec![value.into()],
            "ext" => manifest.providers.ext = vec![value.into()],
            "scheme" => manifest.providers.schemes = vec![value.into()],
            _ => unreachable!(),
        }
        assert!(
            validate_manifest(&manifest).is_err(),
            "{kind} claim {value:?} must fail"
        );
    }
}

/// The install-time half of the filesystem policy.
///
/// `validate_manifest` is what `qos-install install` and `qos-pkg pack`
/// both call, so this is the check a package meets before it is ever
/// launched. Every shape the retired `[sandbox.fs]` block made possible
/// is here, including the two that a denylist would have missed: `~`,
/// which is the whole home directory spelled in one character, and
/// another package's state directory, which is a perfectly ordinary
/// path that happens to belong to someone else.
#[test]
fn a_manifest_that_names_a_filesystem_path_is_refused() {
    for entry in [
        "~",
        "/",
        "~/.ssh",
        "~/../../etc",
        "~/.local/share/dev.qos.totp",
        "$XDG_DATA_HOME/dev.qos.totp",
        "<scope=current>/../..",
    ] {
        let mut m = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
        m.sandbox.fs.write = vec![entry.to_string()];
        let err = validate_manifest(&m)
            .unwrap_err_or_panic(&format!("write = [{entry:?}] must be refused"));
        assert!(
            matches!(err, ManifestError::FsWriteDeclared { .. }),
            "write = [{entry:?}] produced {err:?}"
        );
        let text = err.to_string();
        assert!(
            text.contains("dev.qos.editor") && text.contains(entry),
            "the refusal must name the package and the entry: {text}"
        );
    }

    // A read is refused on the same terms, whether or not the system
    // profile happens to cover it — the field is gone, not filtered.
    for entry in ["/etc/shadow", "/var/workspace", "/usr/share/fonts"] {
        let mut m = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
        m.sandbox.fs.read = vec![entry.to_string()];
        let err = validate_manifest(&m)
            .unwrap_err_or_panic(&format!("read = [{entry:?}] must be refused"));
        assert!(
            matches!(err, ManifestError::FsReadDeclared { .. }),
            "read = [{entry:?}] produced {err:?}"
        );
        assert!(err.to_string().contains(entry));
    }

    // And the fixture is capable of validating: with no paths named, it
    // passes. Otherwise the refusals above would prove nothing.
    let clean = Manifest::from_toml_str(&render_with_dummy_key()).unwrap();
    assert!(clean.sandbox.fs.read.is_empty() && clean.sandbox.fs.write.is_empty());
    validate_manifest(&clean).expect("a manifest naming no path validates");
}

/// `Result::unwrap_err` needs `T: Debug`; `()` has it, but the message
/// on failure is then useless. This carries the case into the panic.
trait UnwrapErrOrPanic<E> {
    fn unwrap_err_or_panic(self, msg: &str) -> E;
}

impl<E> UnwrapErrOrPanic<E> for Result<(), E> {
    fn unwrap_err_or_panic(self, msg: &str) -> E {
        match self {
            Ok(()) => panic!("{msg}"),
            Err(e) => e,
        }
    }
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
    let src = fixtures::render_template(fixtures::CLI_APP_MANIFEST_TEMPLATE, &"aa".repeat(32));
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
    let src = fixtures::render_template(fixtures::BOTH_APP_MANIFEST_TEMPLATE, &"aa".repeat(32));
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
    let src = fixtures::render_template(fixtures::SAMPLE_APP_MANIFEST_TEMPLATE, &"aa".repeat(32));
    let m = Manifest::from_toml_str(&src).expect("decode");
    let ep = m.entrypoint.as_ref().expect("entrypoint");
    assert_eq!(ep.r#type, pkg::EntrypointType::Gui);
    assert!(m.cli.is_none());
    validate_manifest(&m).expect("gui manifest still validates");
}

#[test]
fn cli_manifest_validates() {
    let src = fixtures::render_template(fixtures::CLI_APP_MANIFEST_TEMPLATE, &"aa".repeat(32));
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
