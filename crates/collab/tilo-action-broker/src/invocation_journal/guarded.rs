//! Inert private storage only. Nothing here constructs live disclosure authority.
use super::{
    Deserialize, DurableInvocation, InvocationId, InvocationJournal, InvocationKey,
    InvocationUpdate, Read, Serialize,
};
use serde::de::{DeserializeSeed, IgnoredAny, MapAccess, SeqAccess, Visitor};
use serde::ser::SerializeStruct;
use std::{cell::Cell, fmt, io::BufReader, rc::Rc};

pub(super) const MAX_UPDATE: u64 = 1024;
const MAX_REQUIREMENT: u64 = 256;
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Digest(pub [u8; 32]);
impl Serialize for Digest {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_str(&hex::encode(self.0))
    }
}
impl<'de> Deserialize<'de> for Digest {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct Hex;
        impl Visitor<'_> for Hex {
            type Value = Digest;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("64 lowercase hex bytes")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Digest, E> {
                if s.len() != 64 {
                    return Err(E::custom("invalid guarded digest length"));
                }
                if !s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f')) {
                    return Err(E::custom("noncanonical guarded digest"));
                }
                let mut out = [0; 32];
                hex::decode_to_slice(s, &mut out)
                    .map_err(|_| E::custom("noncanonical guarded digest"))?;
                Ok(Digest(out))
            }
        }
        d.deserialize_str(Hex)
    }
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Requirement {
    pub version: u8,
    pub selection_digest: Digest,
}
fn canonical_invocation<'de, D: serde::Deserializer<'de>>(d: D) -> Result<InvocationId, D::Error> {
    struct Canonical;
    impl Visitor<'_> for Canonical {
        type Value = InvocationId;
        fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
            f.write_str("canonical nonnil invocation UUID")
        }
        fn visit_str<E: serde::de::Error>(self, value: &str) -> Result<Self::Value, E> {
            let id = uuid::Uuid::parse_str(value).map_err(E::custom)?;
            let mut bytes = [0; 36];
            if id.is_nil() || value != id.hyphenated().encode_lower(&mut bytes) {
                return Err(E::custom("noncanonical guarded invocation"));
            }
            Ok(InvocationId(id))
        }
    }
    d.deserialize_str(Canonical)
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case", deny_unknown_fields)]
enum GuardedUpdate {
    Accepted {
        #[serde(deserialize_with = "canonical_invocation")]
        invocation: InvocationId,
    },
    Failed {
        #[serde(deserialize_with = "canonical_invocation")]
        invocation: InvocationId,
        error: FixedFailure,
    },
    GuardedReadSucceeded {
        #[serde(deserialize_with = "canonical_invocation")]
        invocation: InvocationId,
        selection_digest: Digest,
        output_digest: Digest,
        basis_digest: Digest,
        output_bytes: u32,
    },
}
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct FixedFailure {
    code: String,
    message: String,
    retryable: bool,
}
fn fixed_failure(code: &str, message: &str, retryable: bool) -> bool {
    !retryable
        && matches!(
            (code, message),
            (
                "action_read_unavailable",
                "The document read is unavailable."
            ) | (
                "action_read_request_invalid",
                "The document read request is invalid."
            ) | (
                "action_read_limit",
                "The document read exceeded its resource limit."
            ) | (
                "action_outcome_unknown",
                "The application outcome is unknown; this invocation will not be executed again."
            )
        )
}
pub(super) fn allowed_failure(update: &InvocationUpdate, uncertain: bool) -> bool {
    matches!(update, InvocationUpdate::Failed { error, .. }
        if error.details.is_none() && fixed_failure(&error.code, &error.message, error.retryable)
        && uncertain == (error.code == "action_outcome_unknown"))
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct State {
    requirement: Requirement,
    terminal: Option<GuardedUpdate>,
}
#[cfg(test)]
impl State {
    pub(super) fn selected(selection: [u8; 32]) -> Self {
        Self {
            requirement: Requirement {
                version: 1,
                selection_digest: Digest(selection),
            },
            terminal: None,
        }
    }
    pub(super) fn selection(&self) -> &[u8; 32] {
        &self.requirement.selection_digest.0
    }
    pub(super) fn complete(
        &mut self,
        invocation: InvocationId,
        output: [u8; 32],
        basis: [u8; 32],
        len: u32,
    ) -> Result<(), String> {
        if !(1..=65536).contains(&len) || self.terminal.is_some() {
            return Err("invalid guarded terminal transition".into());
        }
        self.terminal = Some(GuardedUpdate::GuardedReadSucceeded {
            invocation,
            selection_digest: self.requirement.selection_digest.clone(),
            output_digest: Digest(output),
            basis_digest: Digest(basis),
            output_bytes: len,
        });
        Ok(())
    }
}
pub(super) fn unavailable(invocation: InvocationId) -> InvocationUpdate {
    InvocationUpdate::Failed{invocation,error:tilo_actions::ActionFailure{code:"action_read_payload_unavailable".into(),message:"The retained read payload is unavailable; this invocation will not be executed again.".into(),details:None,retryable:false}}
}
fn present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Requirement>, D::Error> {
    // The field is absent only through #[serde(default)]. Explicit null fails.
    Requirement::deserialize(d).map(Some)
}
// Capture only update-level and public typed output/error duplicate fields.
// Arbitrary JSON values retain serde_json's existing map semantics. The first
// pass has already bounded guarded update bytes before this owned capture.
pub(super) struct RawUpdate {
    pub(super) value: serde_json::Value,
    duplicate: Vec<String>,
    error_duplicate: Vec<String>,
    output_duplicate: Vec<String>,
}
struct Shallow {
    value: serde_json::Value,
    duplicate: Vec<String>,
}
impl<'de> Deserialize<'de> for Shallow {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = Shallow;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("JSON value")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<Shallow, A::Error> {
                let mut out = serde_json::Map::new();
                let mut duplicate = vec![];
                while let Some(k) = m.next_key::<String>()? {
                    let v = m.next_value()?;
                    if out.contains_key(&k) {
                        duplicate.push(k.clone());
                    }
                    out.insert(k, v);
                }
                Ok(Shallow {
                    value: serde_json::Value::Object(out),
                    duplicate,
                })
            }
            fn visit_seq<A: SeqAccess<'de>>(self, mut a: A) -> Result<Shallow, A::Error> {
                let mut out = vec![];
                while let Some(v) = a.next_element()? {
                    out.push(v);
                }
                Ok(Shallow {
                    value: serde_json::Value::Array(out),
                    duplicate: vec![],
                })
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: s.into(),
                    duplicate: vec![],
                })
            }
            fn visit_string<E: serde::de::Error>(self, s: String) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: s.into(),
                    duplicate: vec![],
                })
            }
            fn visit_bool<E: serde::de::Error>(self, b: bool) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: b.into(),
                    duplicate: vec![],
                })
            }
            fn visit_u64<E: serde::de::Error>(self, n: u64) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: n.into(),
                    duplicate: vec![],
                })
            }
            fn visit_i64<E: serde::de::Error>(self, n: i64) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: n.into(),
                    duplicate: vec![],
                })
            }
            fn visit_f64<E: serde::de::Error>(self, n: f64) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: serde_json::Number::from_f64(n)
                        .map(serde_json::Value::Number)
                        .unwrap_or_default(),
                    duplicate: vec![],
                })
            }
            fn visit_unit<E: serde::de::Error>(self) -> Result<Shallow, E> {
                Ok(Shallow {
                    value: serde_json::Value::Null,
                    duplicate: vec![],
                })
            }
        }
        d.deserialize_any(V)
    }
}
impl<'de> Deserialize<'de> for RawUpdate {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct V;
        impl<'de> Visitor<'de> for V {
            type Value = RawUpdate;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("invocation update object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut m: A) -> Result<RawUpdate, A::Error> {
                let mut out = serde_json::Map::new();
                let mut duplicate = vec![];
                let mut error_duplicate = vec![];
                let mut output_duplicate = vec![];
                while let Some(k) = m.next_key::<String>()? {
                    let v = if k == "error" || k == "output" {
                        let v = m.next_value::<Shallow>()?;
                        if k == "error" {
                            error_duplicate = v.duplicate;
                        } else {
                            output_duplicate = v.duplicate;
                        }
                        v.value
                    } else {
                        m.next_value()?
                    };
                    if out.contains_key(&k) {
                        duplicate.push(k.clone());
                    }
                    out.insert(k, v);
                }
                Ok(RawUpdate {
                    value: serde_json::Value::Object(out),
                    duplicate,
                    error_duplicate,
                    output_duplicate,
                })
            }
        }
        d.deserialize_map(V)
    }
}
impl RawUpdate {
    pub(super) fn check_duplicates(&self, guarded: bool) -> Result<(), String> {
        let state = self.value.get("state").and_then(serde_json::Value::as_str);
        let fields: &[&str] = match state {
            Some("accepted") => &["state", "invocation"],
            Some("progress") => &[
                "state",
                "invocation",
                "sequence",
                "phase",
                "completed",
                "total",
                "message",
            ],
            Some("item") => &["state", "invocation", "sequence", "value"],
            Some("succeeded") => &["state", "invocation", "output"],
            Some("failed") => &["state", "invocation", "error"],
            Some("cancelled") => &["state", "invocation", "message"],
            _ => &["state"],
        };
        if self
            .duplicate
            .iter()
            .any(|key| guarded || fields.contains(&key.as_str()))
            || (state == Some("failed")
                && self.error_duplicate.iter().any(|key| {
                    guarded || ["code", "message", "details", "retryable"].contains(&key.as_str())
                }))
            || (state == Some("succeeded")
                && self
                    .output_duplicate
                    .iter()
                    .any(|key| ["value", "resources"].contains(&key.as_str())))
        {
            return Err("duplicate invocation update field".into());
        }
        Ok(())
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Row {
    key: InvocationKey,
    intent: serde_json::Value,
    wire_id: u64,
    update: RawUpdate,
    uncertain: bool,
    cancellation_requested: bool,
    #[serde(default, deserialize_with = "present")]
    guarded_read: Option<Requirement>,
    #[serde(default, deserialize_with = "prepared_present")]
    prepared_effect: Option<super::PreparedEffect>,
    #[serde(default, deserialize_with = "digest_present")]
    request_digest: Option<Digest>,
    #[serde(default)]
    admission: Option<crate::action_evidence::AdmissionEvidence>,
    #[serde(default)]
    terminal: Option<crate::action_evidence::TerminalReceipt>,
}
// Like the other optional members, absent only through #[serde(default)].
fn digest_present<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<Digest>, D::Error> {
    Digest::deserialize(d).map(Some)
}
fn prepared_present<'de, D: serde::Deserializer<'de>>(
    d: D,
) -> Result<Option<super::PreparedEffect>, D::Error> {
    super::PreparedEffect::deserialize(d).map(Some)
}
impl<'de> Deserialize<'de> for DurableInvocation {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        use serde::de::Error;
        let row = Row::deserialize(d)?;
        row.update
            .check_duplicates(row.guarded_read.is_some())
            .map_err(D::Error::custom)?;
        let (update, guarded) = if let Some(requirement) = row.guarded_read {
            if row.cancellation_requested {
                return Err(D::Error::custom("guarded reads cannot be cancelled"));
            }
            if requirement.version != 1 {
                return Err(D::Error::custom("guarded requirement version unsupported"));
            }
            let stored: GuardedUpdate =
                serde_json::from_value(row.update.value).map_err(D::Error::custom)?;
            let (update, terminal) = match &stored {
                GuardedUpdate::Accepted { invocation } => (
                    InvocationUpdate::Accepted {
                        invocation: *invocation,
                    },
                    None,
                ),
                GuardedUpdate::Failed { invocation, error } => {
                    if !fixed_failure(&error.code, &error.message, error.retryable) {
                        return Err(D::Error::custom("guarded failure is not sanitized"));
                    }
                    (
                        InvocationUpdate::Failed {
                            invocation: *invocation,
                            error: tilo_actions::ActionFailure {
                                code: error.code.clone(),
                                message: error.message.clone(),
                                details: None,
                                retryable: false,
                            },
                        },
                        None,
                    )
                }
                GuardedUpdate::GuardedReadSucceeded {
                    invocation,
                    selection_digest,
                    output_bytes,
                    ..
                } => {
                    if row.uncertain
                        || !(1..=65536).contains(output_bytes)
                        || selection_digest != &requirement.selection_digest
                    {
                        return Err(D::Error::custom("inconsistent guarded terminal"));
                    }
                    (unavailable(*invocation), Some(stored))
                }
            };
            if terminal.is_none()
                && !match &update {
                    InvocationUpdate::Accepted { .. } => !row.uncertain,
                    _ => allowed_failure(&update, row.uncertain),
                }
            {
                return Err(D::Error::custom("inconsistent guarded uncertainty"));
            }
            if update.invocation().0.is_nil() {
                return Err(D::Error::custom("nil guarded invocation"));
            }
            (
                update,
                Some(State {
                    requirement,
                    terminal,
                }),
            )
        } else {
            (
                serde_json::from_value(row.update.value).map_err(D::Error::custom)?,
                None,
            )
        };
        Ok(Self {
            key: row.key,
            intent: row.intent,
            request_digest: row.request_digest,
            wire_id: row.wire_id,
            update,
            uncertain: row.uncertain,
            cancellation_requested: row.cancellation_requested,
            guarded,
            prepared_effect: row.prepared_effect,
            admission: row.admission,
            terminal: row.terminal,
        })
    }
}
impl Serialize for DurableInvocation {
    fn serialize<S: serde::Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        let mut row = s.serialize_struct(
            "DurableInvocation",
            6_usize
                .saturating_add(usize::from(self.guarded.is_some()))
                .saturating_add(usize::from(self.prepared_effect.is_some()))
                .saturating_add(usize::from(self.request_digest.is_some()))
                .saturating_add(usize::from(self.admission.is_some()))
                .saturating_add(usize::from(self.terminal.is_some())),
        )?;
        row.serialize_field("key", &self.key)?;
        row.serialize_field("intent", &self.intent)?;
        row.serialize_field("wire_id", &self.wire_id)?;
        if let Some(terminal) = self.guarded.as_ref().and_then(|g| g.terminal.as_ref()) {
            row.serialize_field("update", terminal)?;
        } else {
            row.serialize_field("update", &self.update)?;
        }
        row.serialize_field("uncertain", &self.uncertain)?;
        row.serialize_field("cancellation_requested", &self.cancellation_requested)?;
        if let Some(guarded) = &self.guarded {
            row.serialize_field("guarded_read", &guarded.requirement)?;
        }
        if let Some(prepared) = &self.prepared_effect {
            row.serialize_field("prepared_effect", prepared)?;
        }
        if let Some(digest) = &self.request_digest {
            row.serialize_field("request_digest", digest)?;
        }
        if let Some(admission) = &self.admission {
            row.serialize_field("admission", admission)?;
        }
        if let Some(terminal) = &self.terminal {
            row.serialize_field("terminal", terminal)?;
        }
        row.end()
    }
}

