//! Descriptive selected replacement locus. Decoding never grants authority.
//! The signed source bootstrap must independently authenticate the exact sealed
//! root-owned descriptor and bind its bytes to its source-token digest.
use serde::{Deserialize, Serialize};
use std::io;

pub const SELECTED_REPLACEMENT_PERMIT_FD: i32 = 63;
pub const MAX_PERMIT_BYTES: usize = 32768;

/// Explicit framework publication purpose. Descriptive bytes never grant authority.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum SelectedReplacementPurpose {
    AtomicSave,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SelectedReplacementPermitRecord {
    pub schema: u32,
    pub purpose: SelectedReplacementPurpose,
    pub source_owner: [u8; 32],
    pub source_operation: String,
    pub person: [u8; 32],
    pub personal_room: String,
    pub editor_request_id: String,
    pub editor_operation: String,
    pub editor_context_sha256: [u8; 32],
    pub package_sha256: String,
    pub source_root: [u64; 2],
    pub parent: [u64; 2],
    pub selected: [u64; 2],
    pub components: Vec<Vec<u8>>,
}
fn invalid() -> io::Error {
    io::ErrorKind::InvalidData.into()
}
fn canonical_id(value: &str) -> bool {
    value.len() == 36
        && value.bytes().enumerate().all(|(i, byte)| {
            if [8, 13, 18, 23].contains(&i) {
                byte == b'-'
            } else {
                byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte)
            }
        })
        && value.bytes().any(|byte| byte != b'0' && byte != b'-')
}
impl SelectedReplacementPermitRecord {
    /// Validate descriptive identities and the bounded raw replacement locus.
    pub fn validate(&self) -> io::Result<()> {
        let raw_bytes = self.components.iter().try_fold(0usize, |total, component| {
            if component.is_empty()
                || component.len() > 255
                || component == b"."
                || component == b".."
                || component.contains(&0)
                || component.contains(&b'/')
            {
                return None;
            }
            total.checked_add(component.len())
        });
        if self.schema != 2
            || self.source_owner == [0; 32]
            || self.person == [0; 32]
            || self.editor_context_sha256 == [0; 32]
            || ![
                &self.source_operation,
                &self.personal_room,
                &self.editor_request_id,
                &self.editor_operation,
            ]
            .iter()
            .all(|value| canonical_id(value))
            || self.package_sha256.len() != 64
            || !self
                .package_sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            || self.package_sha256.bytes().all(|byte| byte == b'0')
            || self.source_root[1] == 0
            || self.parent[1] == 0
            || self.selected[1] == 0
            || self.source_root[0] != self.parent[0]
            || self.source_root[0] != self.selected[0]
            || self.components.is_empty()
            || self.components.len() > 64
            || raw_bytes.is_none_or(|bytes| bytes > 4096)
        {
            return Err(invalid());
        }
        Ok(())
    }
    /// Encode a validated record in its canonical wire representation.
    pub fn encode(&self) -> io::Result<Vec<u8>> {
        self.validate()?;
        let bytes = serde_json::to_vec(self).map_err(io::Error::other)?;
        if bytes.len() > MAX_PERMIT_BYTES {
            return Err(invalid());
        }
        Ok(bytes)
    }
    /// Decode a bounded canonical record without granting replacement authority.
    ///
    /// # Errors
    /// Refuses empty, oversized, malformed, invalid or noncanonical records.
    pub fn decode(bytes: &[u8]) -> io::Result<Self> {
        if bytes.is_empty() || bytes.len() > MAX_PERMIT_BYTES {
            return Err(invalid());
        }
        let record: Self = serde_json::from_slice(bytes).map_err(io::Error::other)?;
        record.validate()?;
        if record.encode()? != bytes {
            return Err(invalid());
        }
        Ok(record)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn record() -> SelectedReplacementPermitRecord {
        SelectedReplacementPermitRecord {
            schema: 2,
            purpose: SelectedReplacementPurpose::AtomicSave,
            source_owner: [1; 32],
            source_operation: "c62f9a00-cbf3-40d9-9f2a-1ead27b2a43f".into(),
            person: [2; 32],
            personal_room: "a7956913-f8dd-4e83-9b02-dd637c7cd522".into(),
            editor_request_id: "b5b95a69-7dbd-4c9c-b0e1-b5e79f3c0bc8".into(),
            editor_operation: "3fa5d30e-79b8-4bb4-839b-cfc18dfc2c24".into(),
            editor_context_sha256: [3; 32],
            package_sha256: "ab".repeat(32),
            source_root: [9, 10],
            parent: [9, 11],
            selected: [9, 12],
            components: vec![b"docs".to_vec(), vec![0xff, b'x']],
        }
    }
    #[test]
    fn exact_codec_preserves_raw_locus_and_rejects_noncanonical_or_unknown_claims() {
        let value = record();
        let bytes = value.encode().unwrap();
        assert_eq!(
            SelectedReplacementPermitRecord::decode(&bytes).unwrap(),
            value
        );
        let mut noncanonical = vec![b' '];
        noncanonical.extend(&bytes);
        assert!(SelectedReplacementPermitRecord::decode(&noncanonical).is_err());
        let mut unknown: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        unknown
            .as_object_mut()
            .unwrap()
            .insert("replace_authorized".into(), true.into());
        assert!(
            SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&unknown).unwrap())
                .is_err()
        );
        for schema in [0, 1, 3] {
            let mut bad = value.clone();
            bad.schema = schema;
            assert!(bad.encode().is_err());
        }
    }
    #[test]
    fn explicit_atomic_save_purpose_is_mandatory_and_closed() {
        let bytes = record().encode().unwrap();
        for purpose in [None, Some("ReplacementRename"), Some("Unknown")] {
            let mut value: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
            let fields = value.as_object_mut().unwrap();
            match purpose {
                None => {
                    fields.remove("purpose");
                }
                Some(value) => {
                    fields.insert("purpose".into(), value.into());
                }
            }
            assert!(
                SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&value).unwrap())
                    .is_err()
            );
        }
        let mut old: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        old["schema"] = 1.into();
        assert!(
            SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&old).unwrap()).is_err()
        );
    }
    #[test]
    fn actual_decoder_refuses_escaped_locus_and_unbounded_component_claims() {
        let mut value = record();
        for component in [b"..".to_vec(), b"a/b".to_vec(), vec![0], vec![b'x'; 256]] {
            value.components = vec![component];
            assert!(
                SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&value).unwrap())
                    .is_err()
            );
        }
        value.components = vec![vec![b'x'; 255]; 17];
        assert!(
            SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&value).unwrap()).is_err()
        );
        value = record();
        value.parent[0] = 8;
        assert!(
            SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&value).unwrap()).is_err()
        );
        value = record();
        value.selected[1] = 0;
        assert!(
            SelectedReplacementPermitRecord::decode(&serde_json::to_vec(&value).unwrap()).is_err()
        );
    }
}
