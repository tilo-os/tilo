//! Embedded catalog admission and private manifest authentication.
use crate::catalog_data::{
    CatalogRelease, InternalLayout, InternalMetadata, InternalPurpose, parse_catalog,
};
use sha2::{Digest, Sha256};
const BUNDLED_CATALOG: &str = include_str!("../release-catalog.json");
pub fn bundled_catalog_json() -> &'static str {
    BUNDLED_CATALOG
}
pub fn catalog_architecture() -> &'static str {
    std::env::consts::ARCH
}
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ReleaseSelection {
    pub(crate) release: InternalRelease,
}
impl ReleaseSelection {
    pub fn release_id(&self) -> &str {
        self.release.release_id()
    }
    pub fn architecture(&self) -> &str {
        self.release.architecture()
    }
    pub fn manifest_sha256(&self) -> &str {
        self.release.manifest_sha256()
    }
}
pub fn bundled_selections() -> Result<Vec<ReleaseSelection>, crate::Error> {
    let catalog = parse_catalog(BUNDLED_CATALOG).map_err(crate::Error::invalid_input)?;
    Ok(catalog
        .releases
        .into_iter()
        .filter_map(|entry| match entry {
            CatalogRelease::Internal(metadata) if metadata.arch == catalog_architecture() => {
                Some(ReleaseSelection {
                    release: InternalRelease { metadata },
                })
            }
            _ => None,
        })
        .collect())
}
/// A selection obtained only from this build's embedded catalog. Authentication
/// depends on a genuinely signed build; development embedding is not a signature.
/// This type cannot become a whole-disk `Source` or a media download request.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct InternalRelease {
    metadata: InternalMetadata,
}

impl InternalRelease {
    pub(crate) fn release_id(&self) -> &str {
        &self.metadata.release_id
    }
    pub(crate) fn architecture(&self) -> &str {
        &self.metadata.arch
    }
    pub(crate) fn manifest_url(&self) -> &str {
        &self.metadata.manifest_url
    }
    pub(crate) fn manifest_sha256(&self) -> &str {
        &self.metadata.manifest_sha256
    }

    #[cfg(any(test, feature = "test-support"))]
    pub(crate) fn transport_fixture(bytes: &[u8], url: &str) -> Self {
        Self {
            metadata: InternalMetadata {
                purpose: InternalPurpose::InternalInstaller,
                layout: InternalLayout::BundleV1,
                release_id: "fixture-only".into(),
                arch: "x86_64".into(),
                manifest_url: url.into(),
                manifest_sha256: format!("{:x}", Sha256::digest(bytes)),
                manifest_size: bytes.len() as u64,
            },
        }
    }

    pub(crate) fn manifest_size(&self) -> u64 {
        self.metadata.manifest_size
    }

    /// Bind the exact received bytes to the embedded selection before parsing.
    /// A later downloader must retain existing origin/length rules; this method
    /// performs no transport, staging, execution or device access.
    /// # Errors
    /// Rejects byte length/digest mismatch, invalid schema or selection mismatch.
    pub(crate) fn verify_manifest(
        &self,
        bytes: &[u8],
    ) -> Result<crate::bundle::InternalBundle, String> {
        let expected = &self.metadata;
        if bytes.len() as u64 != expected.manifest_size
            || format!("{:x}", Sha256::digest(bytes)) != expected.manifest_sha256
        {
            return Err("Internal manifest does not match the embedded catalog bytes.".into());
        }
        let bundle = crate::bundle::validate_internal_bundle(bytes)?;
        let (InternalPurpose::InternalInstaller, InternalLayout::BundleV1) =
            (&expected.purpose, &expected.layout);
        if bundle.release_id() != expected.release_id
            || bundle.architecture() != expected.arch
            || bundle.purpose() != "qos-internal-installer"
        {
            return Err("Internal manifest does not match the selected release.".into());
        }
        Ok(bundle)
    }
}

