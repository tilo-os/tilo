//! Syntax and authenticated image carriage for original-source consumer proofs.
//! These records alone neither qualify a worker nor admit source/output custody.
use serde::{Deserialize, Serialize};
use std::io;

pub const ORIGINAL_SOURCE_PROTOCOL: u32 = 5;
pub const PROOF_BOUND: usize = 8192;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CapturePins {
    pub schema: u32,
    pub worker_protocol: u32,
    pub arch: String,
    pub package_id: String,
    pub service_publisher: String,
    pub package_hash: String,
    pub manifest_sha256: String,
    pub payload_root_hash: String,
    pub runtime_version: String,
    pub runtime_publisher: String,
    pub runtime_root_hash: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RuntimeHelper {
    pub role: String,
    pub bundle: String,
    pub sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ConsumerCapability {
    pub schema: u32,
    pub original_source_protocol: u32,
    pub prepared_helper_sha256: String,
    pub installed_launcher_sha256: String,
    pub runtime_launcher_sha256: String,
    pub capture_pins: CapturePins,
    pub runtime_sdk_sha256: String,
    pub runtime_tar_sha256: String,
    pub runtime_openpgp_table_sha256: String,
    pub runtime_helpers: Vec<RuntimeHelper>,
    /// Digest of the actual private export+independent-verifier result. Its
    /// presence is only syntax here; a qualified producer must supply evidence.
    pub qualifier_receipt_sha256: String,
}
fn invalid() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "Original-source consumer proof syntax is invalid",
    )
}
fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}
impl ConsumerCapability {
    /// Validate bounded closed proof syntax only. The enclosing authenticated
    /// image and actual installed qualification are separate authority checks.
    ///
    /// # Errors
    /// Refuses unsupported protocols, malformed capture pins or inconsistent measured hashes.
    pub fn validate(&self) -> io::Result<()> {
        let pins = &self.capture_pins;
        if self.schema != 1
            || !matches!(self.original_source_protocol, ORIGINAL_SOURCE_PROTOCOL | 6)
            || pins.schema != 1
            || pins.worker_protocol != self.original_source_protocol
            || !matches!(pins.arch.as_str(), "x86_64" | "aarch64")
            || pins.package_id != "so.tilo.imager.home-export"
            || !(1..=256).contains(&pins.runtime_version.len())
            || matches!(pins.runtime_version.as_str(), "." | "..")
            || !pins
                .runtime_version
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b"._+-".contains(&b))
            || self.installed_launcher_sha256 != self.runtime_launcher_sha256
            || [
                &self.prepared_helper_sha256,
                &self.installed_launcher_sha256,
                &self.runtime_launcher_sha256,
                &self.runtime_sdk_sha256,
                &self.runtime_tar_sha256,
                &self.runtime_openpgp_table_sha256,
                &self.qualifier_receipt_sha256,
                &pins.service_publisher,
                &pins.package_hash,
                &pins.manifest_sha256,
                &pins.payload_root_hash,
                &pins.runtime_publisher,
                &pins.runtime_root_hash,
            ]
            .iter()
            .any(|value| !digest(value))
            || !(4..=5).contains(&self.runtime_helpers.len())
        {
            return Err(invalid());
        }
        let mut previous = "";
        let mut required = 0_u8;
        for helper in &self.runtime_helpers {
            let bit = match helper.role.as_str() {
                "agent" => 1,
                "gpgconf" => 2,
                "keyboxd" => 0,
                "openpgp" => 4,
                "ostree" => 8,
                _ => return Err(invalid()),
            };
            // The signed builder uses the actual configured executable's
            // resolved leaf (gpg or gpg2, for example). Exact table matching
            // belongs to installed verification, not this syntax check.
            let leaf = helper.bundle.strip_prefix("bin/").ok_or_else(invalid)?;
            if helper.role.as_str() <= previous
                || !(1..=252).contains(&leaf.len())
                || matches!(leaf, "." | "..")
                || !leaf
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"._-".contains(&b))
                || !digest(&helper.sha256)
                || self
                    .runtime_helpers
                    .iter()
                    .any(|other| other.bundle == helper.bundle && other.sha256 != helper.sha256)
            {
                return Err(invalid());
            }
            previous = &helper.role;
            required |= bit;
        }
        if required != 15 || serde_json::to_vec(self)?.len() > PROOF_BOUND {
            return Err(invalid());
        }
        Ok(())
    }
    /// Serde structs also accept positional arrays: callers check wire shape
    /// first, then decode original bytes to preserve duplicate-key refusals.
    pub fn wire_shape(value: &serde_json::Value) -> bool {
        value.is_object()
            && value
                .get("capture_pins")
                .is_some_and(serde_json::Value::is_object)
            && value
                .get("runtime_helpers")
                .and_then(serde_json::Value::as_array)
                .is_some_and(|helpers| {
                    (4..=5).contains(&helpers.len())
                        && helpers.iter().all(serde_json::Value::is_object)
                })
    }
}

