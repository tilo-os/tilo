//! Bounded, reported hardware evidence. No artifact is executed or authenticated.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::model::{MAX_REPORT_BYTES, report_id};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReceiptStatus {
    Passed,
    Failed,
    Cancelled,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckStatus {
    Passed,
    Failed,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ToolIdentity {
    pub version: String,
    pub executable_sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationReceipt {
    pub schema_version: u64,
    pub evidence: String,
    pub status: ReceiptStatus,
    #[serde(deserialize_with = "unique_map")]
    pub checks: BTreeMap<String, CheckStatus>,
    pub declared_toolchain_image: Option<String>,
    #[serde(default, deserialize_with = "unique_map")]
    pub tools: BTreeMap<String, ToolIdentity>,
    #[serde(deserialize_with = "unique_map")]
    pub artifact_sha256: BTreeMap<String, String>,
    pub excludes: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub sample_manifest_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub verifier_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub artifact_validator_sha256: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_activations: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub input_validation: Option<CheckStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure_stage: Option<String>,
    // Device paths are accepted from producers but never synced or published.
    #[serde(skip_serializing)]
    input_directory: Option<String>,
    #[serde(skip_serializing)]
    run_directory: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub synthetic_coefficients: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub lanes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub group_size: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[serde(default, deserialize_with = "unique_optional_map")]
    pub input_sha256: Option<BTreeMap<String, String>>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VerificationRecord {
    pub imported_bytes_blake3: String,
    pub receipt: VerificationReceipt,
}

fn unique_map<'de, D, T>(deserializer: D) -> Result<BTreeMap<String, T>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    struct Entries<T>(std::marker::PhantomData<T>);
    impl<'de, T: Deserialize<'de>> serde::de::Visitor<'de> for Entries<T> {
        type Value = BTreeMap<String, T>;
        fn expecting(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
            formatter.write_str("a map with unique identity keys")
        }
        fn visit_map<A: serde::de::MapAccess<'de>>(
            self,
            mut map: A,
        ) -> Result<Self::Value, A::Error> {
            let mut values = BTreeMap::new();
            while let Some((name, value)) = map.next_entry::<String, T>()? {
                if values.insert(name, value).is_some() {
                    return Err(serde::de::Error::custom(
                        "duplicate receipt identity or check",
                    ));
                }
                if values.len() > 128 {
                    return Err(serde::de::Error::custom("receipt map exceeds entry limit"));
                }
            }
            Ok(values)
        }
    }
    deserializer.deserialize_map(Entries(std::marker::PhantomData))
}

