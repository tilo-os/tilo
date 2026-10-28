//! Bounded import of analytical model-storage reports, never chip forecasts.

use serde::{Deserialize, Serialize};

pub const MAX_REPORT_BYTES: usize = 1_048_576;
pub const MAX_REPORTS: usize = 128;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Assumptions {
    pub logical_parameters: u64,
    pub weight_bits: u64,
    pub kv_bits: u64,
    pub context_tokens: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum AttentionCache {
    PartialBound {
        full_attention_layers: u64,
        kv_elements: u64,
        packed_bytes: u64,
        excludes: Vec<String>,
    },
    Unsupported {
        reason: String,
    },
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageReport {
    pub schema_version: u64,
    pub model: String,
    pub revision: String,
    pub config_sha256: String,
    pub evidence: String,
    pub batch_size: u64,
    pub assumptions: Assumptions,
    pub packed_weight_bytes: u64,
    pub full_attention_cache: AttentionCache,
    pub not_estimated: Vec<String>,
    pub limitations: Vec<String>,
}

fn hexadecimal(value: &str, length: usize) -> bool {
    value.len() == length
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn repository(value: &str) -> bool {
    let parts: Vec<_> = value.split('/').collect();
    parts.len() == 2
        && parts.iter().all(|part| {
            !part.is_empty()
                && part
                    .bytes()
                    .all(|byte| byte.is_ascii_alphanumeric() || b"_.-".contains(&byte))
        })
}

fn packed_bytes(elements: u64, bits: u64) -> Result<u64, String> {
    let total = u128::from(elements)
        .checked_mul(u128::from(bits))
        .ok_or_else(|| "packed storage multiplication overflow".to_owned())?;
    u64::try_from(total.div_ceil(8)).map_err(|_| "packed storage exceeds supported range".into())
}

impl StorageReport {
    /// Check internal structure and arithmetic. This does not authenticate the
    /// publisher, reconstruct the config, or establish quantization quality.
    pub fn parse(json: &str) -> Result<Self, String> {
        if json.len() > MAX_REPORT_BYTES {
            return Err("report exceeds the 1 MiB import limit".into());
        }
        let report: Self = serde_json::from_str(json).map_err(|error| error.to_string())?;
        report.validate()?;
        Ok(report)
    }

    fn validate(&self) -> Result<(), String> {
        if self.schema_version != 1
            || self.evidence != "analytical_storage_bound"
            || self.batch_size != 1
        {
            return Err("unsupported report schema, evidence class, or batch size".into());
        }
        if !repository(&self.model)
            || self.model.len() > 256
            || !hexadecimal(&self.revision, 40)
            || !hexadecimal(&self.config_sha256, 64)
        {
            return Err("report needs a repository, immutable revision, and config digest".into());
        }
        if self.limitations.len() > 32
            || self.not_estimated.len() > 32
            || self
                .limitations
                .iter()
                .chain(&self.not_estimated)
                .any(|item| item.len() > 4096)
        {
            return Err("report annotations exceed supported limits".into());
        }
        let assumptions = &self.assumptions;
        if assumptions.logical_parameters == 0
            || assumptions.context_tokens == 0
            || !(1..=32).contains(&assumptions.weight_bits)
            || !(1..=32).contains(&assumptions.kv_bits)
        {
            return Err("invalid parameter count, context, or assumed precision".into());
        }
        if packed_bytes(assumptions.logical_parameters, assumptions.weight_bits)?
            != self.packed_weight_bytes
        {
            return Err("weight storage disagrees with the stated assumptions".into());
        }
        match &self.full_attention_cache {
            AttentionCache::PartialBound {
                kv_elements,
                packed_bytes: bytes,
                excludes,
                ..
            } => {
                if packed_bytes(*kv_elements, assumptions.kv_bits)? != *bytes
                    || excludes.is_empty()
                    || excludes.len() > 32
                    || excludes.iter().any(|item| item.len() > 4096)
                {
                    return Err("cache storage arithmetic or exclusions are invalid".into());
                }
            }
            AttentionCache::Unsupported { reason }
                if reason.trim().is_empty() || reason.len() > 4096 =>
            {
                return Err("unsupported cache needs an explanation".into());
            }
            AttentionCache::Unsupported { .. } => {}
        }
        for required in [
            "silicon_area",
            "power",
            "tokens_per_second",
            "coding_quality",
            "total_runtime_memory",
        ] {
            if !self.not_estimated.iter().any(|item| item == required) {
                return Err(format!("report must declare {required} unestimated"));
            }
        }
        if self.limitations.is_empty() {
            return Err("report must retain its limitations".into());
        }
        Ok(())
    }
}

/// Content identity of the original report bytes; never an authentication claim.
pub fn report_id(json: &str) -> String {
    blake3::hash(json.as_bytes()).to_hex().to_string()
}
