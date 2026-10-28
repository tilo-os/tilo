//! Desktop metadata authenticated by the signature on the setup application.

use serde::Deserialize;
use std::collections::HashSet;

/// Package kind is independent of runtime support evidence.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum PackageFormat {
    Rpm,
    Deb,
    Arch,
    NixClosure,
}

#[cfg(unix)]
impl PackageFormat {
    pub(super) fn extension(self) -> &'static str {
        match self {
            Self::Rpm => "rpm",
            Self::Deb => "deb",
            Self::Arch => "pkg.tar.zst",
            Self::NixClosure => "nar",
        }
    }
}

/// Explicit reviewed native-session evidence, bound to these exact package bytes.
#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeEvidence {
    pub schema: u32,
    pub reference: String,
    pub package_sha256: String,
    pub graphical_login: bool,
    pub return_to_prior_session: bool,
    pub identity_setup: bool,
    pub selected_folder_import: bool,
    pub source_intact: bool,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DesktopRelease {
    pub id: String,
    /// Exact native package version, including its revision/epoch where applicable.
    /// Future installer adapters must query the same native version representation.
    pub version: String,
    pub distro_id: String,
    pub distro_version: String,
    pub architecture: String,
    pub package_format: PackageFormat,
    pub url: String,
    pub byte_size: u64,
    pub sha256: String,
    pub verified_runtime: Option<RuntimeEvidence>,
    /// Consumer proof extracted from these exact package bytes by the signed catalog producer.
    #[serde(default)]
    pub account_custody: Option<super::account_admission::ConsumerCapability>,
}

impl DesktopRelease {
    /// No ID_LIKE inheritance, version approximation, or architecture aliasing.
    pub fn matches_platform(
        &self,
        distro_id: &str,
        distro_version: &str,
        architecture: &str,
    ) -> bool {
        self.distro_id == distro_id
            && self.distro_version == distro_version
            && self.architecture == architecture
    }

    pub(super) fn validate_metadata(&self) -> Result<(), String> {
        if [
            &self.id,
            &self.version,
            &self.distro_id,
            &self.distro_version,
        ]
        .iter()
        .any(|value| {
            value.is_empty()
                || value.trim() != value.as_str()
                || value.chars().any(char::is_control)
        }) || !self
            .distro_id
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
        {
            return Err("The Desktop catalog contains an invalid release selector.".into());
        }
        if !matches!(self.architecture.as_str(), "aarch64" | "x86_64") {
            return Err("The Desktop catalog contains an unsupported architecture.".into());
        }
        if self.byte_size == 0 || self.byte_size > i64::MAX as u64 || !valid_digest(&self.sha256) {
            return Err("The Desktop catalog contains an invalid package size or SHA-256.".into());
        }
        if let Some(evidence) = &self.verified_runtime {
            if evidence.schema != 1
                || evidence.reference.trim().is_empty()
                || evidence.package_sha256 != self.sha256
                || !evidence.graphical_login
                || !evidence.return_to_prior_session
                || !evidence.identity_setup
                || !evidence.selected_folder_import
                || !evidence.source_intact
            {
                return Err(
                    "The Desktop catalog contains incomplete or mismatched runtime evidence."
                        .into(),
                );
            }
        }
        if let Some(capability) = &self.account_custody {
            capability
                .validate()
                .map_err(|_| "Invalid Desktop account consumer proof.".to_string())?;
        }
        Ok(())
    }

    pub(super) fn require_account_protocol(&self, required: Option<u32>) -> Result<(), String> {
        self.validate_metadata()?;
        if let Some(protocol) = required {
            if !self
                .account_custody
                .as_ref()
                .is_some_and(|proof| proof.protected_grant_protocol == protocol)
            {
                return Err(super::strings::t(
                    "imager-setup-account-consumer-unavailable",
                ));
            }
        }
        Ok(())
    }

    pub(super) fn require_service_session_protocol(
        &self,
        required: Option<u32>,
    ) -> Result<(), String> {
        self.validate_metadata()?;
        if required.is_some_and(|protocol| {
            self.account_custody.as_ref().is_none_or(|proof| {
                !proof.supports_service_session_protocol(protocol)
                    && !super::account_admission::source_candidate(protocol, proof).unwrap_or(false)
            })
        }) {
            return Err(super::strings::t(
                "imager-setup-account-consumer-unavailable",
            ));
        }
        Ok(())
    }
}

