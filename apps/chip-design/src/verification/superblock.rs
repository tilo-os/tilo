//! Reported `Q4_K` integer banks; external FP32 reconstruction is not rescaler RTL.

use super::{ReceiptStatus, VerificationReceipt, digest, hashes, text};

pub(super) const CHECKS: &[&str] = &[
    "immutable_real_superblocks",
    "pinned_primary_source",
    "real_integer_banks_protocol_exact_and_primary_C",
    "full_A8_extremes_and_lane_endpoints",
    "fault_sign_rejected",
    "fault_scale_rejected",
    "fault_minimum_rejected",
    "generic_synthesis_and_check",
];

pub(super) const EXCLUSIONS: &[&str] = &[
    "FP32_rescaler_RTL",
    "actual_activation_capture",
    "optimized_backend_equivalence",
    "complete_layer",
    "BF16_conversion_lineage",
    "model_quality",
    "PPA",
    "manufacturability",
];

impl VerificationReceipt {
    pub(super) fn has_superblock_fields(&self) -> bool {
        self.sample_sha256.is_some()
            || self.sample_receipt_sha256.is_some()
            || self.rtl_source_sha256.is_some()
            || self.primary_source_sha256.is_some()
            || self.source_sha256.is_some()
            || self.real_protocol.is_some()
            || self.compiler_flags.is_some()
            || self.external_scalar_c_comparisons.is_some()
            || self.c_instrumentation.is_some()
            || self.generic_cells.is_some()
            || self.generic_cell_count.is_some()
            || self.excluded_metadata_cells.is_some()
    }

    pub(super) fn validate_superblock(&self) -> Result<(), String> {
        // This producer emits complete successful observations. Failed producer
        // diagnostics are not yet a supported shared envelope: their free-form
        // error may contain private paths and must not be silently discarded.
        if self.status != ReceiptStatus::Passed
            || self.declared_toolchain_image.is_none()
            || self.failure.is_some()
            || self.sample_manifest_sha256.is_some()
            || self.verifier_sha256.is_some()
            || self.artifact_validator_sha256.is_some()
            || self.synthetic_activations.is_some()
            || self.input_validation.is_some()
            || self.failure_stage.is_some()
            || self.input_directory.is_some()
            || self.run_directory.is_some()
            || self.synthetic_coefficients.is_some()
            || self.lanes.is_some()
            || self.group_size.is_some()
            || self.input_sha256.is_some()
        {
            return Err("unsupported superblock status or foreign scope fields".into());
        }
        for value in [
            &self.sample_sha256,
            &self.sample_receipt_sha256,
            &self.rtl_source_sha256,
        ] {
            if value.as_ref().is_none_or(|value| !digest(value)) {
                return Err("superblock receipt lacks sample or RTL identity".into());
            }
        }
        self.validate_superblock_sources()?;
        self.validate_superblock_counts()?;
        self.validate_superblock_protocol()?;
        self.validate_superblock_artifacts()
    }

    fn validate_superblock_protocol(&self) -> Result<(), String> {
        if self.external_scalar_c_comparisons != Some(32)
            || self.real_protocol.as_deref()
                != Some("SUPERBLOCK_PASS groups=8 inputs=2048 stalls=7 overlap=7")
            || self.c_instrumentation.as_deref()
                != Some(
                    "write aux32[8] and sumi immediately before original FP32 scaling; no numerical changes",
                )
            || self.compiler_flags.as_ref().is_none_or(|flags| {
                flags.iter().map(String::as_str).collect::<Vec<_>>()
                    != [
                        "-std=c11",
                        "-O0",
                        "-fno-fast-math",
                        "-ffp-contract=off",
                        "oracle.c",
                        "-lm",
                        "-o",
                        "oracle",
                    ]
            })
        {
            return Err("unsupported superblock comparison or scalar compiler contract".into());
        }
        Ok(())
    }

    fn validate_superblock_artifacts(&self) -> Result<(), String> {
        for name in [
            "sample.q4k",
            "vectors.json",
            "c-input.bin",
            "c-output.bin",
            "oracle.c",
            "c-compile.log",
            "external-rescaling.json",
            "real-four/simulation.log",
            "real-four/synthesis.log",
            "real-four/netlist.json",
            "lanes-1/simulation.log",
            "lanes-64/simulation.log",
            "fault-sign/simulation.log",
            "fault-scale/simulation.log",
            "fault-minimum/simulation.log",
        ] {
            if !self.artifact_sha256.contains_key(name) {
                return Err("superblock milestone lacks an observed artifact identity".into());
            }
        }
        if self.artifact_sha256.get("sample.q4k") != self.sample_sha256.as_ref()
            || self.artifact_sha256.get("real-four/tile.sv") != self.rtl_source_sha256.as_ref()
        {
            return Err("superblock sample or RTL artifact identity disagrees".into());
        }
        Ok(())
    }

    fn validate_superblock_sources(&self) -> Result<(), String> {
        for (values, names) in [
            (
                &self.primary_source_sha256,
                &["ggml-common.h", "ggml-quants.c", "cpu-quants.c"][..],
            ),
            (
                &self.source_sha256,
                &[
                    "harness",
                    "group_harness",
                    "q8_reference",
                    "q4_reference",
                    "arithmetic",
                ][..],
            ),
        ] {
            if values.as_ref().is_none_or(|values| {
                !hashes(values)
                    || values.len() != names.len()
                    || names.iter().any(|name| !values.contains_key(*name))
            }) {
                return Err("superblock receipt lacks complete source identities".into());
            }
        }
        let primary = self
            .primary_source_sha256
            .as_ref()
            .ok_or("missing primary sources")?;
        // Detached receipts declare source identities. Validate their consistency;
        // accepting them does not authenticate a kernel implementation or run.
        for (name, hash) in primary {
            if self
                .artifact_sha256
                .get(name)
                .is_some_and(|value| value != hash)
            {
                return Err("primary source and retained artifact identities disagree".into());
            }
        }
        Ok(())
    }

    fn validate_superblock_counts(&self) -> Result<(), String> {
        let cells = self
            .generic_cells
            .as_ref()
            .ok_or("superblock lacks generic cell observations")?;
        if cells.is_empty()
            || cells.len() > 128
            || cells.iter().any(|(name, count)| {
                !text(name)
                    || name.len() > 128
                    || !name.starts_with("$_")
                    || !name
                        .bytes()
                        .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'$' | b'_'))
                    || *count == 0
                    || *count > 100_000_000
            })
            || self.generic_cell_count
                != cells
                    .values()
                    .try_fold(0_u64, |sum, count| sum.checked_add(*count))
            || self.excluded_metadata_cells.is_none_or(|count| count > 128)
        {
            return Err("invalid or inconsistent generic cell counts".into());
        }
        Ok(())
    }
}
