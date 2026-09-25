//! The existing owner's committed, signed recovery basis. No room reconstruction.
use super::{EnrollmentError, SessionOwner};
use serde::{
    Serialize,
    ser::{SerializeMap, SerializeSeq},
};
use std::io::{self, Write};

pub(crate) const LOCAL_REPLY_BYTES: usize = 1024 * 1024;
// The separately appended discovery hint is bounded to 128KiB by enrollment.rs.
const DISCOVERY_ALLOWANCE: usize = 128 * 1024;

struct Count {
    remaining: usize,
    exceeded: bool,
}
impl Write for Count {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.remaining = match self.remaining.checked_sub(bytes.len()) {
            Some(remaining) => remaining,
            None => {
                self.exceeded = true;
                return Err(io::Error::other(
                    "canonical enrollment requires a bounded HTTPS handoff",
                ));
            }
        };
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}
struct Messages<'a>(&'a SessionOwner);
impl Serialize for Messages<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let outgoing = self.0.outgoing();
        let mut sequence = serializer.serialize_seq(Some(outgoing.len()))?;
        for publication in outgoing.iter() {
            sequence.serialize_element(publication.bytes)?;
        }
        sequence.end()
    }
}
struct Kinds<'a>(&'a SessionOwner);
impl Serialize for Kinds<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let outgoing = self.0.outgoing();
        let mut sequence = serializer.serialize_seq(Some(outgoing.len()))?;
        for publication in outgoing.iter() {
            sequence.serialize_element(&publication.kind)?;
        }
        sequence.end()
    }
}
#[derive(Serialize)]
struct Governance<'a> {
    governance_messages: Messages<'a>,
    governance_retention: Kinds<'a>,
    governance_generation: String,
}
pub(crate) fn append_inline_governance(
    value: &mut serde_json::Value,
    owner: &SessionOwner,
) -> Result<(), EnrollmentError> {
    let governance = Governance {
        governance_messages: Messages(owner),
        governance_retention: Kinds(owner),
        governance_generation: owner.committed_generation().to_string(),
    };
    // Count borrowed original bytes before allocating per-byte JSON Values.
    // Adding two JSON objects overcounts the merged braces, safely and exactly
    // within the existing full-reply limit including the discovery allowance.
    let mut count = Count {
        remaining: LOCAL_REPLY_BYTES - DISCOVERY_ALLOWANCE,
        exceeded: false,
    };
    serde_json::to_writer(&mut count, &*value).map_err(EnrollmentError::unavailable)?;
    serde_json::to_writer(&mut count, &governance).map_err(EnrollmentError::unavailable)?;
    let serde_json::Value::Object(fields) =
        serde_json::to_value(governance).map_err(EnrollmentError::unavailable)?
    else {
        return Err(EnrollmentError::new(503, "native governance missing"));
    };
    let target = value
        .get_mut("bootstrap")
        .and_then(serde_json::Value::as_object_mut)
        .ok_or_else(|| EnrollmentError::new(503, "native bootstrap missing"))?;
    target.extend(fields);
    Ok(())
}

/// HTTPS encodes borrowed original bytes directly, avoiding per-byte JSON Value
/// allocation. The caller's existing response lease covers this <=1MiB buffer.
pub(crate) fn encode_inline_bootstrap(
    value: &serde_json::Value,
    owner: &SessionOwner,
) -> Result<Option<Vec<u8>>, EnrollmentError> {
    let merged = WithGovernance {
        metadata: value,
        owner,
        nested: false,
    };
    let mut count = Count {
        remaining: LOCAL_REPLY_BYTES,
        exceeded: false,
    };
    if let Err(error) = serde_json::to_writer(&mut count, &merged) {
        return if count.exceeded {
            Ok(None)
        } else {
            Err(EnrollmentError::unavailable(error))
        };
    }
    let length = LOCAL_REPLY_BYTES - count.remaining;
    let mut bytes = Vec::with_capacity(length);
    serde_json::to_writer(&mut bytes, &merged).map_err(EnrollmentError::unavailable)?;
    if bytes.len() != length || bytes.capacity() != length {
        return Err(EnrollmentError::new(
            503,
            "enrollment encoding allocation changed",
        ));
    }
    Ok(Some(bytes))
}
struct WithGovernance<'a> {
    metadata: &'a serde_json::Value,
    owner: &'a SessionOwner,
    nested: bool,
}
impl Serialize for WithGovernance<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let object = self
            .metadata
            .as_object()
            .ok_or_else(|| serde::ser::Error::custom("native bootstrap missing"))?;
        let mut map =
            serializer.serialize_map(Some(object.len() + if self.nested { 3 } else { 0 }))?;
        for (name, value) in object {
            if !self.nested && name == "bootstrap" {
                map.serialize_entry(
                    name,
                    &WithGovernance {
                        metadata: value,
                        owner: self.owner,
                        nested: true,
                    },
                )?;
            } else {
                map.serialize_entry(name, value)?;
            }
        }
        if self.nested {
            map.serialize_entry("governance_messages", &Messages(self.owner))?;
            map.serialize_entry("governance_retention", &Kinds(self.owner))?;
            map.serialize_entry(
                "governance_generation",
                &self.owner.committed_generation().to_string(),
            )?;
        }
        map.end()
    }
}

pub(crate) fn encode_handoff_bootstrap(
    metadata: &serde_json::Value,
    manifest: &[u8],
    generation: u64,
) -> Result<Vec<u8>, EnrollmentError> {
    let generation = generation.to_string();
    let merged = WithHandoff {
        metadata,
        manifest,
        generation: &generation,
        nested: false,
    };
    let mut count = Count {
        remaining: LOCAL_REPLY_BYTES,
        exceeded: false,
    };
    serde_json::to_writer(&mut count, &merged).map_err(EnrollmentError::unavailable)?;
    let length = LOCAL_REPLY_BYTES - count.remaining;
    let mut bytes = Vec::with_capacity(length);
    serde_json::to_writer(&mut bytes, &merged).map_err(EnrollmentError::unavailable)?;
    if bytes.len() != length || bytes.capacity() != length {
        return Err(EnrollmentError::new(
            503,
            "enrollment handoff allocation changed",
        ));
    }
    Ok(bytes)
}
struct WithHandoff<'a> {
    metadata: &'a serde_json::Value,
    manifest: &'a [u8],
    generation: &'a str,
    nested: bool,
}
impl Serialize for WithHandoff<'_> {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        let object = self
            .metadata
            .as_object()
            .ok_or_else(|| serde::ser::Error::custom("native bootstrap missing"))?;
        let mut map =
            serializer.serialize_map(Some(object.len() + if self.nested { 2 } else { 0 }))?;
        for (name, value) in object {
            if !self.nested && name == "bootstrap" {
                map.serialize_entry(
                    name,
                    &WithHandoff {
                        metadata: value,
                        manifest: self.manifest,
                        generation: self.generation,
                        nested: true,
                    },
                )?;
            } else {
                map.serialize_entry(name, value)?;
            }
        }
        if self.nested {
            map.serialize_entry("governance_handoff", self.manifest)?;
            map.serialize_entry("governance_generation", self.generation)?;
        }
        map.end()
    }
}