fn valid_digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

pub(super) fn trusted_url(value: &str) -> Result<reqwest::Url, String> {
    let url = reqwest::Url::parse(value).map_err(|_| "Invalid Desktop package URL.".to_string())?;
    if url.scheme() != "https"
        || url.host_str() != Some("code.tilo.so")
        || url.port_or_known_default() != Some(443)
        || !url.username().is_empty()
        || url.password().is_some()
        || url.fragment().is_some()
    {
        return Err("Untrusted Desktop package URL.".into());
    }
    Ok(url)
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    schema: u32,
    tag: String,
    releases: Vec<DesktopRelease>,
}

/// Parse all entries before selecting a host, so invalid foreign entries also fail closed.
///
/// # Errors
/// Rejects unsupported schemas, unknown fields, invalid identities, evidence, and origins.
pub fn parse_catalog(json: &str) -> Result<Vec<DesktopRelease>, String> {
    let catalog: Catalog =
        serde_json::from_str(json).map_err(|error| format!("Invalid Desktop catalog: {error}"))?;
    if catalog.schema != 1
        || catalog.tag.trim().is_empty()
        || (catalog.tag == "unreleased") != catalog.releases.is_empty()
    {
        return Err("Unsupported or inconsistent Desktop catalog schema/release state.".into());
    }
    let mut selectors = HashSet::new();
    let mut ids = HashSet::new();
    for release in &catalog.releases {
        release.validate_metadata()?;
        trusted_url(&release.url)?;
        if !ids.insert(&release.id)
            || !selectors.insert((
                &release.distro_id,
                &release.distro_version,
                &release.architecture,
            ))
        {
            return Err("Duplicate Desktop release id or platform selector.".into());
        }
    }
    Ok(catalog.releases)
}