fn unique_optional_map<'de, D>(
    deserializer: D,
) -> Result<Option<BTreeMap<String, String>>, D::Error>
where
    D: serde::Deserializer<'de>,
{
    #[derive(Deserialize)]
    struct Entries(#[serde(deserialize_with = "unique_map")] BTreeMap<String, String>);
    Option::<Entries>::deserialize(deserializer).map(|value| value.map(|entries| entries.0))
}

fn digest(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn text(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 4096
}

fn hashes(values: &BTreeMap<String, String>) -> bool {
    values.len() <= 128
        && values
            .iter()
            .all(|(name, value)| !name.is_empty() && name.len() <= 512 && digest(value))
}

impl VerificationReceipt {
    fn validate(&self) -> Result<(), String> {
        self.validate_annotations()?;
        let (required_checks, required_exclusions): (&[&str], &[&str]) =
            match self.evidence.as_str() {
                "checkpoint_tile_integer_contract" => {
                    self.validate_checkpoint()?;
                    (
                        &[
                            "generic_synthesis",
                            "netlist_vs_integer_oracle",
                            "rtl_vs_integer_oracle",
                        ],
                        &[
                            "full_checkpoint_integrity",
                            "group_rescaling",
                            "activation_quantization",
                            "full_model_execution",
                            "coding_quality",
                            "physical_ppa",
                        ],
                    )
                }
                "rtl_contract_check" => {
                    self.validate_synthetic()?;
                    (
                        &[
                            "generic_synthesis",
                            "rtl_vs_integer_oracle",
                            "synthesized_netlist_vs_integer_oracle",
                        ],
                        &[
                            "model_quality",
                            "checkpoint_weight_validation",
                            "group_rescaling",
                            "full_model_execution",
                            "process_mapping",
                            "physical_area",
                            "power",
                            "timing_closure",
                        ],
                    )
                }
                _ => return Err("unsupported hardware evidence class".into()),
            };
        if self
            .checks
            .keys()
            .any(|name| !required_checks.contains(&name.as_str()))
            || required_exclusions
                .iter()
                .any(|name| !self.excludes.iter().any(|value| value == name))
        {
            return Err("unsupported checks or missing verification exclusions".into());
        }
        match self.status {
            ReceiptStatus::Passed => {
                if self.failure.is_some()
                    || required_checks
                        .iter()
                        .any(|name| self.checks.get(*name) != Some(&CheckStatus::Passed))
                    || ["verilator", "yosys"]
                        .iter()
                        .any(|name| !self.tools.contains_key(*name))
                    || !self.artifact_sha256.contains_key("synthesized.v")
                {
                    return Err(
                        "passed receipt lacks checks, tools or synthesized artifact identity"
                            .into(),
                    );
                }
            }
            ReceiptStatus::Failed | ReceiptStatus::Cancelled if self.failure.is_none() => {
                return Err("failed or cancelled receipt needs a failure explanation".into());
            }
            ReceiptStatus::Failed | ReceiptStatus::Cancelled => {}
        }
        Ok(())
    }
    fn validate_annotations(&self) -> Result<(), String> {
        if self.schema_version != 1 {
            return Err("unsupported verification receipt schema".into());
        }
        if !hashes(&self.artifact_sha256)
            || self.excludes.is_empty()
            || self.excludes.len() > 32
            || self.excludes.iter().any(|value| !text(value))
            || self.tools.len() > 2
            || self.tools.iter().any(|(name, tool)| {
                !["verilator", "yosys"].contains(&name.as_str())
                    || !text(&tool.version)
                    || !digest(&tool.executable_sha256)
            })
            || self.failure.as_ref().is_some_and(|value| {
                !(text(value) || self.status == ReceiptStatus::Cancelled && value.is_empty())
            })
            || [&self.input_directory, &self.run_directory]
                .into_iter()
                .any(|path| path.as_ref().is_some_and(|value| !text(value)))
        {
            return Err("invalid or oversized receipt identities or annotations".into());
        }
        if self.declared_toolchain_image.as_ref().is_some_and(|image| {
            image
                .strip_prefix("sha256:")
                .is_none_or(|value| !digest(value))
        }) {
            return Err("declared toolchain image must be an immutable SHA-256 identity".into());
        }
        Ok(())
    }

    fn validate_checkpoint(&self) -> Result<(), String> {
        if self.synthetic_coefficients.is_some()
            || self.lanes.is_some()
            || self.group_size.is_some()
            || self.input_sha256.is_some()
            || self
                .verifier_sha256
                .as_ref()
                .is_none_or(|value| !digest(value))
            || self
                .sample_manifest_sha256
                .as_ref()
                .is_some_and(|value| !digest(value))
            || self
                .artifact_validator_sha256
                .as_ref()
                .is_some_and(|value| !digest(value))
            || self.synthetic_activations == Some(false)
            || self.failure_stage.as_ref().is_some_and(|stage| {
                !["input_validation", "tool_execution"].contains(&stage.as_str())
            })
        {
            return Err("invalid checkpoint integer receipt fields".into());
        }
        if self.status == ReceiptStatus::Passed
            && (self.synthetic_activations != Some(true)
                || self.sample_manifest_sha256.is_none()
                || self.input_validation == Some(CheckStatus::Failed)
                || self.failure_stage.is_some())
        {
            return Err("passed checkpoint receipt lacks validated sample identity".into());
        }
        if self.input_validation == Some(CheckStatus::Failed) && !self.checks.is_empty() {
            return Err("failed input validation cannot claim executed hardware checks".into());
        }
        Ok(())
    }

    fn validate_synthetic(&self) -> Result<(), String> {
        if self.status == ReceiptStatus::Cancelled
            || self.synthetic_coefficients != Some(true)
            || self.lanes.is_none_or(|value| !(1..=64).contains(&value))
            || self
                .group_size
                .is_none_or(|value| !(1..=5120).contains(&value))
            || self.sample_manifest_sha256.is_some()
            || self.verifier_sha256.is_some()
            || self.artifact_validator_sha256.is_some()
            || self.synthetic_activations.is_some()
            || self.input_validation.is_some()
            || self.failure_stage.is_some()
            || self.input_directory.is_some()
            || self.run_directory.is_some()
        {
            return Err("invalid synthetic integer receipt fields".into());
        }
        let inputs = self
            .input_sha256
            .as_ref()
            .ok_or("receipt lacks input identities")?;
        if !hashes(inputs)
            || [
                "rtl/integer_group_tile.sv",
                "integer_reference.py",
                "verify_tile.py",
                "generated/fixed_linear_fixture.sv",
                "generated/tb.sv",
                "generated/vectors.json",
            ]
            .iter()
            .any(|name| !inputs.contains_key(*name))
        {
            return Err("receipt lacks complete integer input identities".into());
        }
        Ok(())
    }
}

impl VerificationRecord {
    /// Validate reported claims and discard device paths before collaborative storage.
    ///
    /// # Errors
    /// Rejects oversized, malformed, incomplete or unsupported receipt claims.
    pub fn import(json: &str) -> Result<(String, String), String> {
        if json.len() > MAX_REPORT_BYTES {
            return Err("receipt exceeds the 1 MiB import limit".into());
        }
        let mut receipt: VerificationReceipt =
            serde_json::from_str(json).map_err(|error| error.to_string())?;
        receipt.validate()?;
        if let Some(failure) = &mut receipt.failure {
            for (directory, marker) in [
                (&receipt.input_directory, "<input directory>"),
                (&receipt.run_directory, "<run directory>"),
            ] {
                if let Some(directory) = directory {
                    *failure = failure.replace(directory, marker);
                }
            }
        }
        receipt.input_directory = None;
        receipt.run_directory = None;
        let record = Self {
            imported_bytes_blake3: report_id(json),
            receipt,
        };
        let stored = serde_json::to_string(&record).map_err(|error| error.to_string())?;
        if stored.len() > MAX_REPORT_BYTES {
            return Err("sanitized receipt exceeds the import limit".into());
        }
        Ok((report_id(&stored), stored))
    }

    /// Read a sanitized immutable envelope from the collaborative document.
    ///
    /// # Errors
    /// Rejects invalid identity, unsanitized paths or unsupported receipt claims.
    pub fn read(json: &str) -> Result<Self, String> {
        if json.len() > MAX_REPORT_BYTES {
            return Err("stored receipt exceeds the import limit".into());
        }
        let record: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        if !digest(&record.imported_bytes_blake3)
            || record.receipt.input_directory.is_some()
            || record.receipt.run_directory.is_some()
        {
            return Err("invalid shared receipt envelope".into());
        }
        record.receipt.validate()?;
        Ok(record)
    }
}