#[cfg(test)]
mod internal_catalog_tests {
    use super::*;
    use serde_json::{Value, json};
    fn media() -> Value {
        json!({"name":"qOS","version":"test","channel":"test","arch":"x86_64",
            "url":"https://code.podesta.ai/test.img.zst","archive_sha256":"a".repeat(64),
            "archive_size":42,"image_sha256":"b".repeat(64),"image_size":84})
    }
    fn catalog(entry: Value) -> String {
        json!({"schema":1,"tag":"imager-vtest","releases":[entry]}).to_string()
    }
    #[test]
    fn legacy_media_still_parses() {
        assert!(parse_catalog(&catalog(media())).is_ok());
    }
    #[test]
    fn explicit_internal_purpose_never_falls_through_as_media() {
        let mut value = media();
        value["purpose"] = json!("qos-internal-installer");
        value["layout"] = json!("internal-bundle-v1");
        assert!(parse_catalog(&catalog(value)).is_err());
    }
    #[test]
    fn catalog_rejects_unknown_fields_and_bad_foreign_entries() {
        let mut value = media();
        value["unexpected"] = json!(true);
        assert!(parse_catalog(&catalog(value)).is_err());
        for (field, bad) in [
            ("archive_sha256", json!("invalid")),
            ("url", json!("https://evil.example/file")),
            ("image_size", json!(0)),
        ] {
            let mut value = media();
            value["arch"] = json!("aarch64");
            value[field] = bad;
            assert!(parse_catalog(&catalog(value)).is_err(), "accepted {field}");
        }
    }
    fn internal(manifest: &[u8]) -> Value {
        json!({"purpose":"qos-internal-installer","layout":"internal-bundle-v1",
            "release_id":"fixture-only","arch":"x86_64",
            "manifest_url":"https://code.podesta.ai/fixture/manifest.json",
            "manifest_sha256":format!("{:x}",Sha256::digest(manifest)),"manifest_size":manifest.len()})
    }
    fn selection(value: Value) -> InternalRelease {
        match parse_catalog(&catalog(value))
            .unwrap()
            .releases
            .pop()
            .unwrap()
        {
            CatalogRelease::Internal(metadata) => InternalRelease { metadata },
            _ => panic!("internal entry became media"),
        }
    }
    #[test]
    fn internal_entries_remain_separate_and_bind_exact_manifest_bytes() {
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        let value = internal(&bytes);
        assert!(
            parse_catalog(&catalog(value.clone()))
                .unwrap()
                .releases
                .iter()
                .all(|entry| !matches!(entry, CatalogRelease::Media(_)))
        );
        let release = selection(value);
        assert!(release.verify_manifest(&bytes).is_ok());
        let mut longer = bytes.clone();
        longer.push(b' ');
        assert!(release.verify_manifest(&longer).is_err());
        let mut changed_manifest = crate::bundle::tests::fixture();
        changed_manifest["boot_binding"]["uki_sha256"] = json!("c".repeat(64));
        let changed = crate::bundle::tests::bytes(&changed_manifest);
        assert_eq!(changed.len(), bytes.len());
        let valid = crate::bundle::validate_internal_bundle(&changed).unwrap();
        assert_eq!(valid.release_id(), release.release_id());
        assert_eq!(valid.architecture(), release.architecture());
        assert!(release.verify_manifest(&changed).is_err());
    }
    #[test]
    fn internal_selection_rejects_release_and_architecture_mismatch() {
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        for (key, value) in [
            ("release_id", json!("other-release")),
            ("arch", json!("aarch64")),
        ] {
            let mut entry = internal(&bytes);
            entry[key] = value;
            assert!(
                selection(entry).verify_manifest(&bytes).is_err(),
                "accepted {key}"
            );
        }
    }

