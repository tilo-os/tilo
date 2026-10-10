//! Closed governance JSON boundary: explicit nesting, number and
//! allocation limits before and during decoding.
use super::PeerResourceError;

#[derive(Clone, Copy)]
pub(crate) struct JsonLimits {
    pub(crate) allocation_bytes: usize,
    pub(crate) collection_entries: usize,
}

pub(crate) trait Schema: serde::de::DeserializeOwned + serde::Serialize {}
impl Schema for crate::state::GovernedIdentity {}
impl Schema for tilo_action_policy::Grant {}
impl Schema for tilo_action_policy::Role {}
impl Schema for crate::state::RedemptionReceipt {}

/// Decode one governance JSON value within `limits`.
pub(crate) fn decode<T: Schema>(bytes: &[u8], limits: JsonLimits) -> Result<T, PeerResourceError> {
    lexical_policy(bytes)?;
    super::guard::json(
        bytes,
        super::OuterLimits {
            allocation_bytes: limits.allocation_bytes,
            collection_entries: limits.collection_entries,
        },
    )
}

// Allocation-free refusal policy, not a second JSON grammar/schema decoder.
// Serde still verifies syntax, Unicode, field types and trailing input. Unknown
// extension values are scanned too. Quoted strings and signed bytes are intact.
fn lexical_policy(bytes: &[u8]) -> Result<(), PeerResourceError> {
    // `i` never exceeds `bytes.len()` + 1, so saturating steps are exact.
    let at = |i: usize| bytes.get(i).copied();
    let mut i = 0usize;
    let mut depth = 0usize;
    while let Some(byte) = at(i) {
        match byte {
            b'"' => {
                i = i.saturating_add(1);
                while let Some(byte) = at(i) {
                    match byte {
                        b'"' => {
                            i = i.saturating_add(1);
                            break;
                        }
                        b'\\' => i = i.saturating_add(2),
                        _ => i = i.saturating_add(1),
                    }
                }
            }
            b'{' | b'[' => {
                depth = depth.saturating_add(1);
                if depth > 64 {
                    return Err(PeerResourceError::Capacity);
                }
                i = i.saturating_add(1);
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i = i.saturating_add(1);
            }
            b'-' | b'0'..=b'9' => {
                let mut digits = 0usize;
                while let Some(byte @ (b'-' | b'.' | b'0'..=b'9')) = at(i) {
                    if byte.is_ascii_digit() {
                        digits = digits.saturating_add(1);
                        if digits > 20 {
                            return Err(PeerResourceError::Capacity);
                        }
                    }
                    i = i.saturating_add(1);
                }
                if matches!(at(i), Some(b'e' | b'E')) {
                    i = i.saturating_add(1);
                    if matches!(at(i), Some(b'+' | b'-')) {
                        i = i.saturating_add(1);
                    }
                    let mut exponent = 0u32;
                    while let Some(byte @ b'0'..=b'9') = at(i) {
                        // At most 64 before this step, so neither operation saturates.
                        exponent = exponent
                            .saturating_mul(10)
                            .saturating_add(u32::from(byte.saturating_sub(b'0')));
                        if exponent > 64 {
                            return Err(PeerResourceError::Capacity);
                        }
                        i = i.saturating_add(1);
                    }
                }
            }
            _ => i = i.saturating_add(1),
        }
    }
    Ok(())
}
#[cfg(test)]
mod tests;
