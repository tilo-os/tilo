//! Full streaming lifecycle grammar. This yields inert facts, never a receipt.
use super::OutboxFailure;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Lifecycle {
    Accepted,
    Progress,
    Item,
    Succeeded,
    Failed,
    Cancelled,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Facts {
    pub lifecycle: Lifecycle,
    pub sequence: Option<u64>,
    pub consumable: bool,
}
// The visitor retains no strings/trees; all its diagnostic messages are fixed.
// Streaming scratch is bounded by the MAX_OBSERVATION input limit.

use serde::de::{DeserializeSeed, Error, MapAccess, SeqAccess, Visitor};
use std::fmt;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Update,
    Output,
    Failure,
    Resources,
    Json,
    Text,
    NullableText,
    Number,
    NullableNumber,
    Bool,
    Key,
    State,
    Invocation,
    Code,
}
#[derive(Clone, Copy, PartialEq, Eq)]
enum Special {
    Normal,
    Unknown,
    PayloadUnavailable,
}
#[derive(Clone, Copy)]
struct Parsed {
    lifecycle: Lifecycle,
    invocation: [u8; 16],
    sequence: Option<u64>,
    special: Special,
}
#[derive(Clone, Copy)]
enum Value {
    None,
    Number(u64),
    Key(u8),
    State(Lifecycle),
    Invocation([u8; 16]),
    Code(Special),
    Update(Parsed),
}
fn invalid<E: Error>() -> E {
    E::custom("invalid managed invocation observation")
}
impl<'de> DeserializeSeed<'de> for Kind {
    type Value = Value;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<Value, D::Error> {
        d.deserialize_any(self)
    }
}
impl<'de> Visitor<'de> for Kind {
    type Value = Value;
    fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("managed invocation observation")
    }
    fn visit_unit<E: Error>(self) -> Result<Value, E> {
        if matches!(self, Kind::Json | Kind::NullableText | Kind::NullableNumber) {
            Ok(Value::None)
        } else {
            Err(invalid())
        }
    }
    fn visit_bool<E: Error>(self, _: bool) -> Result<Value, E> {
        if matches!(self, Kind::Json | Kind::Bool) {
            Ok(Value::None)
        } else {
            Err(invalid())
        }
    }
    fn visit_i64<E: Error>(self, n: i64) -> Result<Value, E> {
        if self == Kind::Json {
            Ok(Value::None)
        } else if n >= 0 {
            self.visit_u64(n as u64)
        } else {
            Err(invalid())
        }
    }
    fn visit_u64<E: Error>(self, n: u64) -> Result<Value, E> {
        match self {
            Kind::Json => Ok(Value::None),
            Kind::Number | Kind::NullableNumber => Ok(Value::Number(n)),
            _ => Err(invalid()),
        }
    }
    fn visit_f64<E: Error>(self, _: f64) -> Result<Value, E> {
        if self == Kind::Json {
            Ok(Value::None)
        } else {
            Err(invalid())
        }
    }
    fn visit_str<E: Error>(self, s: &str) -> Result<Value, E> {
        match self {
            Kind::Json | Kind::Text | Kind::NullableText => Ok(Value::None),
            Kind::Key => Ok(Value::Key(match s {
                "state" => 0,
                "invocation" => 1,
                "sequence" => 2,
                "phase" => 3,
                "completed" => 4,
                "total" => 5,
                "message" => 6,
                "value" => 7,
                "output" => 8,
                "error" => 9,
                "resources" => 10,
                "code" => 11,
                "details" => 12,
                "retryable" => 13,
                _ => return Err(invalid()),
            })),
            Kind::State => Ok(Value::State(match s {
                "accepted" => Lifecycle::Accepted,
                "progress" => Lifecycle::Progress,
                "item" => Lifecycle::Item,
                "succeeded" => Lifecycle::Succeeded,
                "failed" => Lifecycle::Failed,
                "cancelled" => Lifecycle::Cancelled,
                _ => return Err(invalid()),
            })),
            Kind::Invocation => {
                let id = uuid::Uuid::parse_str(s).map_err(|_| invalid())?;
                let mut canonical = [0; 36];
                if id.is_nil() || id.hyphenated().encode_lower(&mut canonical) != s {
                    return Err(invalid());
                }
                Ok(Value::Invocation(*id.as_bytes()))
            }
            Kind::Code => Ok(Value::Code(match s {
                "action_outcome_unknown" => Special::Unknown,
                "action_read_payload_unavailable" => Special::PayloadUnavailable,
                _ => Special::Normal,
            })),
            _ => Err(invalid()),
        }
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<Value, A::Error> {
        let item = match self {
            Kind::Json => Kind::Json,
            Kind::Resources => Kind::Text,
            _ => return Err(invalid()),
        };
        while seq.next_element_seed(item)?.is_some() {}
        Ok(Value::None)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Value, A::Error> {
        if self == Kind::Json {
            while map.next_key_seed(Kind::Text)?.is_some() {
                map.next_value_seed(Kind::Json)?;
            }
            return Ok(Value::None);
        }
        if !matches!(self, Kind::Update | Kind::Output | Kind::Failure) {
            return Err(invalid());
        }
        let mut seen = 0u16;
        let mut state = None;
        let mut invocation = None;
        let mut sequence = None;
        let mut special = Special::Normal;
        while let Some(key) = map.next_key_seed(Kind::Key)? {
            let Value::Key(key) = key else {
                return Err(invalid());
            };
            let bit = 1u16 << key;
            if seen & bit != 0 {
                return Err(invalid());
            }
            seen |= bit;
            let kind = match (self, key) {
                (Kind::Update, 0) => Kind::State,
                (Kind::Update, 1) => Kind::Invocation,
                (Kind::Update, 2) => Kind::Number,
                (Kind::Update, 3) | (Kind::Failure, 6) => Kind::Text,
                (Kind::Update, 4 | 5) => Kind::NullableNumber,
                (Kind::Update, 6) => Kind::NullableText,
                (Kind::Update | Kind::Output, 7) | (Kind::Failure, 12) => Kind::Json,
                (Kind::Update, 8) => Kind::Output,
                (Kind::Update, 9) => Kind::Failure,
                (Kind::Output, 10) => Kind::Resources,
                (Kind::Failure, 11) => Kind::Code,
                (Kind::Failure, 13) => Kind::Bool,
                _ => return Err(invalid()),
            };
            match map.next_value_seed(kind)? {
                Value::State(value) => state = Some(value),
                Value::Invocation(value) => invocation = Some(value),
                Value::Number(value) if key == 2 => sequence = Some(value),
                Value::Code(value) => special = value,
                _ => {}
            }
        }
        match self {
            Kind::Output => Ok(Value::None),
            Kind::Failure => {
                if seen & ((1 << 11) | (1 << 6)) != ((1 << 11) | (1 << 6)) {
                    return Err(invalid());
                }
                Ok(Value::Code(special))
            }
            Kind::Update => {
                let lifecycle = state.ok_or_else(invalid)?;
                let invocation = invocation.ok_or_else(invalid)?;
                let (required, allowed) = match lifecycle {
                    Lifecycle::Accepted => (3, 3),
                    Lifecycle::Progress => (
                        3 | (1 << 2) | (1 << 3),
                        3 | (1 << 2) | (1 << 3) | (1 << 4) | (1 << 5) | (1 << 6),
                    ),
                    Lifecycle::Item => (3 | (1 << 2) | (1 << 7), 3 | (1 << 2) | (1 << 7)),
                    Lifecycle::Succeeded => (3 | (1 << 8), 3 | (1 << 8)),
                    Lifecycle::Failed => (3 | (1 << 9), 3 | (1 << 9)),
                    Lifecycle::Cancelled => (3, 3 | (1 << 6)),
                };
                if seen & required != required || seen & !allowed != 0 {
                    return Err(invalid());
                }
                Ok(Value::Update(Parsed {
                    lifecycle,
                    invocation,
                    sequence,
                    special,
                }))
            }
            _ => Err(invalid()),
        }
    }
}

pub(super) fn validate(
    json: &str,
    invocation: &[u8; 16],
    uncertain: bool,
    _cancellation_requested: bool,
) -> Result<Facts, OutboxFailure> {
    if json.len() > super::codec::MAX_OBSERVATION {
        return Err(OutboxFailure::Capacity);
    }
    // Deserializer and every error are dropped before returning.
    let parsed = {
        let mut decoder = serde_json::Deserializer::from_str(json);
        let parsed = Kind::Update
            .deserialize(&mut decoder)
            .map_err(|_| OutboxFailure::Invalid)?;
        decoder.end().map_err(|_| OutboxFailure::Invalid)?;
        parsed
    };
    let Value::Update(parsed) = parsed else {
        return Err(OutboxFailure::Invalid);
    };
    if &parsed.invocation != invocation || uncertain != (parsed.special == Special::Unknown) {
        return Err(OutboxFailure::Invalid);
    }
    Ok(Facts {
        lifecycle: parsed.lifecycle,
        sequence: parsed.sequence,
        consumable: matches!(
            parsed.lifecycle,
            Lifecycle::Succeeded | Lifecycle::Failed | Lifecycle::Cancelled
        ) && parsed.special == Special::Normal
            && !uncertain,
    })
}

#[cfg(test)]
mod tests;