    #[test]
    fn explicit_media_pair_and_internal_catalog_profiles_are_strict() {
        let mut tagged = media();
        tagged["purpose"] = json!("removable-media");
        tagged["layout"] = json!("whole-disk-image");
        assert_eq!(
            parse_catalog(&catalog(tagged.clone()))
                .unwrap()
                .releases
                .iter()
                .filter(|entry| matches!(entry, CatalogRelease::Media(_)))
                .count(),
            1
        );
        for bad in [Value::Null, json!("unknown"), json!(true)] {
            let mut value = tagged.clone();
            value["purpose"] = bad;
            assert!(parse_catalog(&catalog(value)).is_err());
        }
        tagged.as_object_mut().unwrap().remove("layout");
        assert!(parse_catalog(&catalog(tagged)).is_err());
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        for (field, bad) in [
            ("layout", json!("whole-disk-image")),
            ("manifest_size", json!(65537)),
            ("manifest_sha256", json!("A".repeat(64))),
            ("purpose", json!("removable-media")),
            (
                "manifest_url",
                json!("https://code.podesta.ai:444/manifest"),
            ),
        ] {
            let mut value = internal(&bytes);
            value[field] = bad;
            assert!(parse_catalog(&catalog(value)).is_err(), "accepted {field}");
        }
        assert!(bundled_selections().unwrap().is_empty());
        assert!(
            parse_catalog(bundled_catalog_json())
                .unwrap()
                .releases
                .is_empty()
        );
        let raw = catalog(media()).replacen("\"arch\":", "\"arch\":\"x86_64\",\"arch\":", 1);
        assert!(parse_catalog(&raw).is_err());
    }
    #[test]
    fn embedded_selection_cannot_be_minted_from_parsed_data() {
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        let mut metadata = internal(&bytes);
        metadata["release_id"] = json!("mutated-valid-id");
        assert!(parse_catalog(&catalog(metadata)).is_ok());
        let embedded = parse_catalog(bundled_catalog_json()).unwrap();
        let expected: Vec<_> = embedded
            .releases
            .into_iter()
            .filter_map(|entry| match entry {
                CatalogRelease::Internal(metadata) if metadata.arch == catalog_architecture() => {
                    Some(metadata)
                }
                _ => None,
            })
            .collect();
        let selections = bundled_selections().unwrap();
        assert_eq!(selections.len(), expected.len());
        for (selection, expected) in selections.iter().zip(expected) {
            assert_eq!(selection.release.metadata, expected);
        }
        for (field, value) in [
            ("manifest_sha256", json!("0".repeat(64))),
            ("manifest_size", json!(bytes.len() + 1)),
            ("arch", json!("aarch64")),
        ] {
            let mut value_metadata = internal(&bytes);
            value_metadata[field] = value;
            assert!(selection(value_metadata).verify_manifest(&bytes).is_err());
        }
        let mut wrong_purpose = crate::bundle::tests::fixture();
        wrong_purpose["purpose"] = json!("removable-media");
        let wrong_bytes = crate::bundle::tests::bytes(&wrong_purpose);
        assert!(
            selection(internal(&wrong_bytes))
                .verify_manifest(&wrong_bytes)
                .is_err()
        );
    }
    fn positional_record(value: &Value, fields: &str) -> Value {
        Value::Array(
            fields
                .split_whitespace()
                .map(|key| value[key].clone())
                .collect(),
        )
    }
    #[test]
    fn representation_catalog_must_be_an_object() {
        let raw = catalog(media());
        assert!(parse_catalog(&raw).is_ok());
        let value: Value = serde_json::from_str(&raw).unwrap();
        let array = positional_record(&value, "schema tag releases");
        assert!(parse_catalog(&array.to_string()).is_err());
    }
    #[test]
    fn representation_media_entry_must_be_an_object() {
        let mut value = media();
        value["purpose"] = json!("removable-media");
        value["layout"] = json!("whole-disk-image");
        assert!(parse_catalog(&catalog(value.clone())).is_ok());
        let array = positional_record(
            &value,
            "purpose layout name version channel arch url archive_sha256 archive_size image_sha256 image_size",
        );
        assert!(parse_catalog(&catalog(array)).is_err());
    }
    #[test]
    fn representation_internal_entry_must_be_an_object() {
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        let value = internal(&bytes);
        assert!(parse_catalog(&catalog(value.clone())).is_ok());
        let array = positional_record(
            &value,
            "purpose layout release_id arch manifest_url manifest_sha256 manifest_size",
        );
        assert!(parse_catalog(&catalog(array)).is_err());
    }
    #[test]
    fn representation_internal_purpose_and_layout_must_be_strings() {
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        let entry = internal(&bytes);
        assert!(parse_catalog(&catalog(entry.clone())).is_ok());
        let accepted: Vec<_> = ["purpose", "layout"]
            .into_iter()
            .filter(|field| {
                let mut value = entry.clone();
                value[*field] = json!({entry[*field].as_str().unwrap():null});
                parse_catalog(&catalog(value)).is_ok()
            })
            .collect();
        assert!(
            accepted.is_empty(),
            "accepted object-shaped tags: {accepted:?}"
        );
    }
    #[test]
    fn representation_media_purpose_and_layout_must_be_strings() {
        let mut entry = media();
        entry["purpose"] = json!("removable-media");
        entry["layout"] = json!("whole-disk-image");
        assert!(parse_catalog(&catalog(entry.clone())).is_ok());
        let accepted: Vec<_> = ["purpose", "layout"]
            .into_iter()
            .filter(|field| {
                let mut value = entry.clone();
                value[*field] = json!({entry[*field].as_str().unwrap():null});
                parse_catalog(&catalog(value)).is_ok()
            })
            .collect();
        assert!(
            accepted.is_empty(),
            "accepted object-shaped tags: {accepted:?}"
        );
    }
    #[test]
    fn representation_checks_do_not_collapse_duplicate_catalog_keys() {
        let bytes = crate::bundle::tests::bytes(&crate::bundle::tests::fixture());
        for entry in [media(), internal(&bytes)] {
            let raw = catalog(entry);
            assert!(parse_catalog(&raw).is_ok());
            for (marker, replacement) in [
                ("\"schema\":1", "\"schema\":1,\"schema\":1"),
                (
                    "\"arch\":\"x86_64\"",
                    "\"arch\":\"x86_64\",\"arch\":\"x86_64\"",
                ),
            ] {
                let changed = raw.replacen(marker, replacement, 1);
                assert_ne!(changed, raw);
                assert!(parse_catalog(&changed).is_err());
            }
        }
    }
}

#[cfg(any(test, feature = "test-support"))]
pub(crate) fn fixture_selection(metadata: InternalMetadata) -> ReleaseSelection {
    ReleaseSelection {
        release: InternalRelease { metadata },
    }
}