// First pass: skip arbitrary legacy output without materializing it. Guarded
// update/requirement encoded extents are bounded before Row's owned Value decode.
// This is not qualification of the legacy journal's aggregate allocation.
struct Count<R> {
    reader: R,
    position: Rc<Cell<u64>>,
}
impl<R: Read> Read for Count<R> {
    fn read(&mut self, b: &mut [u8]) -> std::io::Result<usize> {
        let n = self.reader.read(b)?;
        self.position
            .set(self.position.get().saturating_add(n as u64));
        Ok(n)
    }
}
struct Seed {
    position: Rc<Cell<u64>>,
    kind: u8,
}
struct Probe {
    position: Rc<Cell<u64>>,
    kind: u8,
}
struct Field(u32);
impl<'de> Deserialize<'de> for Field {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        struct F;
        impl Visitor<'_> for F {
            type Value = Field;
            fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
                f.write_str("object field")
            }
            fn visit_str<E: serde::de::Error>(self, s: &str) -> Result<Field, E> {
                Ok(Field(match s {
                    "records" => 1,
                    "guarded_read" => 2,
                    "update" => 4,
                    "state" => 8,
                    "invocation" => 16,
                    "selection_digest" => 32,
                    "output_digest" => 64,
                    "basis_digest" => 128,
                    "output_bytes" => 256,
                    "error" => 512,
                    "code" => 1024,
                    "message" => 2048,
                    "retryable" => 4096,
                    "prepared_effect" => 8192,
                    "record" => 16384,
                    _ => 0,
                }))
            }
        }
        d.deserialize_identifier(F)
    }
}
impl<'de> DeserializeSeed<'de> for Seed {
    type Value = bool;
    fn deserialize<D: serde::Deserializer<'de>>(self, d: D) -> Result<bool, D::Error> {
        let p = Probe {
            position: self.position,
            kind: self.kind,
        };
        if self.kind == 1 {
            d.deserialize_seq(p)
        } else {
            d.deserialize_map(p)
        }
    }
}
impl<'de> Visitor<'de> for Probe {
    type Value = bool;
    fn expecting(&self, f: &mut fmt::Formatter) -> fmt::Result {
        f.write_str("journal object or records")
    }
    fn visit_seq<A: SeqAccess<'de>>(self, mut seq: A) -> Result<bool, A::Error> {
        while seq
            .next_element_seed(Seed {
                position: self.position.clone(),
                kind: 2,
            })?
            .is_some()
        {}
        Ok(false)
    }
    fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<bool, A::Error> {
        use serde::de::Error;
        let mut present = false;
        let mut update_seen = false;
        let mut update_bytes = 0;
        let mut requirement_bytes = 0;
        let mut prepared_seen = false;
        let malformed = false;
        while let Some(Field(field)) = map.next_key()? {
            let start = self.position.get();
            match (self.kind, field) {
                (0, 1) => {
                    map.next_value_seed(Seed {
                        position: self.position.clone(),
                        kind: 1,
                    })?;
                }
                // An appended entry carries one row, bounded like a snapshot's.
                (0, 16384) => {
                    map.next_value_seed(Seed {
                        position: self.position.clone(),
                        kind: 2,
                    })?;
                }
                (2, 2) => {
                    if present {
                        return Err(A::Error::duplicate_field("guarded_read"));
                    }
                    present = true;
                    map.next_value::<IgnoredAny>()?;
                    requirement_bytes = self.position.get().saturating_sub(start);
                }
                (2, 4) => {
                    if update_seen {
                        return Err(A::Error::duplicate_field("update"));
                    }
                    update_seen = true;
                    map.next_value::<IgnoredAny>()?;
                    update_bytes = self.position.get().saturating_sub(start);
                }
                (2, 8192) => {
                    if prepared_seen {
                        return Err(A::Error::duplicate_field("prepared_effect"));
                    }
                    prepared_seen = true;
                    map.next_value::<IgnoredAny>()?;
                    if self.position.get().saturating_sub(start) > 512 * 1024 {
                        return Err(A::Error::custom("prepared effect exceeds encoded bound"));
                    }
                }
                _ => {
                    map.next_value::<IgnoredAny>()?;
                }
            }
        }
        if self.kind == 2
            && present
            && (update_bytes > MAX_UPDATE || requirement_bytes > MAX_REQUIREMENT)
        {
            return Err(A::Error::custom(
                "guarded journal update shape or extent invalid",
            ));
        }
        Ok(malformed)
    }
}
/// Checks one encoded snapshot, or one appended entry, before owned decoding.
pub(super) fn preflight(reader: &mut impl Read) -> Result<(), String> {
    let position = Rc::new(Cell::new(0));
    let reader = Count {
        reader: BufReader::new(reader.take(InvocationJournal::MAX_BYTES)),
        position: position.clone(),
    };
    let mut decoder = serde_json::Deserializer::from_reader(reader);
    Seed { position, kind: 0 }
        .deserialize(&mut decoder)
        .map_err(|_| "invocation journal guarded preflight failed")?;
    decoder
        .end()
        .map_err(|_| "invocation journal trailing bytes".to_string())
}