/// Check an authenticated selected image declaration, not installed readiness.
/// No requested original-source operation preserves ordinary legacy installs.
///
/// # Errors
/// Refuses missing or malformed consumer proof or a protocol different from the requested one.
pub fn require(capability: Option<&ConsumerCapability>, required: Option<u32>) -> io::Result<()> {
    if let Some(protocol) = required {
        let proof = capability.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::Unsupported,
                "Selected image has no original-source consumer proof",
            )
        })?;
        proof.validate()?;
        if proof.original_source_protocol != protocol {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
pub(crate) fn syntax_fixture() -> ConsumerCapability {
    let hex = "a".repeat(64);
    ConsumerCapability {
        schema: 1,
        original_source_protocol: 5,
        prepared_helper_sha256: hex.clone(),
        installed_launcher_sha256: hex.clone(),
        runtime_launcher_sha256: hex.clone(),
        runtime_sdk_sha256: hex.clone(),
        runtime_tar_sha256: hex.clone(),
        runtime_openpgp_table_sha256: hex.clone(),
        qualifier_receipt_sha256: hex.clone(),
        capture_pins: CapturePins {
            schema: 1,
            worker_protocol: 5,
            arch: "x86_64".into(),
            package_id: "so.tilo.imager.home-export".into(),
            service_publisher: hex.clone(),
            package_hash: hex.clone(),
            manifest_sha256: hex.clone(),
            payload_root_hash: hex.clone(),
            runtime_version: "1.0".into(),
            runtime_publisher: hex.clone(),
            runtime_root_hash: hex.clone(),
        },
        runtime_helpers: [
            ("agent", "bin/gpg-agent"),
            ("gpgconf", "bin/gpgconf"),
            ("openpgp", "bin/gpg"),
            ("ostree", "bin/ostree"),
        ]
        .into_iter()
        .map(|(role, bundle)| RuntimeHelper {
            role: role.into(),
            bundle: bundle.into(),
            sha256: hex.clone(),
        })
        .collect(),
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn closed_original_proof_syntax_refuses_malformed_and_substituted_consumers() {
        let proof = syntax_fixture();
        proof.validate().unwrap();
        let mut configured_leaf = proof.clone();
        configured_leaf.runtime_helpers[2].bundle = "bin/gpg2".into();
        configured_leaf.validate().unwrap();
        let mut shared_leaf = proof.clone();
        shared_leaf.runtime_helpers[1].bundle = shared_leaf.runtime_helpers[0].bundle.clone();
        shared_leaf.validate().unwrap();
        shared_leaf.runtime_helpers[1].sha256 = "b".repeat(64);
        assert!(shared_leaf.validate().is_err());
        let mut with_keyboxd = proof.clone();
        with_keyboxd.runtime_helpers.insert(
            2,
            RuntimeHelper {
                role: "keyboxd".into(),
                bundle: "bin/keyboxd".into(),
                sha256: "a".repeat(64),
            },
        );
        with_keyboxd.validate().unwrap();
        let mut other_arch = proof.clone();
        other_arch.capture_pins.arch = "aarch64".into();
        other_arch.validate().unwrap();
        let mutations: [fn(&mut ConsumerCapability); 7] = [
            |p| p.original_source_protocol = 4,
            |p| p.capture_pins.worker_protocol = 4,
            |p| p.capture_pins.runtime_version = "..".into(),
            |p| p.runtime_launcher_sha256 = "b".repeat(64),
            |p| p.qualifier_receipt_sha256.clear(),
            |p| p.runtime_helpers[0].bundle = "bin/../gpg-agent".into(),
            |p| p.runtime_helpers.swap(0, 1),
        ];
        for mutate in mutations {
            let mut invalid = proof.clone();
            mutate(&mut invalid);
            assert!(invalid.validate().is_err());
        }
        let mut incomplete = proof.clone();
        incomplete.runtime_helpers.pop();
        assert!(incomplete.validate().is_err());
        let value = serde_json::to_value(&proof).unwrap();
        assert!(ConsumerCapability::wire_shape(&value));
        let mut unknown = value.clone();
        unknown["qualified"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ConsumerCapability>(unknown).is_err());
        let mut unknown = value.clone();
        unknown["capture_pins"]["qualified"] = serde_json::json!(true);
        assert!(serde_json::from_value::<ConsumerCapability>(unknown).is_err());
        let mut bad_shape = value;
        bad_shape["runtime_helpers"][0] =
            serde_json::json!(["agent", "bin/gpg-agent", "a".repeat(64)]);
        assert!(!ConsumerCapability::wire_shape(&bad_shape));
        let serialized = serde_json::to_string(&proof).unwrap();
        let duplicate = serialized.replacen("\"schema\":1", "\"schema\":1,\"schema\":1", 1);
        assert!(serde_json::from_str::<ConsumerCapability>(&duplicate).is_err());
        assert!(require(None, None).is_ok());
        assert!(require(None, Some(5)).is_err());
        let mut applications = proof.clone();
        applications.original_source_protocol = 6;
        assert!(applications.validate().is_err());
        applications.capture_pins.worker_protocol = 6;
        applications.validate().unwrap();
        require(Some(&applications), Some(6)).unwrap();
        assert!(require(Some(&proof), Some(6)).is_err());
        assert!(require(Some(&applications), Some(5)).is_err());
    }
}
