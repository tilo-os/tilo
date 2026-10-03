//! Bounded declarative registration; no callback or regex key matchers.
use super::{
    HistoryRestoreNode, HistoryRestoreSchema, HistoryRootPattern, HistoryRootSelector, RestoreError,
};
use automerge::{AutoCommit, ROOT, ReadDoc};
use std::collections::BTreeSet;

const MAX_CAPTURE_BYTES: usize = 256;
const MAX_AFFIX_BYTES: usize = 256;
const MAX_ENTRY_BYTES: usize = 1024;
const MAX_DEPTH: usize = 256;

pub(super) fn selectors(schema: &HistoryRestoreSchema) -> Vec<HistoryRootSelector> {
    schema
        .roots
        .iter()
        .map(|root| HistoryRootSelector {
            pattern: HistoryRootPattern::Exact(root.key),
            required: root.required,
            node: HistoryRestoreNode::Whole {
                expected_type: root.expected_type,
            },
        })
        .chain(schema.selectors.iter().copied())
        .collect()
}
fn invalid(reason: &str) -> RestoreError {
    RestoreError::InvalidSchema(reason.into())
}
fn capture_byte(byte: u8) -> bool {
    !byte.is_ascii_control()
}

impl HistoryRootPattern {
    fn capture(self, key: &str) -> Option<&str> {
        match self {
            Self::Affix { prefix, suffix, .. } => key.strip_prefix(prefix)?.strip_suffix(suffix),
            Self::Exact(_) => None,
        }
    }
    pub(super) fn matches(self, key: &str) -> bool {
        match self {
            Self::Exact(exact) => exact == key,
            Self::Affix {
                max_capture_bytes, ..
            } => self.capture(key).is_some_and(|capture| {
                !capture.is_empty()
                    && capture.len() <= max_capture_bytes
                    && capture.chars().all(|character| !character.is_control())
            }),
        }
    }
    pub(super) fn keys(self, doc: &AutoCommit) -> Result<Vec<String>, RestoreError> {
        match self {
            Self::Exact(key) => Ok(vec![key.into()]),
            Self::Affix { .. } => {
                let mut keys = Vec::new();
                for key in doc.keys(ROOT) {
                    if self.capture(&key).is_some() {
                        if !self.matches(&key) {
                            return Err(RestoreError::SchemaInvalid(format!(
                                "invalid family capture in {key}"
                            )));
                        }
                        keys.push(key);
                    }
                }
                Ok(keys)
            }
        }
    }
    fn validate(self) -> Result<(), RestoreError> {
        match self {
            Self::Exact("") => Err(invalid("empty exact property")),
            Self::Exact(_) => Ok(()),
            Self::Affix {
                prefix,
                suffix,
                max_capture_bytes,
            } => {
                if prefix.is_empty()
                    || !prefix.contains(['.', ':'])
                    || !prefix.is_ascii()
                    || !suffix.is_ascii()
                    || prefix.len() > MAX_AFFIX_BYTES
                    || suffix.len() > MAX_AFFIX_BYTES
                    || max_capture_bytes == 0
                    || max_capture_bytes > MAX_CAPTURE_BYTES
                    || prefix
                        .bytes()
                        .chain(suffix.bytes())
                        .any(|byte| byte.is_ascii_control())
                {
                    return Err(invalid(
                        "family needs a fixed domain prefix, ASCII affixes <=256 bytes and capture bound 1..=256",
                    ));
                }
                Ok(())
            }
        }
    }
    fn lengths(self) -> std::ops::RangeInclusive<usize> {
        match self {
            Self::Exact(key) => key.len()..=key.len(),
            Self::Affix {
                prefix,
                suffix,
                max_capture_bytes,
            } => {
                let fixed = prefix.len().saturating_add(suffix.len());
                fixed.saturating_add(1)..=fixed.saturating_add(max_capture_bytes)
            }
        }
    }
    /// At a candidate total length, each byte is fixed or belongs to the same
    /// capture alphabet. Affixes are ASCII, so every possible family/family
    /// intersection has an ASCII witness. Exact keys may contain UTF-8 and are
    /// checked directly, retaining the byte-bound and control-character rules.
    fn byte_at(self, length: usize, index: usize) -> Option<u8> {
        match self {
            Self::Exact(key) => key.as_bytes().get(index).copied(),
            Self::Affix { prefix, suffix, .. } => {
                if index < prefix.len() {
                    prefix.as_bytes().get(index).copied()
                } else if index >= length.saturating_sub(suffix.len()) {
                    suffix
                        .as_bytes()
                        .get(index.saturating_sub(length.saturating_sub(suffix.len())))
                        .copied()
                } else {
                    None
                }
            }
        }
    }
    fn overlaps(self, other: Self) -> bool {
        if let Self::Exact(key) = self {
            return other.matches(key);
        }
        if let Self::Exact(key) = other {
            return self.matches(key);
        }
        let left = self.lengths();
        let right = other.lengths();
        let start = (*left.start()).max(*right.start());
        let end = (*left.end()).min(*right.end());
        (start..=end).any(|length| {
            (0..length).all(|index| {
                match (self.byte_at(length, index), other.byte_at(length, index)) {
                    (Some(a), Some(b)) => a == b,
                    (Some(byte), None) | (None, Some(byte)) => capture_byte(byte),
                    (None, None) => true,
                }
            })
        })
    }
    fn reserved(self) -> bool {
        self.lengths().any(|length| {
            length >= 4
                && b"tilo:".iter().enumerate().all(|(index, byte)| {
                    self.byte_at(length, index)
                        .map_or_else(|| capture_byte(*byte), |fixed| fixed == *byte)
                })
        })
    }
}

