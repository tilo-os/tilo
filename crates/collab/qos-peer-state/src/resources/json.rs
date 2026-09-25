//! Closed governance JSON boundary. No native allocation qualification.
mod guard;

use super::PeerResourceError;
use automerge::resource_limits::{NativeReservation, NativeWorkTurn};
use std::io::Write;

#[derive(Clone, Copy)]
pub(crate) struct JsonLimits {
    pub(crate) allocation_bytes: usize,
    pub(crate) collection_entries: usize,
}

pub(crate) trait Schema: serde::de::DeserializeOwned + serde::Serialize {}
impl Schema for crate::state::GovernedIdentity {}
impl Schema for collab_types::caps::CapOverride {}
impl Schema for crate::state::RedemptionReceipt {}

pub(crate) struct JsonHeld<T> {
    value: T,
    _reservation: NativeReservation,
}
impl<T: Schema> JsonHeld<T> {
    pub(crate) fn decode(
        bytes: &[u8],
        limits: JsonLimits,
        turn: &NativeWorkTurn,
    ) -> Result<Self, PeerResourceError> {
        // All parser allocation is covered before constructing its scratch/error
        // state. NativeWorkTurn spends this turn's real allowance, not residency.
        let _parser = turn
            .reserve(parser_bytes(bytes.len())?)
            .map_err(|_| PeerResourceError::Capacity)?;
        lexical_policy(bytes)?;
        let reservation = turn
            .reserve(limits.allocation_bytes)
            .map_err(|_| PeerResourceError::Capacity)?;
        let value = guard::decode(bytes, limits).map_err(|error| match error {
            guard::GuardError::Capacity => PeerResourceError::Capacity,
            guard::GuardError::Malformed => PeerResourceError::Malformed,
        })?;
        Ok(Self {
            value,
            _reservation: reservation,
        })
    }
    pub(crate) fn value(&self) -> &T {
        &self.value
    }
    pub(crate) fn encode(
        &self,
        maximum: usize,
        turn: &NativeWorkTurn,
    ) -> Result<JsonOutput, PeerResourceError> {
        // Known Serialize implementations borrow fields; UUID uses its stack
        // buffer. Only controlled writer errors can allocate an ErrorImpl.
        let _errors = turn
            .reserve(error_boxes())
            .map_err(|_| PeerResourceError::Capacity)?;
        let mut count = Count { length: 0, maximum };
        serde_json::to_writer(&mut count, &self.value).map_err(|_| PeerResourceError::Capacity)?;
        let reservation = turn
            .reserve(count.length)
            .map_err(|_| PeerResourceError::Capacity)?;
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(count.length)
            .map_err(|_| PeerResourceError::Capacity)?;
        bytes.resize(count.length, 0);
        let mut sink = bytes.as_mut_slice();
        serde_json::to_writer(&mut sink, &self.value).map_err(|_| PeerResourceError::Malformed)?;
        if !sink.is_empty() {
            return Err(PeerResourceError::Malformed);
        }
        Ok(JsonOutput {
            bytes,
            _reservation: reservation,
        })
    }
}
pub(crate) struct JsonOutput {
    bytes: Vec<u8>,
    _reservation: NativeReservation,
}
impl JsonOutput {
    pub(crate) fn as_bytes(&self) -> &[u8] {
        &self.bytes
    }
}
// Pinned serde_json1.0.151 ErrorImpl contains ErrorCode plus two positions.
// ErrorCode's largest payload is bounded by its Box<str>/io::Error and word
// padding/discriminant. fix_position may temporarily overlap two boxes.
fn error_boxes() -> usize {
    2 * (size_of::<Box<str>>() + size_of::<std::io::Error>() + 6 * size_of::<usize>())
}
fn parser_bytes(n: usize) -> Result<usize, PeerResourceError> {
    let capacity = || PeerResourceError::Capacity;
    // SliceRead escaped scratch requests at most n+4 (WTF8's reserve4).
    // Numeric String requests at most n+1, initial16. Vec/String geometric
    // growth may overlap the old and new backing: three times requested size.
    let scratch = n
        .checked_add(4)
        .and_then(|n| n.max(8).checked_mul(3))
        .ok_or_else(capacity)?;
    let numeric = n
        .checked_add(1)
        .and_then(|n| n.max(16).checked_mul(3))
        .ok_or_else(capacity)?;
    // Closed GuardError suppresses schema error formatting. Parser Debug of an
    // unexpected string uses at most10 bytes per input byte plus fixed framing.
    let diagnostic = n
        .checked_mul(10)
        .and_then(|n| n.checked_add(128))
        .and_then(|n| n.checked_mul(3))
        .ok_or_else(capacity)?;
    // Known cert/revocation/signature visitors format this fixed prefix plus a
    // decimal usize length BEFORE calling Error::custom. Count that allocation.
    let signature = 3 * ("expected 64-byte signature, got ".len() + usize::BITS as usize);
    // lexical pow5 n<=149 chooses small multiplication (table5^128<=13
    // 32-bit limbs, initial x<=3, branch sum16<64). Each multiply's shorter
    // operand has exactly ONE limb, below Karatsuba32. Only later pow2 shifts
    // reach55 limbs. Two Vecs, <=8 bytes/limb, old/new growth overlap3.
    let floating = 2 * 3 * 55 * 8;
    scratch
        .checked_add(numeric)
        .and_then(|n| n.checked_add(diagnostic))
        .and_then(|n| n.checked_add(signature))
        .and_then(|n| n.checked_add(floating))
        .and_then(|n| n.checked_add(error_boxes()))
        .ok_or_else(capacity)
}

