//! Immutable existing publication backing, exposed as one canonical wire stream.
use crate::owner::resources::OwnedPublicationPin;
use anyhow::{Result, ensure};
use std::borrow::Cow;
use tilo_relay::delivery_v3::onboarding_wire::{
    HandoffSegment, MAX_GOVERNANCE_BYTES, MAX_SEGMENT_BYTES,
};

pub(super) enum Payload {
    Encoded(Vec<u8>),
    Governance(OwnedPublicationPin),
}
impl Payload {
    /// The one-field GovernanceBundle is a postcard sequence of existing
    /// GovernanceItem { bytes, retention }. These prefixes are canonical and
    /// bounded; payload bytes are never copied into another document store.
    fn visit(&self, mut consume: impl FnMut(&[u8]) -> Result<()>) -> Result<()> {
        match self {
            Self::Encoded(bytes) => consume(bytes),
            Self::Governance(pin) => {
                let mut prefix = [0; 16];
                consume(postcard::to_slice(&pin.len(), &mut prefix)?)?;
                for index in 0..pin.len() {
                    let item = pin
                        .get(index)
                        .ok_or_else(|| anyhow::anyhow!("pinned publication index changed"))?;
                    consume(postcard::to_slice(&item.bytes.len(), &mut prefix)?)?;
                    consume(item.bytes)?;
                    consume(postcard::to_slice(&item.kind, &mut prefix)?)?;
                }
                Ok(())
            }
        }
    }
    pub(super) fn len(&self) -> Result<usize> {
        let mut length = 0usize;
        self.visit(|bytes| {
            length = length
                .checked_add(bytes.len())
                .ok_or_else(|| anyhow::anyhow!("handoff length overflow"))?;
            Ok(())
        })?;
        ensure!(
            length > 0 && length <= MAX_GOVERNANCE_BYTES,
            "handoff exceeds canonical bound"
        );
        Ok(length)
    }
    pub(super) fn hashes(&self) -> Result<([u8; 32], Vec<HandoffSegment>)> {
        let length = self.len()?;
        let mut whole = blake3::Hasher::new();
        let mut segment = blake3::Hasher::new();
        let mut segment_len = 0usize;
        let mut segments = Vec::with_capacity(length.div_ceil(MAX_SEGMENT_BYTES as usize));
        self.visit(|mut bytes| {
            whole.update(bytes);
            while !bytes.is_empty() {
                let count = bytes.len().min(MAX_SEGMENT_BYTES as usize - segment_len);
                segment.update(&bytes[..count]);
                segment_len += count;
                bytes = &bytes[count..];
                if segment_len == MAX_SEGMENT_BYTES as usize {
                    segments.push(HandoffSegment {
                        digest: *segment.finalize().as_bytes(),
                        length: segment_len as u64,
                    });
                    segment = blake3::Hasher::new();
                    segment_len = 0;
                }
            }
            Ok(())
        })?;
        if segment_len > 0 {
            segments.push(HandoffSegment {
                digest: *segment.finalize().as_bytes(),
                length: segment_len as u64,
            });
        }
        Ok((*whole.finalize().as_bytes(), segments))
    }
    pub(super) fn range(&self, offset: usize, length: usize) -> Result<Cow<'_, [u8]>> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| anyhow::anyhow!("handoff range overflow"))?;
        ensure!(end <= self.len()?, "handoff range exceeds pin");
        if let Self::Encoded(bytes) = self {
            return Ok(Cow::Borrowed(&bytes[offset..end]));
        }
        let mut output = Vec::with_capacity(length);
        let mut position = 0usize;
        self.visit(|bytes| {
            let next = position + bytes.len();
            if position < end && next > offset {
                output.extend_from_slice(
                    &bytes[offset.saturating_sub(position)..bytes.len().min(end - position)],
                );
            }
            position = next;
            Ok(())
        })?;
        ensure!(
            output.len() == length && output.capacity() == length,
            "handoff range allocation changed"
        );
        Ok(Cow::Owned(output))
    }
}