pub(super) fn validate(schema: &HistoryRestoreSchema) -> Result<(), RestoreError> {
    let selectors = selectors(schema);
    if schema.id.is_empty() || schema.version == 0 || selectors.is_empty() {
        return Err(invalid(
            "schema id, nonzero version and content selectors are required",
        ));
    }
    let preserved: Vec<_> = schema
        .preserved_roots
        .iter()
        .map(|key| HistoryRootPattern::Exact(key))
        .chain(schema.preserved.iter().copied())
        .collect();
    for pattern in &preserved {
        pattern.validate()?;
    }
    for (index, selector) in selectors.iter().enumerate() {
        selector.pattern.validate()?;
        if selector.pattern.reserved()
            || preserved
                .iter()
                .any(|pattern| selector.pattern.overlaps(*pattern))
            || selectors
                .iter()
                .take(index)
                .any(|previous| selector.pattern.overlaps(previous.pattern))
        {
            return Err(invalid("overlapping or protected selector"));
        }
        validate_node(selector.node, 0)?;
    }
    Ok(())
}
fn validate_node(node: HistoryRestoreNode, depth: usize) -> Result<(), RestoreError> {
    let mut pending = vec![(node, depth)];
    while let Some((node, depth)) = pending.pop() {
        if depth > MAX_DEPTH {
            return Err(invalid("selector nesting exceeds 256 (possibly cyclic)"));
        }
        match node {
            HistoryRestoreNode::Whole { .. } => {}
            HistoryRestoreNode::MapContentExcept { preserved_fields } => {
                if !(1..=64).contains(&preserved_fields.len()) {
                    return Err(invalid("preserved field count must be 1..=64"));
                }
                let mut keys = BTreeSet::new();
                for field in preserved_fields {
                    if field.is_empty() || field.len() > MAX_ENTRY_BYTES || !keys.insert(*field) {
                        return Err(invalid(
                            "preserved fields must be unique nonempty names of at most 1024 bytes",
                        ));
                    }
                }
            }
            HistoryRestoreNode::MapOverlay(overlay) => {
                if overlay.fields.is_empty() && overlay.entries.is_none() {
                    return Err(invalid("empty map overlay"));
                }
                let mut keys = BTreeSet::new();
                for field in overlay.fields {
                    if field.key.is_empty() || !keys.insert(field.key) {
                        return Err(invalid("empty or duplicate overlay field"));
                    }
                }
                if let Some(entries) = overlay.entries {
                    match entries.membership {
                        super::HistoryMembership::TargetAuthoritative => {}
                    }
                    if entries.key_limit_bytes == 0 || entries.key_limit_bytes > MAX_ENTRY_BYTES {
                        return Err(invalid("entity key bound must be 1..=1024 bytes"));
                    }
                    pending.push((entries.node, depth.saturating_add(1)));
                }
                pending.extend(
                    overlay
                        .fields
                        .iter()
                        .rev()
                        .map(|field| (field.node, depth.saturating_add(1))),
                );
            }
        }
    }
    Ok(())
}