// Allocation-free refusal policy, not a second JSON grammar/schema decoder.
// Serde still verifies syntax, Unicode, field types and trailing input. Unknown
// extension values are scanned too. Quoted strings and signed bytes are intact.
fn lexical_policy(bytes: &[u8]) -> Result<(), PeerResourceError> {
    let mut i = 0;
    let mut depth = 0usize;
    while i < bytes.len() {
        match bytes[i] {
            b'"' => {
                i += 1;
                while i < bytes.len() {
                    match bytes[i] {
                        b'"' => {
                            i += 1;
                            break;
                        }
                        b'\\' => {
                            i = i.saturating_add(2);
                        }
                        _ => i += 1,
                    }
                }
            }
            b'{' | b'[' => {
                depth += 1;
                if depth > 64 {
                    return Err(PeerResourceError::Capacity);
                }
                i += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                i += 1;
            }
            b'-' | b'0'..=b'9' => {
                let mut digits = 0usize;
                while i < bytes.len() && matches!(bytes[i], b'-' | b'.' | b'0'..=b'9') {
                    if bytes[i].is_ascii_digit() {
                        digits += 1;
                        if digits > 20 {
                            return Err(PeerResourceError::Capacity);
                        }
                    }
                    i += 1;
                }
                if i < bytes.len() && matches!(bytes[i], b'e' | b'E') {
                    i += 1;
                    if i < bytes.len() && matches!(bytes[i], b'+' | b'-') {
                        i += 1;
                    }
                    let mut exponent = 0u32;
                    while i < bytes.len() && bytes[i].is_ascii_digit() {
                        exponent = exponent * 10 + u32::from(bytes[i] - b'0');
                        if exponent > 64 {
                            return Err(PeerResourceError::Capacity);
                        }
                        i += 1;
                    }
                }
            }
            _ => i += 1,
        }
    }
    Ok(())
}
struct Count {
    length: usize,
    maximum: usize,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.length = self
            .length
            .checked_add(bytes.len())
            .filter(|n| *n <= self.maximum)
            .ok_or(std::io::ErrorKind::WriteZero)?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests;