/// Catalog authenticated by the signed application, never a network manifest.
pub fn bundled_releases() -> Result<Vec<DesktopRelease>, String> {
    parse_catalog(include_str!("../../desktop-catalog.json"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};

    pub(super) fn release_json() -> Value {
        json!({"id":"fedora-44-aarch64", "version":"test-1", "distro_id":"fedora",
            "distro_version":"44", "architecture":"aarch64", "package_format":"rpm",
            "url":"https://code.tilo.so/tilo/releases/test.rpm", "byte_size":7,
            "sha256":"a".repeat(64), "verified_runtime":null})
    }

    fn catalog(releases: Vec<Value>) -> String {
        json!({"schema":1,"tag":"test-1","releases":releases}).to_string()
    }

    #[test]
    fn desktop_catalog_accepts_strict_typed_entries_and_empty_releases() {
        {
            assert!(
                parse_catalog(include_str!("../../desktop-catalog.json"))
                    .unwrap()
                    .is_empty()
            );
            assert!(parse_catalog(r#"{"schema":1,"tag":"unreleased","releases":[]}"#).is_ok());
            assert!(parse_catalog(r#"{"schema":2,"tag":"unreleased","releases":[]}"#).is_err());
            assert!(parse_catalog(r#"{"schema":1,"tag":"released","releases":[]}"#).is_err());
        }

        {
            for format in ["rpm", "deb", "arch", "nix_closure"] {
                let mut release = release_json();
                release["package_format"] = json!(format);
                let parsed = parse_catalog(&catalog(vec![release])).unwrap().remove(0);
                assert!(parsed.matches_platform("fedora", "44", "aarch64"));
                assert!(!parsed.matches_platform("example-fedora", "44", "aarch64"));
                assert!(!parsed.matches_platform("fedora", "44", "x86_64"));
                assert!(!parsed.matches_platform("fedora", "44.0", "aarch64"));
                assert!(parsed.verified_runtime.is_none());
            }
        }

        {
            let mut release = release_json();
            release["verified_runtime"] = json!({"schema":1,"reference":"docs/evidence/test.md",
                "package_sha256":"a".repeat(64),"graphical_login":true,"return_to_prior_session":true,"identity_setup":true,"selected_folder_import":true,"source_intact":true});
            assert!(parse_catalog(&catalog(vec![release.clone()])).is_ok());
            for (field, value) in [
                ("schema", json!(2)),
                ("package_sha256", json!("b".repeat(64))),
                ("reference", json!("")),
                ("graphical_login", json!(false)),
                ("return_to_prior_session", json!(false)),
                ("identity_setup", json!(false)),
                ("selected_folder_import", json!(false)),
                ("source_intact", json!(false)),
                ("unknown", json!(true)),
            ] {
                let mut altered = release.clone();
                altered["verified_runtime"][field] = value;
                assert!(
                    parse_catalog(&catalog(vec![altered])).is_err(),
                    "accepted {field}"
                );
            }
        }
    }

    #[test]
    fn desktop_catalog_authenticates_strict_package_consumer_declaration() {
        let mut entry = release_json();
        entry["account_custody"] = json!({"schema":1,"protected_grant_protocol":1,
            "credential_helper_sha256":"1".repeat(64),"runtime_root_hash":"2".repeat(64),
            "runtime_probe_sha256":"3".repeat(64),"runtime_library_sha256":"4".repeat(64),
            "runtime_version":"fixture-1","runtime_library":"lib/libruntime_dylib.so"});
        let parsed = parse_catalog(&catalog(vec![entry.clone()]))
            .unwrap()
            .remove(0);
        parsed.require_account_protocol(Some(1)).unwrap();
        parsed.require_service_session_protocol(None).unwrap();
        assert!(parsed.require_service_session_protocol(Some(1)).is_err());
        assert!(parsed.require_account_protocol(Some(2)).is_err());
        for (field, value) in [
            ("schema", json!(2)),
            ("protected_grant_protocol", json!(2)),
            ("runtime_library", json!("../libruntime_dylib.so")),
            ("unknown", json!(true)),
        ] {
            let mut altered = entry.clone();
            altered["account_custody"][field] = value;
            assert!(parse_catalog(&catalog(vec![altered])).is_err());
        }
        let legacy = parse_catalog(&catalog(vec![release_json()]))
            .unwrap()
            .remove(0);
        legacy.require_account_protocol(None).unwrap();
        assert!(legacy.require_account_protocol(Some(1)).is_err());
    }

    #[test]
    fn desktop_catalog_rejects_duplicates_invalid_metadata_and_untrusted_urls() {
        {
            let release = release_json();
            let mut other = release.clone();
            other["id"] = json!("different-id");
            assert!(parse_catalog(&catalog(vec![release.clone(), other])).is_err());
            let mut other = release.clone();
            other["distro_version"] = json!("45");
            assert!(parse_catalog(&catalog(vec![release, other])).is_err());
        }

        {
            for (field, value) in [
                ("sha256", json!("bad")),
                ("byte_size", json!(0)),
                ("package_format", json!("exe")),
                ("architecture", json!("arm64")),
                ("id", json!("")),
                ("distro_id", json!("Fedora")),
                ("unexpected", json!(true)),
            ] {
                let mut release = release_json();
                release[field] = value;
                assert!(
                    parse_catalog(&catalog(vec![release])).is_err(),
                    "accepted {field}"
                );
            }
            let mut value: Value = serde_json::from_str(&catalog(vec![release_json()])).unwrap();
            value["unexpected"] = json!(true);
            assert!(parse_catalog(&value.to_string()).is_err());
            value.as_object_mut().unwrap().remove("unexpected");
            value["tag"] = json!("unreleased");
            assert!(parse_catalog(&value.to_string()).is_err());
        }

        {
            for url in [
                "http://code.tilo.so/test.rpm",
                "https://example.com/test.rpm",
                "https://code.tilo.so:444/test.rpm",
                "https://user@code.tilo.so/test.rpm",
                "https://code.tilo.so/test.rpm#fragment",
            ] {
                let mut release = release_json();
                release["url"] = json!(url);
                assert!(
                    parse_catalog(&catalog(vec![release])).is_err(),
                    "accepted {url}"
                );
            }
        }
    }
}
