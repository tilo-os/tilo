//! What a destination admits: one expectation per sealed unit. The shared
//! fields and bounds are checked once here; each kind adds only its own.
use super::{Limits, Purpose};
use crate::setup::migration::changed;
use serde::{Deserialize, Serialize, de::DeserializeOwned};
use std::{fmt::Debug, io};

/// A kind of data carried as one sealed unit.
pub trait UnitKind: Clone + Debug + PartialEq + Eq + Serialize + DeserializeOwned {
    const PURPOSE: Purpose;
    /// The unit's file is `{PREFIX}-{operation}.sealed`.
    const PREFIX: &'static str;
    /// Refusal of an expectation that breaks any bound.
    const INVALID: &'static str;
    fn operation_valid(operation: &str) -> bool {
        crate::is_operation_id(operation)
    }
    /// Bounds of this kind beyond the shared ones.
    fn within_bounds(expectation: &Expectation<Self>) -> bool;
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields, bound = "")]
pub struct Expectation<K: UnitKind> {
    pub operation: String,
    pub filename: String,
    pub setup_binding: String,
    pub sealed_bytes: u64,
    pub plaintext_bytes: u64,
    pub entries: usize,
    pub ciphertext_sha256: [u8; 32],
    pub limits: Limits,
    pub kind: K,
}

impl<K: UnitKind> Expectation<K> {
    /// The unit's file name for `operation`.
    ///
    /// # Errors
    /// Refuses an operation this kind does not accept.
    pub fn filename(operation: &str) -> io::Result<String> {
        if !K::operation_valid(operation) {
            return Err(changed(K::INVALID));
        }
        Ok(format!("{}-{operation}.sealed", K::PREFIX))
    }
    pub fn purpose(&self) -> Purpose {
        K::PURPOSE
    }
    /// # Errors
    /// Refuses malformed operations, bindings, and sizes beyond the unit's bounds.
    pub fn validate(&self) -> io::Result<()> {
        if Self::filename(&self.operation).ok().as_deref() != Some(self.filename.as_str())
            || self.setup_binding.is_empty()
            || self.setup_binding.len() > 256
            || self.setup_binding.chars().any(char::is_control)
            || self.sealed_bytes == 0
            || self.plaintext_bytes == 0
            || self.plaintext_bytes > self.limits.plaintext_bytes
            || self.entries == 0
            || self.entries > self.limits.entries
            || self.limits.entries > 1_000_000
            || self.limits.metadata_bytes == 0
            || self.limits.metadata_bytes > 512 * 1024 * 1024
            || self
                .limits
                .sealed_bound()
                .is_none_or(|bound| self.sealed_bytes > bound)
            || !K::within_bounds(self)
        {
            return Err(changed(K::INVALID));
        }
        Ok(())
    }
}
