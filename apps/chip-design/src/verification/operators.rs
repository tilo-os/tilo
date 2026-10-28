//! Strict, bounded CPU numerical observations. Imports report claims, never run code.

use super::{
    CheckStatus, ReceiptStatus, VerificationReceipt, digest, hashes, identity_name, unique_map,
    unique_optional_map,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OperatorSummary {
    pub original_receipt_sha256: String,
    pub primary_revision: String,
    #[serde(deserialize_with = "unique_map")]
    pub primary_source_sha256: BTreeMap<String, String>,
    #[serde(deserialize_with = "unique_map")]
    pub local_source_sha256: BTreeMap<String, String>,
    #[serde(deserialize_with = "unique_map")]
    pub linked_library_sha256: BTreeMap<String, String>,
    pub compiler_version: String,
    pub compiler_executable_sha256: String,
    pub compiler_flags: Vec<String>,
    pub observations: Observations,
    pub sample: Option<NormSample>,
    pub accumulator_fault: Option<AccumulatorFault>,
    pub mutation_differing_silu_words: Option<u64>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "operator", content = "cases", rename_all = "snake_case")]
pub enum Observations {
    RmsNorm(Vec<NormComparison>),
    Silu(SiluComparisons),
    GatedDelta(Vec<DeltaComparison>),
    Softmax(Vec<SoftmaxComparison>),
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormComparison {
    pub c_output_sha256: String,
    pub fp32_comparisons: u64,
    pub mean_bits: u32,
    pub profile: String,
    pub scale_bits: u32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormSample {
    pub absolute_offset: u64,
    pub epsilon_bits: u32,
    pub header_sha256: String,
    pub sha256: String,
    pub tensor: NormTensor,
    pub weight_bytes: u64,
    pub whole_file_sha256: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct NormTensor {
    pub elements: u64,
    pub name: String,
    pub payload_bytes: u64,
    pub relative_offset: u64,
    pub shape_ggml_order: Vec<u64>,
    pub storage: String,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccumulatorFault {
    pub differing_profiles: Vec<String>,
    pub mutant_source_sha256: String,
    pub observed_output_sha256: String,
    pub process_exit: i32,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DifferenceExample {
    pub input_bits: u32,
    pub observed_exp_bits: u32,
    pub reference_exp_bits: u32,
    pub ulp: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SiluComparisons {
    #[serde(rename = "additional_FP32_inputs")]
    pub additional_fp32_inputs: u64,
    pub difference_examples: Vec<DifferenceExample>,
    pub exp_difference_examples: Vec<DifferenceExample>,
    pub exp_differences: u64,
    #[serde(rename = "finite_BF16_sourced_FP32_inputs")]
    pub finite_bf16_sourced_fp32_inputs: u64,
    pub max_exp_ulp: u64,
    pub max_silu_ulp: u64,
    pub silu_differences: u64,
    pub total_inputs: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaConfig {
    pub heads: u64,
    pub key_heads: u64,
    pub slots: u64,
    pub tokens: u64,
    pub width: u64,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeltaComparison {
    pub attention_words: u64,
    pub config: DeltaConfig,
    pub primary_sha256: String,
    pub state_words: u64,
    pub unwritten_slots: Vec<u64>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SoftmaxComparison {
    pub differing_words: u64,
    pub masked: u64,
    pub max_ulp: u64,
    pub name: String,
    pub observed_probability_sum_fraction: String,
    pub primary_sha256: String,
    pub width: u64,
}

#[derive(Deserialize)]
#[serde(untagged)]
enum RawComparisons {
    Norm(Vec<NormComparison>),
    Silu(SiluComparisons),
    Delta(Vec<DeltaComparison>),
    Softmax(Vec<SoftmaxComparison>),
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReceipt {
    schema_version: u64,
    evidence: String,
    status: ReceiptStatus,
    #[serde(deserialize_with = "unique_map")]
    checks: BTreeMap<String, CheckStatus>,
    excludes: Vec<String>,
    primary_revision: String,
    #[serde(deserialize_with = "unique_map")]
    primary_source_sha256: BTreeMap<String, String>,
    compiler_version: String,
    compiler_executable_sha256: String,
    compiler_flags: Vec<String>,
    comparisons: RawComparisons,
    #[serde(default, deserialize_with = "unique_map")]
    artifact_sha256: BTreeMap<String, String>,
    #[serde(default, deserialize_with = "unique_optional_map")]
    local_source_sha256: Option<BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "unique_optional_map")]
    source_sha256: Option<BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "unique_optional_map")]
    linked_library_sha256: Option<BTreeMap<String, String>>,
    #[serde(default, deserialize_with = "unique_optional_map")]
    observed_linked_library_sha256: Option<BTreeMap<String, String>>,
    input_sha256: Option<String>,
    oracle_sha256: Option<String>,
    reference_sha256: Option<String>,
    sample: Option<NormSample>,
    accumulator_fault: Option<AccumulatorFault>,
    mutation_differing_silu_words: Option<u64>,
}

pub(super) fn supported(evidence: &str) -> bool {
    contract(evidence).is_some()
}
fn contract(evidence: &str) -> Option<(&'static [&'static str], &'static [&'static str])> {
    Some(match evidence {
        "bounded_fused_scalar_rms_norm" => (
            &[
                "actual_model_norm_weights_epsilon",
                "compiled_FP32_accumulator_fault_detected",
                "six_synthetic_profiles_vs_pinned_fused_function",
            ],
            &[
                "actual_hidden_states",
                "unfused_SIMD_or_GPU_backend",
                "model_quality",
                "full_graph_execution",
                "RTL",
                "PPA",
                "manufacturability",
            ],
        ),
        "bounded_scalar_silu_libm_characterization" => (
            &[
                "compiled_exponential_sign_fault_detected",
                "local_source_lifetime_stable",
                "scalar_primary_vs_stabilized_decimal_exp_reference",
            ],
            &[
                "actual_hidden_states",
                "model_quality",
                "SIMD_GPU_equivalence",
                "correct_rounding_proof_for_all_FP32",
                "RTL",
                "PPA",
            ],
        ),
        "bounded_scalar_gated_delta_state_transition" => (
            &[
                "compiled_outer_product_key_query_fault_detected",
                "latest_state_snapshots_and_caller_owned_unwritten_slot",
                "local_source_lifetime_stable",
                "primary_attention_and_transposed_state_words",
            ],
            &[
                "actual_hidden_states",
                "complete_recurrent_block",
                "multi_sequence",
                "KDA",
                "SIMD_GPU_or_prefill_equivalence",
                "model_quality",
                "RTL",
                "PPA",
            ],
        ),
        "bounded_scalar_masked_softmax_characterization" => (
            &[
                "compiled_removed_normalization_fault_detected",
                "local_source_lifetime_stable",
                "masked_probabilities_exact_zero",
                "observed_scalar_preprocessor_branch",
                "primary_outputs_within_declared_three_ulp_envelope",
            ],
            &[
                "actual_attention_scores",
                "model_quality",
                "all_masked_rows",
                "FP16_masks",
                "sinks",
                "ALiBi",
                "nonunit_input_scale",
                "SIMD_GPU_equivalence",
                "correct_rounding_proof_for_all_FP32",
                "RTL",
                "PPA",
            ],
        ),
        _ => return None,
    })
}

pub(super) fn import(json: &str) -> Result<VerificationReceipt, String> {
    let raw: RawReceipt = serde_json::from_str(json).map_err(|error| error.to_string())?;
    if raw.schema_version != 1 || raw.status != ReceiptStatus::Passed {
        return Err(
            "unsupported CPU receipt schema/status; free-form failures are not shared".into(),
        );
    }
    let observations = match (raw.evidence.as_str(), raw.comparisons) {
        ("bounded_fused_scalar_rms_norm", RawComparisons::Norm(cases)) => {
            Observations::RmsNorm(cases)
        }
        ("bounded_scalar_silu_libm_characterization", RawComparisons::Silu(cases)) => {
            Observations::Silu(cases)
        }
        ("bounded_scalar_gated_delta_state_transition", RawComparisons::Delta(cases)) => {
            Observations::GatedDelta(cases)
        }
        ("bounded_scalar_masked_softmax_characterization", RawComparisons::Softmax(cases)) => {
            Observations::Softmax(cases)
        }
        _ => return Err("CPU receipt evidence contradicts observation type".into()),
    };
    let mut artifacts = raw.artifact_sha256;
    let norm = matches!(observations, Observations::RmsNorm(_));
    for (name, value) in [
        ("input.f32", raw.input_sha256),
        ("oracle.cpp", raw.oracle_sha256),
        ("reference.py", raw.reference_sha256),
    ] {
        if let Some(value) = value {
            if !norm || artifacts.insert(name.into(), value).is_some() {
                return Err("foreign or duplicate RMS artifact identity".into());
            }
        } else if norm {
            return Err("RMS receipt lacks input/oracle/reference identity".into());
        }
    }
    if raw.local_source_sha256.is_some() && raw.source_sha256.is_some()
        || raw.linked_library_sha256.is_some() && raw.observed_linked_library_sha256.is_some()
    {
        return Err("contradictory duplicate source/library scopes".into());
    }
    let mut libraries = BTreeMap::new();
    for (path, hash) in raw
        .linked_library_sha256
        .or(raw.observed_linked_library_sha256)
        .unwrap_or_default()
    {
        let Some(name) = path.strip_prefix("/lib64/") else {
            return Err("unsupported library identity path".into());
        };
        if !["libc.so.6", "libm.so.6", "libgcc_s.so.1", "libstdc++.so.6"].contains(&name)
            || libraries.insert(name.into(), hash).is_some()
        {
            return Err("unsupported or duplicate linked library identity".into());
        }
    }
    let summary = OperatorSummary {
        original_receipt_sha256: hex::encode(Sha256::digest(json.as_bytes())),
        primary_revision: raw.primary_revision,
        primary_source_sha256: raw.primary_source_sha256,
        local_source_sha256: raw
            .local_source_sha256
            .or(raw.source_sha256)
            .ok_or("CPU receipt lacks local source identities")?,
        linked_library_sha256: libraries,
        compiler_version: raw.compiler_version,
        compiler_executable_sha256: raw.compiler_executable_sha256,
        compiler_flags: raw.compiler_flags,
        observations,
        sample: raw.sample,
        accumulator_fault: raw.accumulator_fault,
        mutation_differing_silu_words: raw.mutation_differing_silu_words,
    };
    let receipt: VerificationReceipt = serde_json::from_value(serde_json::json!({
        "schema_version": 1, "evidence": raw.evidence, "status": "passed", "checks": raw.checks,
        "excludes": raw.excludes, "artifact_sha256": artifacts, "operator_summary": summary,
        "tools": {}, "declared_toolchain_image": null,
    }))
    .map_err(|error| error.to_string())?;
    validate(&receipt)?;
    Ok(receipt)
}

pub(super) fn validate(receipt: &VerificationReceipt) -> Result<(), String> {
    let (checks, excludes) = contract(&receipt.evidence).ok_or("unsupported CPU evidence class")?;
    let summary = receipt
        .operator_summary
        .as_ref()
        .ok_or("CPU receipt lacks typed summary")?;
    // Shared envelopes admit only this scope, never an RTL/mapping success claim.
    let value = serde_json::to_value(receipt).map_err(|error| error.to_string())?;
    let allowed = [
        "schema_version",
        "evidence",
        "status",
        "checks",
        "excludes",
        "artifact_sha256",
        "operator_summary",
        "tools",
        "declared_toolchain_image",
    ];
    if value
        .as_object()
        .ok_or("invalid CPU receipt")?
        .iter()
        .any(|(name, value)| !value.is_null() && !allowed.contains(&name.as_str()))
        || receipt.schema_version != 1
        || receipt.status != ReceiptStatus::Passed
        || !receipt.tools.is_empty()
        || receipt.input_directory.is_some()
        || receipt.run_directory.is_some()
        || receipt.declared_toolchain_image.is_some()
        || receipt.checks.len() != checks.len()
        || checks
            .iter()
            .any(|name| receipt.checks.get(*name) != Some(&CheckStatus::Passed))
        || receipt.excludes.len() != excludes.len()
        || excludes.iter().any(|name| {
            receipt
                .excludes
                .iter()
                .filter(|value| value.as_str() == *name)
                .count()
                != 1
        })
        || !hashes(&receipt.artifact_sha256)
        || receipt.artifact_sha256.is_empty()
    {
        return Err("contradictory CPU scope, checks, exclusions or artifacts".into());
    }
    summary.validate(&receipt.evidence, &receipt.artifact_sha256)
}

impl OperatorSummary {
    fn validate(&self, evidence: &str, artifacts: &BTreeMap<String, String>) -> Result<(), String> {
        self.validate_identities(artifacts)?;
        if !matches!(self.observations, Observations::RmsNorm(_))
            && (self.sample.is_some() || self.accumulator_fault.is_some())
            || !matches!(self.observations, Observations::Silu(_))
                && self.mutation_differing_silu_words.is_some()
        {
            return Err("foreign CPU observation fields".into());
        }
        match (&self.observations, evidence) {
            (Observations::RmsNorm(cases), "bounded_fused_scalar_rms_norm") => {
                self.validate_norm(cases)
            }
            (Observations::Silu(cases), "bounded_scalar_silu_libm_characterization") => {
                self.validate_silu(cases)
            }
            (Observations::GatedDelta(cases), "bounded_scalar_gated_delta_state_transition") => {
                validate_delta(cases, artifacts)
            }
            (Observations::Softmax(cases), "bounded_scalar_masked_softmax_characterization") => {
                validate_softmax(cases, artifacts)
            }
            _ => Err("CPU evidence and observations disagree".into()),
        }
    }
    fn validate_identities(&self, artifacts: &BTreeMap<String, String>) -> Result<(), String> {
        self.validate_compiler()?;
        let (standard, source) = if matches!(self.observations, Observations::Silu(_)) {
            ("-std=c11", "oracle.c")
        } else {
            ("-std=c++17", "oracle.cpp")
        };
        let mut expected_flags = vec![standard, "-O0", "-fno-fast-math", "-ffp-contract=off"];
        if matches!(self.observations, Observations::Softmax(_)) {
            expected_flags.extend([
                "-U__AVX512F__",
                "-U__AVX512DQ__",
                "-U__AVX2__",
                "-U__FMA__",
                "-U__SSE2__",
                "-U__ARM_FEATURE_SVE",
                "-U__ARM_NEON",
                "-U__riscv_v_intrinsic",
            ]);
        }
        expected_flags.extend([source, "-lm", "-o", "oracle"]);
        if self
            .compiler_flags
            .iter()
            .map(String::as_str)
            .collect::<Vec<_>>()
            != expected_flags
        {
            return Err("CPU compiler flags contradict scalar numerical contract".into());
        }
        if !matches!(self.observations, Observations::RmsNorm(_)) {
            for name in [
                "oracle",
                source,
                "mutant",
                "mutant-output.f32",
                "compiler-version.log",
            ] {
                if !artifacts.contains_key(name) {
                    return Err(
                        "CPU observation lacks compiler/output/mutation artifact identity".into(),
                    );
                }
            }
        }
        for (name, hash) in &self.primary_source_sha256 {
            for key in [format!("primary-source/{name}"), format!("{name}.primary")] {
                if artifacts.get(&key).is_some_and(|value| value != hash) {
                    return Err("primary source and artifact identities disagree".into());
                }
            }
        }
        for (name, hash) in &self.local_source_sha256 {
            if artifacts
                .get(&format!("local-source/{name}.py"))
                .is_some_and(|value| value != hash)
            {
                return Err("local source and artifact identities disagree".into());
            }
        }
        Ok(())
    }
    fn validate_compiler(&self) -> Result<(), String> {
        if !digest(&self.original_receipt_sha256)
            || !digest(&self.compiler_executable_sha256)
            || self.primary_revision.len() != 40
            || !self
                .primary_revision
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || !hashes(&self.primary_source_sha256)
            || self.primary_source_sha256.is_empty()
            || !hashes(&self.local_source_sha256)
            || self.local_source_sha256.is_empty()
            || self.linked_library_sha256.len() > 4
            || self.linked_library_sha256.iter().any(|(name, hash)| {
                !["libc.so.6", "libm.so.6", "libgcc_s.so.1", "libstdc++.so.6"]
                    .contains(&name.as_str())
                    || !digest(hash)
            })
            || self.compiler_version.is_empty()
            || self.compiler_version.len() > 256
            || self
                .compiler_version
                .bytes()
                .any(|b| !(b.is_ascii_alphanumeric() || b" ().-+".contains(&b)))
            || self.compiler_flags.len() > 24
            || self.compiler_flags.iter().any(|flag| {
                self.compiler_flags
                    .iter()
                    .filter(|other| *other == flag)
                    .count()
                    != 1
            })
            || !self.compiler_flags.iter().all(|flag| {
                [
                    "-std=c11",
                    "-std=c++17",
                    "-O0",
                    "-fno-fast-math",
                    "-ffp-contract=off",
                    "-U__AVX512F__",
                    "-U__AVX512DQ__",
                    "-U__AVX2__",
                    "-U__FMA__",
                    "-U__SSE2__",
                    "-U__ARM_FEATURE_SVE",
                    "-U__ARM_NEON",
                    "-U__riscv_v_intrinsic",
                    "oracle.c",
                    "oracle.cpp",
                    "-lm",
                    "-o",
                    "oracle",
                ]
                .contains(&flag.as_str())
            })
            || !["-O0", "-fno-fast-math", "-ffp-contract=off"]
                .iter()
                .all(|flag| {
                    self.compiler_flags
                        .iter()
                        .filter(|value| value.as_str() == *flag)
                        .count()
                        == 1
                })
        {
            return Err("invalid CPU source/compiler identity".into());
        }
        Ok(())
    }
    fn validate_norm(&self, cases: &[NormComparison]) -> Result<(), String> {
        let sample = self.sample.as_ref().ok_or("RMS sample absent")?;
        let profiles = [
            "zero",
            "signed_zero",
            "signed_one",
            "min_subnormal",
            "varied_magnitudes",
            "sha_integer",
        ];
        let fault = self
            .accumulator_fault
            .as_ref()
            .ok_or("RMS mutation evidence absent")?;
        if cases.len() != profiles.len()
            || profiles
                .iter()
                .any(|profile| cases.iter().filter(|case| case.profile == *profile).count() != 1)
            || cases.iter().any(|case| {
                !digest(&case.c_output_sha256)
                    || case.fp32_comparisons != sample.tensor.elements
                    || case.mean_bits & 0x7f80_0000 == 0x7f80_0000
                    || case.scale_bits & 0x7f80_0000 == 0x7f80_0000
            })
            || sample.tensor.elements == 0
            || sample.tensor.elements > 65536
            || sample.tensor.shape_ggml_order != [sample.tensor.elements]
            || sample.tensor.storage != "F32"
            || !identity_name(&sample.tensor.name)
            || sample.tensor.elements.checked_mul(4) != Some(sample.tensor.payload_bytes)
            || sample.weight_bytes != sample.tensor.payload_bytes
            || sample.absolute_offset < sample.tensor.relative_offset
            || sample
                .absolute_offset
                .checked_add(sample.weight_bytes)
                .is_none()
            || ![
                &sample.header_sha256,
                &sample.sha256,
                &sample.whole_file_sha256,
                &fault.mutant_source_sha256,
                &fault.observed_output_sha256,
            ]
            .iter()
            .all(|hash| digest(hash))
            || sample.epsilon_bits == 0
            || sample.epsilon_bits >= 0x7f80_0000
            || fault.process_exit != 0
            || fault.differing_profiles.is_empty()
            || fault.differing_profiles.len() > 6
            || fault.differing_profiles.iter().any(|profile| {
                !profiles.contains(&profile.as_str())
                    || fault
                        .differing_profiles
                        .iter()
                        .filter(|value| *value == profile)
                        .count()
                        != 1
            })
        {
            return Err("contradictory RMS sample/comparison/mutation observation".into());
        }
        Ok(())
    }
    fn validate_silu(&self, cases: &SiluComparisons) -> Result<(), String> {
        if cases.finite_bf16_sourced_fp32_inputs != 65280
            || cases.total_inputs > 1_000_000
            || cases.total_inputs
                != cases
                    .finite_bf16_sourced_fp32_inputs
                    .checked_add(cases.additional_fp32_inputs)
                    .ok_or("SiLU count overflow")?
            || cases.silu_differences != 0
            || cases.max_silu_ulp != 0
            || !cases.difference_examples.is_empty()
            || cases.exp_differences > cases.total_inputs
            || cases.max_exp_ulp > u64::from(u32::MAX)
            || cases.exp_difference_examples.len() > 16
            || cases.exp_difference_examples.len() as u64 > cases.exp_differences
            || (cases.exp_differences == 0) != (cases.max_exp_ulp == 0)
            || cases.exp_difference_examples.iter().any(|case| {
                case.ulp == 0
                    || case.ulp > cases.max_exp_ulp
                    || u64::from(case.observed_exp_bits.abs_diff(case.reference_exp_bits))
                        != case.ulp
            })
            || self
                .mutation_differing_silu_words
                .is_none_or(|count| count == 0 || count > cases.total_inputs)
        {
            return Err("contradictory SiLU counts, differences or mutation observation".into());
        }
        Ok(())
    }
}

fn validate_delta(
    cases: &[DeltaComparison],
    artifacts: &BTreeMap<String, String>,
) -> Result<(), String> {
    if cases.is_empty()
        || cases.len() > 32
        || artifacts.keys().any(|name| {
            name.strip_prefix("case-")
                .and_then(|suffix| suffix.split_once('-'))
                .and_then(|(index, _)| index.parse::<usize>().ok())
                .is_some_and(|index| index >= cases.len())
        })
    {
        return Err("GDN case count or artifact index out of bounds".into());
    }
    for (index, case) in cases.iter().enumerate() {
        let config = &case.config;
        let attention = config
            .heads
            .checked_mul(config.tokens)
            .and_then(|n| n.checked_mul(config.width));
        let state = config
            .heads
            .checked_mul(config.width)
            .and_then(|n| n.checked_mul(config.width))
            .and_then(|n| n.checked_mul(config.slots));
        if [
            config.heads,
            config.key_heads,
            config.slots,
            config.tokens,
            config.width,
        ]
        .iter()
        .any(|n| *n == 0 || *n > 65536)
            || config.heads.checked_rem(config.key_heads) != Some(0)
            || attention != Some(case.attention_words)
            || state != Some(case.state_words)
            || case.state_words > 1_000_000
            || case.unwritten_slots.len() > 32
            || u64::try_from(case.unwritten_slots.len())
                != Ok(config.slots.saturating_sub(config.tokens))
            || case.unwritten_slots.iter().any(|slot| {
                *slot >= config.slots
                    || *slot < config.tokens
                    || case.unwritten_slots.iter().filter(|n| *n == slot).count() != 1
            })
            || !digest(&case.primary_sha256)
            || artifacts.get(&format!("case-{index}-reference.f32")) != Some(&case.primary_sha256)
            || !artifacts.contains_key(&format!("case-{index}-input.bin"))
            || artifacts.get(&format!("case-{index}-primary.f32")) != Some(&case.primary_sha256)
        {
            return Err("contradictory GDN dimensions, state or artifact identity".into());
        }
    }
    Ok(())
}
fn fraction(value: &str) -> bool {
    let parts = value.split('/').collect::<Vec<_>>();
    (1..=2).contains(&parts.len())
        && parts.iter().all(|part| {
            !part.is_empty() && part.len() <= 512 && part.bytes().all(|b| b.is_ascii_digit())
        })
        && parts
            .last()
            .is_some_and(|part| part.bytes().any(|b| b != b'0'))
}
fn validate_softmax(
    cases: &[SoftmaxComparison],
    artifacts: &BTreeMap<String, String>,
) -> Result<(), String> {
    if cases.is_empty() || cases.len() > 32 {
        return Err("softmax case count out of bounds".into());
    }
    for (index, case) in cases.iter().enumerate() {
        if !identity_name(&case.name)
            || cases.iter().filter(|other| other.name == case.name).count() != 1
            || case.width == 0
            || case.width > 65536 // allow-breakpoint: a reported tensor's bound, not a layout
            || case.masked >= case.width
            || case
                .width
                .checked_sub(case.masked)
                .is_none_or(|words| case.differing_words > words)
            || case.max_ulp > 3
            || (case.differing_words == 0) != (case.max_ulp == 0)
            || !fraction(&case.observed_probability_sum_fraction)
            || !digest(&case.primary_sha256)
            || !artifacts.contains_key(&format!("case-{index}-input.bin"))
            || artifacts
                .get(&format!("case-{index}-reference.f32"))
                .is_none_or(|hash| (hash == &case.primary_sha256) != (case.differing_words == 0))
            || artifacts.get(&format!("case-{index}-primary.f32")) != Some(&case.primary_sha256)
        {
            return Err("contradictory softmax dimensions, bounded differences or identity".into());
        }
    }
    Ok(())
}
