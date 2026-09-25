//! Inert, bounded directory-chain selection. These bytes grant no authority.
use super::super::{model::*, store::StoreError};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::connection) struct RegularContentV1 {
    pub sha256: [u8; 32],
    pub bytes: u64,
    pub executable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::connection) enum DirectoryChainTerminalV1 {
    DirectoryAdd,
    RegularAdd { content: RegularContentV1 },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::connection) struct DirectoryChainSelectionV1 {
    pub version: u8,
    pub connection: ConnectionId,
    pub endpoints: [EndpointIdentity; 2],
    pub review_revision: u64,
    pub original_fence: u64,
    pub baseline: Option<u64>,
    pub preview_digest: [u8; 32],
    pub selection_digest: [u8; 32],
    pub path: RelativePath,
    pub from: Side,
    pub terminal: DirectoryChainTerminalV1,
    pub baselined_prefixes: u64,
    pub existing_prefixes: u64,
    pub missing_prefixes: u64,
}

use sha2::{Digest, Sha256};
const MAX_SELECTION_BYTES: usize = 12_288;
const DOMAIN: &[u8] = b"qos-imager/connections-v1/directory-chain-selection/v1\0";
struct Reader<'a>(&'a [u8]);
impl Reader<'_> {
    fn take<const N: usize>(&mut self) -> Result<[u8; N], StoreError> {
        let (head, tail) = self.0.split_at_checked(N).ok_or(StoreError::Corrupt)?;
        self.0 = tail;
        head.try_into().map_err(|_| StoreError::Corrupt)
    }
    fn byte(&mut self) -> Result<u8, StoreError> {
        Ok(self.take::<1>()?[0])
    }
    fn number(&mut self) -> Result<u64, StoreError> {
        Ok(u64::from_le_bytes(self.take()?))
    }
    fn identity(&mut self) -> Result<EndpointIdentity, StoreError> {
        Ok(EndpointIdentity {
            volume: self.take()?,
            root: self.take()?,
        })
    }
}
impl DirectoryChainSelectionV1 {
    pub(in crate::connection) fn decode(bytes: &[u8]) -> Result<Self, StoreError> {
        if bytes.len() > MAX_SELECTION_BYTES {
            return Err(StoreError::LimitExceeded);
        }
        let mut r = Reader(bytes);
        if r.take::<8>()? != *b"QCHSEL1\0" {
            return Err(StoreError::Corrupt);
        }
        let version = r.byte()?;
        let connection = ConnectionId(r.take()?);
        let source = r.identity()?;
        let destination = r.identity()?;
        let review_revision = r.number()?;
        let original_fence = r.number()?;
        let baseline = match r.byte()? {
            0 => None,
            1 => Some(r.number()?),
            _ => return Err(StoreError::Corrupt),
        };
        let preview_digest = r.take()?;
        let encoding = r.byte()?;
        let count = usize::from(u16::from_le_bytes(r.take()?));
        if count > MAX_PATH_UNITS {
            return Err(StoreError::LimitExceeded);
        }
        let path = match encoding {
            1 => {
                let (path, tail) = r.0.split_at_checked(count).ok_or(StoreError::Corrupt)?;
                r.0 = tail;
                RelativePath::unix(path)
            }
            2 => {
                let mut units = Vec::with_capacity(count);
                for _ in 0..count {
                    units.push(u16::from_le_bytes(r.take()?));
                }
                RelativePath::windows(&units)
            }
            _ => return Err(StoreError::Corrupt),
        }
        .map_err(|_| StoreError::Corrupt)?;
        let from = match r.byte()? {
            0 => Side::Source,
            1 => Side::Destination,
            _ => return Err(StoreError::Corrupt),
        };
        let terminal = match r.byte()? {
            0 => DirectoryChainTerminalV1::DirectoryAdd,
            1 => DirectoryChainTerminalV1::RegularAdd {
                content: RegularContentV1 {
                    sha256: r.take()?,
                    bytes: r.number()?,
                    executable: match r.byte()? {
                        0 => false,
                        1 => true,
                        _ => return Err(StoreError::Corrupt),
                    },
                },
            },
            _ => return Err(StoreError::Corrupt),
        };
        let value = Self {
            version,
            connection,
            endpoints: [source, destination],
            review_revision,
            original_fence,
            baseline,
            preview_digest,
            path,
            from,
            terminal,
            baselined_prefixes: r.number()?,
            existing_prefixes: r.number()?,
            missing_prefixes: r.number()?,
            selection_digest: r.take()?,
        };
        if !r.0.is_empty() || value.encode()? != bytes {
            return Err(StoreError::Corrupt);
        }
        Ok(value)
    }
    pub(in crate::connection) fn checked(mut self) -> Result<Self, StoreError> {
        let bytes = self.encode_preimage()?;
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(bytes);
        self.selection_digest = hash.finalize().into();
        self.encode()?;
        Ok(self)
    }
    pub(in crate::connection) fn encode(&self) -> Result<Vec<u8>, StoreError> {
        let mut bytes = self.encode_preimage()?;
        let mut hash = Sha256::new();
        hash.update(DOMAIN);
        hash.update(&bytes);
        let digest: [u8; 32] = hash.finalize().into();
        if self.selection_digest != digest {
            return Err(StoreError::Corrupt);
        }
        bytes.extend(digest);
        if bytes.len() > MAX_SELECTION_BYTES {
            return Err(StoreError::LimitExceeded);
        }
        Ok(bytes)
    }
    fn encode_preimage(&self) -> Result<Vec<u8>, StoreError> {
        self.validate_structure()?;
        let count = self
            .path
            .components()
            .iter()
            .map(|c| match c {
                NativeName::Unix(v) => v.len(),
                NativeName::Windows(v) => v.len(),
            })
            .sum::<usize>()
            + self.path.components().len()
            - 1;
        let width = if self.path.encoding() == Encoding::Windows {
            2
        } else {
            1
        };
        // Every variable-length term is bounded before allocating the output.
        let size = 263
            + usize::from(self.baseline.is_some()) * 8
            + usize::from(matches!(
                self.terminal,
                DirectoryChainTerminalV1::RegularAdd { .. }
            )) * 41
            + count * width;
        if count > MAX_PATH_UNITS || size > MAX_SELECTION_BYTES {
            return Err(StoreError::LimitExceeded);
        }
        let mut b = Vec::with_capacity(size);
        b.extend(b"QCHSEL1\0");
        b.push(self.version);
        b.extend(self.connection.0);
        for endpoint in self.endpoints {
            b.extend(endpoint.volume);
            b.extend(endpoint.root);
        }
        b.extend(self.review_revision.to_le_bytes());
        b.extend(self.original_fence.to_le_bytes());
        b.push(u8::from(self.baseline.is_some()));
        if let Some(baseline) = self.baseline {
            b.extend(baseline.to_le_bytes());
        }
        b.extend(self.preview_digest);
        b.push(match self.path.encoding() {
            Encoding::Unix => 1,
            Encoding::Windows => 2,
        });
        b.extend(
            u16::try_from(count)
                .map_err(|_| StoreError::LimitExceeded)?
                .to_le_bytes(),
        );
        for (i, c) in self.path.components().iter().enumerate() {
            match c {
                NativeName::Unix(v) => {
                    if i != 0 {
                        b.push(b'/');
                    }
                    b.extend(v);
                }
                NativeName::Windows(v) => {
                    if i != 0 {
                        b.extend(92u16.to_le_bytes());
                    }
                    for u in v {
                        b.extend(u.to_le_bytes());
                    }
                }
            }
        }
        b.push(match self.from {
            Side::Source => 0,
            Side::Destination => 1,
        });
        match &self.terminal {
            DirectoryChainTerminalV1::DirectoryAdd => b.push(0),
            DirectoryChainTerminalV1::RegularAdd { content } => {
                b.push(1);
                b.extend(content.sha256);
                b.extend(content.bytes.to_le_bytes());
                b.push(u8::from(content.executable));
            }
        }
        for mask in [
            self.baselined_prefixes,
            self.existing_prefixes,
            self.missing_prefixes,
        ] {
            b.extend(mask.to_le_bytes());
        }
        Ok(b)
    }
    fn validate_structure(&self) -> Result<(), StoreError> {
        let depth = self.path.components().len();
        let expected = if depth == 64 {
            u64::MAX - 1
        } else {
            (1u64.checked_shl(depth as u32).ok_or(StoreError::Corrupt)? - 1) & !1
        };
        let [a, b, c] = [
            self.baselined_prefixes,
            self.existing_prefixes,
            self.missing_prefixes,
        ];
        if self.version != 1
            || self.review_revision == 0
            || self
                .baseline
                .is_some_and(|v| v == 0 || v > self.review_revision)
            || depth == 0
            || depth > 64
            || (a | b | c) != expected
            || a & b != 0
            || a & c != 0
            || b & c != 0
        {
            return Err(StoreError::Corrupt);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::connection) struct DirectoryChainMember {
    pub path: RelativePath,
    pub from: Side,
    pub terminal: DirectoryChainTerminalV1,
}
impl DirectoryChainSelectionV1 {
    pub(in crate::connection) fn member_count(&self) -> Result<u8, StoreError> {
        self.encode()?;
        u8::try_from(self.missing_prefixes.count_ones() + 1).map_err(|_| StoreError::Corrupt)
    }
    pub(in crate::connection) fn member(
        &self,
        index: u8,
    ) -> Result<DirectoryChainMember, StoreError> {
        let count = self.member_count()?;
        if index >= count {
            return Err(StoreError::Corrupt);
        }
        if index == count - 1 {
            return Ok(DirectoryChainMember {
                path: self.path.clone(),
                from: self.from,
                terminal: self.terminal.clone(),
            });
        }
        let mut remaining = self.missing_prefixes;
        for _ in 0..index {
            remaining &= remaining - 1;
        }
        let depth = remaining.trailing_zeros() as usize;
        let mut path = self.path.clone();
        while path.components().len() > depth {
            path = path.parent().ok_or(StoreError::Corrupt)?;
        }
        Ok(DirectoryChainMember {
            path,
            from: self.from,
            terminal: DirectoryChainTerminalV1::DirectoryAdd,
        })
    }
    #[cfg(test)]
    pub(in crate::connection) fn validate_observations(
        &self,
        baseline: &VerifiedBaseline,
        source: &ObservedTree,
        destination: &ObservedTree,
    ) -> Result<(), StoreError> {
        self.encode()?;
        if baseline.connection != self.connection
            || baseline.source != self.endpoints[0]
            || baseline.destination != self.endpoints[1]
            || baseline.entries().is_some() != self.baseline.is_some()
            || source.endpoint().identity != self.endpoints[0]
            || destination.endpoint().identity != self.endpoints[1]
            || source.observation() != &Observation::Complete
            || destination.observation() != &Observation::Complete
        {
            return Err(StoreError::RecoveryRequired);
        }
        // Reuse the model's endpoint-native namespace comparisons; a missing key,
        // alias, conflicting ancestor or unrepresentable path cannot be approved.
        let plan = super::super::reconcile::reconcile(baseline, source, destination);
        let selected = plan
            .proposals
            .iter()
            .find(|p| p.path.logical() == self.path.logical())
            .ok_or(StoreError::RecoveryRequired)?;
        if matches!(
            selected.decision,
            Decision::Conflict(_) | Decision::Pending(_)
        ) {
            return Err(StoreError::RecoveryRequired);
        }
        let (origin, target) = match self.from {
            Side::Source => (source, destination),
            Side::Destination => (destination, source),
        };
        let expected = match &self.terminal {
            DirectoryChainTerminalV1::DirectoryAdd => EntryState::Directory,
            DirectoryChainTerminalV1::RegularAdd { content } => EntryState::File {
                sha256: content.sha256,
                bytes: content.bytes,
                executable: content.executable,
            },
        };
        let observed =
            |tree: &ObservedTree, path: &RelativePath| -> Result<Option<EntryState>, StoreError> {
                let native = path
                    .to_encoding(tree.endpoint().encoding)
                    .map_err(|_| StoreError::RecoveryRequired)?;
                Ok(tree.entries().get(&native).cloned())
            };
        let prior = |path: &RelativePath| {
            baseline
                .entries()
                .and_then(|entries| entries.get(&path.logical()))
        };
        if observed(origin, &self.path)? != Some(expected)
            || observed(target, &self.path)?.is_some()
            || prior(&self.path).is_some()
        {
            return Err(StoreError::RecoveryRequired);
        }
        let mut parent = self.path.parent();
        while let Some(path) = parent {
            if observed(origin, &path)? != Some(EntryState::Directory) {
                return Err(StoreError::RecoveryRequired);
            }
            let bit = 1u64 << path.components().len();
            let old = prior(&path);
            let target_state = observed(target, &path)?;
            let valid = if self.baselined_prefixes & bit != 0 {
                old == Some(&EntryState::Directory) && target_state == Some(EntryState::Directory)
            } else if self.existing_prefixes & bit != 0 {
                old.is_none() && target_state == Some(EntryState::Directory)
            } else {
                old.is_none() && target_state.is_none()
            };
            if !valid {
                return Err(StoreError::RecoveryRequired);
            }
            parent = path.parent();
        }
        Ok(())
    }
}
