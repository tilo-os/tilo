//! Bounded restart clues. Decoding never produces live endpoint authority.
pub(crate) mod directory;
use crate::connection::model::{NativeName, RelativePath};
use sha2::{Digest, Sha256};

#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Error {
    Invalid,
}
type Result<T> = std::result::Result<T, Error>;

pub(crate) const MAX_BYTES: usize = 16384;
const HEADER_BYTES: usize = 121;
const CHECKSUM_BYTES: usize = 32;

/// Persisted correlation only. Every consumer must reacquire live authority.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Header {
    pub connection: [u8; 16],
    pub operation: u64,
    pub attempt: u64,
    pub nonce: [u8; 16],
    pub epoch: [u8; 16],
    pub session: [u8; 16],
    pub admission: u64,
    pub baseline: Option<u64>,
    pub progress: u64,
    pub fence: u64,
}
impl Header {
    pub fn namespace_name(&self) -> String {
        let hex = |bytes: &[u8]| {
            bytes.iter().fold(String::new(), |mut output, byte| {
                use std::fmt::Write;
                let _ = write!(output, "{byte:02x}");
                output
            })
        };
        format!(
            ".qos-operation-{}-{:016x}-{}",
            hex(&self.connection),
            self.operation,
            hex(&self.nonce)
        )
    }
    fn same_attempt(&self, other: &Self) -> bool {
        self.connection == other.connection
            && self.operation == other.operation
            && self.attempt == other.attempt
            && self.nonce == other.nonce
            && self.admission == other.admission
            && self.baseline == other.baseline
    }
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Object {
    pub dev: u64,
    pub ino: u64,
    pub kind: u32,
    pub generation: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stamp {
    pub length: u64,
    pub modified: (i64, i64),
    pub changed: (i64, i64),
    pub mode: u32,
    pub uid: u32,
    pub gid: u32,
    pub links: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Content {
    pub digest: [u8; 32],
    pub length: u64,
    pub executable: bool,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum State {
    Absent,
    Regular {
        object: Object,
        stamp: Stamp,
        content: Content,
    },
    Unsupported {
        object: Object,
        stamp: Stamp,
    },
    Unknown,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub endpoint: [u8; 64],
    pub platform: u8,
    pub identity: [u8; 24],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Recovery {
    pub connection: [u8; 16],
    pub operation: u64,
    pub slot: u32,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Selection {
    None,
    Restore {
        recovery: Recovery,
        artifact: Artifact,
        content: Content,
    },
    Resolve {
        conflict: ([u8; 16], u64),
        revision: u64,
        recovery: Recovery,
        artifact: Artifact,
        content: Content,
    },
    Tombstone {
        conflict: ([u8; 16], u64),
        revision: u64,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Intent {
    pub endpoints: [[u8; 64]; 2],
    pub direction: u8,
    pub decision: u8,
    pub namespace: Object,
    pub path: RelativePath,
    pub chains: [Vec<Object>; 2],
    pub expected: [State; 2],
    pub selection: Selection,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Source {
    Endpoint {
        side: u8,
        parent: Object,
        object: Object,
        stamp: Stamp,
    },
    Recovery {
        recovery: Recovery,
        artifact: Artifact,
        content: Content,
    },
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stage {
    pub intent: [u8; 32],
    pub object: Object,
    pub content: Content,
    pub source: Source,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Displaced {
    pub intent: [u8; 32],
    pub stage: Option<[u8; 32]>,
    pub object: Object,
    pub stamp: Stamp,
    pub matches_expected: bool,
    pub side: u8,
    pub parent: Object,
    pub leaf: NativeName,
    pub barriers: u8,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Reference {
    pub slot: u32,
    pub side: u8,
    pub role: u8,
    pub binding: [u8; 32],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub intent: [u8; 32],
    pub stage: Option<[u8; 32]>,
    pub displaced: Option<[u8; 32]>,
    pub states: [State; 2],
    pub kind: u8,
    pub barriers: u8,
    pub recovery: Vec<Reference>,
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    Intent(Intent),
    Stage(Stage),
    Displaced(Displaced),
    Outcome(Outcome),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
    pub profile: Profile,
    pub header: Header,
    pub body: Body,
    pub digest: [u8; 32],
}
/// Historical producer correlation, never a live creation witness.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Profile {
    Neutral,
    LegacyStage,
    CreatedRegular,
}

/// Bounded historical admission metadata. Content is the approved proposal;
/// this DTO never proves a fresh read, lease, approval or write admission.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Admission {
    pub connection: [u8; 16],
    pub operation: u64,
    pub revision: u64,
    pub fence: u64,
    pub baseline: Option<u64>,
    pub endpoints: [[u8; 64]; 2],
    pub direction: u8,
    pub decision: u8,
    pub path: RelativePath,
    pub chains: [Vec<Object>; 2],
    pub states: [State; 2],
}
impl Admission {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer(Vec::with_capacity(512));
        w.put(b"QNA1");
        w.u32(0);
        w.put(self.connection);
        w.u64(self.operation);
        w.u64(self.revision);
        w.u64(self.fence);
        w.u8(u8::from(self.baseline.is_some()));
        w.u64(self.baseline.unwrap_or(0));
        w.put(self.endpoints[0]);
        w.put(self.endpoints[1]);
        w.u8(self.direction);
        w.u8(self.decision);
        w.path(&self.path)?;
        for chain in &self.chains {
            require(chain.len() == self.path.components().len() && chain.len() <= 64)?;
            w.u8(u8::try_from(chain.len()).map_err(|_| Error::Invalid)?);
            for object in chain {
                w.object(object);
            }
        }
        w.state(&self.states[0]);
        w.state(&self.states[1]);
        let length =
            w.0.len()
                .checked_add(CHECKSUM_BYTES)
                .ok_or(Error::Invalid)?;
        require(length <= MAX_BYTES)?;
        w.0.get_mut(4..8).ok_or(Error::Invalid)?.copy_from_slice(
            &u32::try_from(length)
                .map_err(|_| Error::Invalid)?
                .to_le_bytes(),
        );
        let digest: [u8; 32] = Sha256::digest(&w.0).into();
        w.put(digest);
        Self::decode(&w.0)?;
        Ok(w.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        require((CHECKSUM_BYTES..=MAX_BYTES).contains(&bytes.len()))?;
        let (payload, checksum) = bytes.split_at(bytes.len() - CHECKSUM_BYTES);
        require(checksum == <[u8; 32]>::from(Sha256::digest(payload)))?;
        let mut c = Cursor {
            bytes: payload,
            at: 0,
        };
        require(c.array::<4>()? == *b"QNA1")?;
        require(usize::try_from(c.u32()?).map_err(|_| Error::Invalid)? == bytes.len())?;
        let connection = c.array()?;
        let operation = c.u64()?;
        let revision = c.u64()?;
        let fence = c.u64()?;
        let present = c.boolean()?;
        let baseline = c.u64()?;
        require(
            operation != 0
                && ((!present && baseline == 0)
                    || (present && baseline > 0 && baseline <= revision)),
        )?;
        let endpoints = [c.array()?, c.array()?];
        let direction = c.side()?;
        let decision = c.u8()?;
        require(matches!(decision, 0..=5 | 10..=14))?;
        require(!matches!(decision, 1 | 4) || direction == 0)?;
        require(!matches!(decision, 2 | 3) || direction == 1)?;
        let path = c.path()?;
        let chains = [
            c.chain(path.components().len())?,
            c.chain(path.components().len())?,
        ];
        require(
            chains
                .iter()
                .flatten()
                .all(|object| object.generation != [0; 32]),
        )?;
        let states = [c.state(false)?, c.state(false)?];
        require(states.iter().all(
            |state| !matches!(state, State::Regular { object, .. } if object.generation == [0; 32]),
        ))?;
        require(c.at == payload.len())?;
        Ok(Self {
            connection,
            operation,
            revision,
            fence,
            baseline: present.then_some(baseline),
            endpoints,
            direction,
            decision,
            path,
            chains,
            states,
        })
    }
}

/// Frozen actual versions for conflict accounting. These bytes are clues only;
/// capture and completion retain fresh native guards outside this DTO.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct CapturedVersion {
    pub side: u8,
    pub role: u8,
    pub state: State,
    pub displaced: Option<[u8; 32]>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ConflictSnapshot {
    pub connection: [u8; 16],
    pub operation: u64,
    pub admission: u64,
    pub revision: u64,
    pub fence: u64,
    pub baseline: Option<u64>,
    pub endpoints: [[u8; 64]; 2],
    pub path: RelativePath,
    pub chains: [Vec<Object>; 2],
    pub versions: Vec<CapturedVersion>,
}
impl ConflictSnapshot {
    pub fn encode(&self) -> Result<Vec<u8>> {
        require((2..=4).contains(&self.versions.len()))?;
        let mut w = Writer(Vec::with_capacity(1024));
        w.put(b"QNC1");
        w.u32(0);
        w.put(self.connection);
        w.u64(self.operation);
        w.u64(self.admission);
        w.u64(self.revision);
        w.u64(self.fence);
        w.u8(u8::from(self.baseline.is_some()));
        w.u64(self.baseline.unwrap_or(0));
        w.put(self.endpoints[0]);
        w.put(self.endpoints[1]);
        w.path(&self.path)?;
        for chain in &self.chains {
            require(chain.len() == self.path.components().len() && chain.len() <= 64)?;
            w.u8(u8::try_from(chain.len()).map_err(|_| Error::Invalid)?);
            for object in chain {
                w.object(object);
            }
        }
        w.u8(u8::try_from(self.versions.len()).map_err(|_| Error::Invalid)?);
        for version in &self.versions {
            w.u8(version.side);
            w.u8(version.role);
            w.state(&version.state);
            w.optional_digest(version.displaced.as_ref());
        }
        let length =
            w.0.len()
                .checked_add(CHECKSUM_BYTES)
                .ok_or(Error::Invalid)?;
        require(length <= MAX_BYTES)?;
        w.0.get_mut(4..8).ok_or(Error::Invalid)?.copy_from_slice(
            &u32::try_from(length)
                .map_err(|_| Error::Invalid)?
                .to_le_bytes(),
        );
        let digest: [u8; 32] = Sha256::digest(&w.0).into();
        w.put(digest);
        Self::decode(&w.0)?;
        Ok(w.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        require((CHECKSUM_BYTES..=MAX_BYTES).contains(&bytes.len()))?;
        let (payload, checksum) = bytes.split_at(bytes.len() - CHECKSUM_BYTES);
        require(checksum == <[u8; 32]>::from(Sha256::digest(payload)))?;
        let mut c = Cursor {
            bytes: payload,
            at: 0,
        };
        require(c.array::<4>()? == *b"QNC1")?;
        require(usize::try_from(c.u32()?).map_err(|_| Error::Invalid)? == bytes.len())?;
        let connection = c.array()?;
        let operation = c.u64()?;
        let admission = c.u64()?;
        let revision = c.u64()?;
        let fence = c.u64()?;
        let present = c.boolean()?;
        let baseline = c.u64()?;
        require(
            operation != 0
                && admission > 0
                && revision >= admission
                && ((!present && baseline == 0)
                    || (present && baseline > 0 && baseline < admission)),
        )?;
        let endpoints = [c.array()?, c.array()?];
        let path = c.path()?;
        let chains = [
            c.chain(path.components().len())?,
            c.chain(path.components().len())?,
        ];
        require(
            chains
                .iter()
                .flatten()
                .all(|object| object.generation != [0; 32]),
        )?;
        let count = c.u8()?;
        require((2..=4).contains(&count))?;
        let mut versions = Vec::with_capacity(usize::from(count));
        let mut previous = None;
        let mut live_sides = [false; 2];
        for _ in 0..count {
            let side = c.side()?;
            let role = c.u8()?;
            let state = c.state(true)?;
            let displaced = c.optional_digest()?;
            require(role <= 2 && previous.is_none_or(|key| key < (side, role)))?;
            require((role == 1) == displaced.is_some())?;
            require(displaced.is_none_or(|value| value != [0; 32]))?;
            match &state {
                State::Absent => require(role == 0)?,
                State::Regular { object, .. } | State::Unsupported { object, .. } => {
                    require(object.generation != [0; 32])?;
                }
                State::Unknown => return Err(Error::Invalid),
            }
            if role == 2 {
                require(matches!(state, State::Regular { .. }))?;
            }
            if role == 0 {
                *live_sides
                    .get_mut(usize::from(side))
                    .ok_or(Error::Invalid)? = true;
            }
            previous = Some((side, role));
            versions.push(CapturedVersion {
                side,
                role,
                state,
                displaced,
            });
        }
        require(live_sides == [true, true] && c.at == payload.len())?;
        Ok(Self {
            connection,
            operation,
            admission,
            revision,
            fence,
            baseline: present.then_some(baseline),
            endpoints,
            path,
            chains,
            versions,
        })
    }
    pub fn binding_digest(&self) -> Result<[u8; 32]> {
        Ok(Sha256::digest(self.encode()?).into())
    }
}
struct Cursor<'a> {
    bytes: &'a [u8],
    at: usize,
}
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8]> {
        let end = self.at.checked_add(count).ok_or(Error::Invalid)?;
        let bytes = self.bytes.get(self.at..end).ok_or(Error::Invalid)?;
        self.at = end;
        Ok(bytes)
    }
    fn array<const N: usize>(&mut self) -> Result<[u8; N]> {
        self.take(N)?.try_into().map_err(|_| Error::Invalid)
    }
    fn u8(&mut self) -> Result<u8> {
        Ok(self.array::<1>()?[0])
    }
    fn u16(&mut self) -> Result<u16> {
        Ok(u16::from_le_bytes(self.array()?))
    }
    fn u32(&mut self) -> Result<u32> {
        Ok(u32::from_le_bytes(self.array()?))
    }
    fn u64(&mut self) -> Result<u64> {
        Ok(u64::from_le_bytes(self.array()?))
    }
    fn i64(&mut self) -> Result<i64> {
        Ok(i64::from_le_bytes(self.array()?))
    }
    fn zero(&mut self, count: usize) -> Result<()> {
        require(self.take(count)?.iter().all(|v| *v == 0))
    }
    fn boolean(&mut self) -> Result<bool> {
        match self.u8()? {
            0 => Ok(false),
            1 => Ok(true),
            _ => Err(Error::Invalid),
        }
    }
    fn side(&mut self) -> Result<u8> {
        let value = self.u8()?;
        require(value <= 1)?;
        Ok(value)
    }
    fn object(&mut self) -> Result<Object> {
        let value = Object {
            dev: self.u64()?,
            ino: self.u64()?,
            kind: self.u32()?,
            generation: self.array()?,
        };
        require(matches!(
            value.kind,
            0o010_000 | 0o020_000 | 0o040_000 | 0o060_000 | 0o100_000 | 0o120_000 | 0o140_000
        ))?;
        Ok(value)
    }
    fn directory(&mut self) -> Result<Object> {
        let v = self.object()?;
        require(v.kind == 0o040_000)?;
        Ok(v)
    }
    fn regular(&mut self) -> Result<Object> {
        let v = self.object()?;
        require(v.kind == 0o100_000)?;
        Ok(v)
    }
    fn stamp(&mut self) -> Result<Stamp> {
        let v = Stamp {
            length: self.u64()?,
            modified: (self.i64()?, self.i64()?),
            changed: (self.i64()?, self.i64()?),
            mode: self.u32()?,
            uid: self.u32()?,
            gid: self.u32()?,
            links: self.u64()?,
        };
        require(
            (0..1_000_000_000).contains(&v.modified.1) && (0..1_000_000_000).contains(&v.changed.1),
        )?;
        Ok(v)
    }
    fn content(&mut self) -> Result<Content> {
        Ok(Content {
            digest: self.array()?,
            length: self.u64()?,
            executable: self.boolean()?,
        })
    }
    fn state(&mut self, outcome: bool) -> Result<State> {
        match self.u8()? {
            0 => {
                self.zero(153)?;
                Ok(State::Absent)
            }
            1 => {
                let object = self.regular()?;
                let stamp = self.stamp()?;
                let content = self.content()?;
                require(
                    stamp.length == content.length
                        && stamp.mode & 0o170_000 == object.kind
                        && (stamp.mode & 0o111 != 0) == content.executable,
                )?;
                Ok(State::Regular {
                    object,
                    stamp,
                    content,
                })
            }
            2 if outcome => {
                let object = self.object()?;
                let stamp = self.stamp()?;
                self.zero(41)?;
                require(object.kind != 0o100_000 && stamp.mode & 0o170_000 == object.kind)?;
                Ok(State::Unsupported { object, stamp })
            }
            3 if outcome => {
                self.zero(153)?;
                Ok(State::Unknown)
            }
            _ => Err(Error::Invalid),
        }
    }
    fn optional_digest(&mut self) -> Result<Option<[u8; 32]>> {
        let present = self.boolean()?;
        let value = self.array()?;
        if !present {
            require(value == [0; 32])?;
        }
        Ok(present.then_some(value))
    }
    fn artifact(&mut self) -> Result<Artifact> {
        let value = Artifact {
            endpoint: self.array()?,
            platform: self.u8()?,
            identity: self.array()?,
        };
        require(
            value.platform <= 1
                && (value.platform != 0 || value.identity[8..].iter().all(|v| *v == 0)),
        )?;
        Ok(value)
    }
    fn recovery(&mut self) -> Result<Recovery> {
        let value = Recovery {
            connection: self.array()?,
            operation: self.u64()?,
            slot: self.u32()?,
        };
        require(value.operation > 0 && value.slot < 128)?;
        Ok(value)
    }
    fn path(&mut self) -> Result<RelativePath> {
        let encoding = self.u8()?;
        let components = self.u8()?;
        let count = usize::from(self.u16()?);
        require((1..=64).contains(&components) && (1..=4096).contains(&count))?;
        let path = match encoding {
            0 => RelativePath::unix(self.take(count)?),
            1 => {
                let raw = self.take(count.checked_mul(2).ok_or(Error::Invalid)?)?;
                let units: Vec<_> = raw
                    .chunks_exact(2)
                    .map(|v| {
                        v.try_into()
                            .map(u16::from_le_bytes)
                            .map_err(|_| Error::Invalid)
                    })
                    .collect::<Result<Vec<_>>>()?;
                RelativePath::windows(&units)
            }
            _ => return Err(Error::Invalid),
        }
        .map_err(|_| Error::Invalid)?;
        require(path.components().len() == usize::from(components))?;
        Ok(path)
    }
    fn leaf(&mut self) -> Result<NativeName> {
        let encoding = self.u8()?;
        let count = usize::from(self.u16()?);
        require((1..=255).contains(&count))?;
        let path = match encoding {
            0 => RelativePath::unix(self.take(count)?),
            1 => {
                let bytes = self.take(count.checked_mul(2).ok_or(Error::Invalid)?)?;
                let units: Vec<_> = bytes
                    .chunks_exact(2)
                    .map(|v| {
                        v.try_into()
                            .map(u16::from_le_bytes)
                            .map_err(|_| Error::Invalid)
                    })
                    .collect::<Result<Vec<_>>>()?;
                RelativePath::windows(&units)
            }
            _ => return Err(Error::Invalid),
        }
        .map_err(|_| Error::Invalid)?;
        require(path.components().len() == 1)?;
        path.components().first().cloned().ok_or(Error::Invalid)
    }
    fn chain(&mut self, count: usize) -> Result<Vec<Object>> {
        let length = usize::from(self.u8()?);
        require(length == count && length <= 64)?;
        (0..length).map(|_| self.directory()).collect()
    }
    fn selection(&mut self) -> Result<Selection> {
        match self.u8()? {
            0 => {
                self.zero(190)?;
                Ok(Selection::None)
            }
            1 => {
                self.zero(32)?;
                Ok(Selection::Restore {
                    recovery: self.recovery()?,
                    artifact: self.artifact()?,
                    content: self.content()?,
                })
            }
            2 => {
                let conflict = (self.array()?, self.u64()?);
                let revision = self.u64()?;
                require(conflict.1 > 0 && revision > 0)?;
                Ok(Selection::Resolve {
                    conflict,
                    revision,
                    recovery: self.recovery()?,
                    artifact: self.artifact()?,
                    content: self.content()?,
                })
            }
            3 => {
                let conflict = (self.array()?, self.u64()?);
                let revision = self.u64()?;
                require(conflict.1 > 0 && revision > 0)?;
                self.zero(158)?;
                Ok(Selection::Tombstone { conflict, revision })
            }
            _ => Err(Error::Invalid),
        }
    }
}
fn require(value: bool) -> Result<()> {
    if value { Ok(()) } else { Err(Error::Invalid) }
}
#[expect(
    clippy::too_many_lines,
    reason = "The four bounded wire layouts remain adjacent for checked size and tag auditing."
)]
pub(crate) fn decode(bytes: &[u8]) -> Result<Descriptor> {
    require((HEADER_BYTES + CHECKSUM_BYTES..=MAX_BYTES).contains(&bytes.len()))?;
    let (payload, checksum) = bytes.split_at(bytes.len() - CHECKSUM_BYTES);
    let digest: [u8; 32] = Sha256::digest(payload).into();
    require(checksum == digest)?;
    let mut c = Cursor {
        bytes: payload,
        at: 0,
    };
    let magic = c.array::<4>()?;
    require(usize::try_from(c.u32()?).map_err(|_| Error::Invalid)? == bytes.len())?;
    let connection = c.array()?;
    let operation = c.u64()?;
    let attempt = c.u64()?;
    let nonce = c.array()?;
    let epoch = c.array()?;
    let session = c.array()?;
    let admission = c.u64()?;
    let present = c.boolean()?;
    let baseline = c.u64()?;
    let progress = c.u64()?;
    let fence = c.u64()?;
    require(
        operation > 0
            && (1..=8).contains(&attempt)
            && nonce != [0; 16]
            && admission > 0
            && progress >= admission
            && (if present {
                baseline > 0 && baseline < admission
            } else {
                baseline == 0
            }),
    )?;
    let header = Header {
        connection,
        operation,
        attempt,
        nonce,
        epoch,
        session,
        admission,
        baseline: present.then_some(baseline),
        progress,
        fence,
    };
    let body = match &magic {
        b"QNI2" => {
            let endpoints = [c.array()?, c.array()?];
            let direction = c.side()?;
            let decision = c.u8()?;
            require(matches!(decision,0..=5|10..=14))?;
            let namespace = c.directory()?;
            let path = c.path()?;
            let chains = [
                c.chain(path.components().len())?,
                c.chain(path.components().len())?,
            ];
            let expected = [c.state(false)?, c.state(false)?];
            let selection = c.selection()?;
            if matches!(selection, Selection::None) {
                require(decision != 5)?;
                require(!matches!(decision, 1 | 4) || direction == 0)?;
                require(!matches!(decision, 2 | 3) || direction == 1)?;
            }
            Body::Intent(Intent {
                endpoints,
                direction,
                decision,
                namespace,
                path,
                chains,
                expected,
                selection,
            })
        }
        b"QNS2" | b"QNS3" => {
            let intent = c.array()?;
            let object = c.regular()?;
            let content = c.content()?;
            let source = match c.u8()? {
                0 => Source::Endpoint {
                    side: c.side()?,
                    parent: c.directory()?,
                    object: c.regular()?,
                    stamp: c.stamp()?,
                },
                1 => {
                    let recovery = c.recovery()?;
                    let artifact = c.artifact()?;
                    let content = c.content()?;
                    c.zero(7)?;
                    Source::Recovery {
                        recovery,
                        artifact,
                        content,
                    }
                }
                _ => return Err(Error::Invalid),
            };
            require(magic != *b"QNS3" || matches!(source, Source::Endpoint { .. }))?;
            Body::Stage(Stage {
                intent,
                object,
                content,
                source,
            })
        }
        b"QND1" => {
            let intent = c.array()?;
            let stage = c.optional_digest()?;
            let object = c.object()?;
            let stamp = c.stamp()?;
            require(object.kind == stamp.mode & 0o170_000)?;
            let matches_expected = c.boolean()?;
            let side = c.side()?;
            let parent = c.directory()?;
            let leaf = c.leaf()?;
            let barriers = c.u8()?;
            require(barriers == 3)?;
            Body::Displaced(Displaced {
                intent,
                stage,
                object,
                stamp,
                matches_expected,
                side,
                parent,
                leaf,
                barriers,
            })
        }
        b"QNO1" => {
            let intent = c.array()?;
            let stage = c.optional_digest()?;
            let displaced = c.optional_digest()?;
            let states = [c.state(true)?, c.state(true)?];
            let kind = c.u8()?;
            let barriers = c.u8()?;
            require(kind <= 4 && barriers <= 3)?;
            let count = usize::from(c.u16()?);
            require(count <= 128)?;
            require(
                count
                    .checked_mul(38)
                    .and_then(|v| v.checked_add(8))
                    .is_some_and(|v| v == payload.len() - c.at),
            )?;
            let mut recovery = Vec::with_capacity(count);
            for _ in 0..count {
                let slot = c.u32()?;
                let side = c.side()?;
                let role = c.u8()?;
                let binding = c.array()?;
                require(
                    slot < 128
                        && role <= 3
                        && recovery.last().is_none_or(|v: &Reference| v.slot < slot),
                )?;
                recovery.push(Reference {
                    slot,
                    side,
                    role,
                    binding,
                });
            }
            let sequence = c.u64()?;
            Body::Outcome(Outcome {
                intent,
                stage,
                displaced,
                states,
                kind,
                barriers,
                recovery,
                sequence,
            })
        }
        _ => return Err(Error::Invalid),
    };
    require(c.at == payload.len())?;
    Ok(Descriptor {
        profile: match &magic {
            b"QNS2" => Profile::LegacyStage,
            b"QNS3" => Profile::CreatedRegular,
            _ => Profile::Neutral,
        },
        header,
        body,
        digest,
    })
}
pub(crate) fn validate(bytes: &[u8]) -> Result<()> {
    decode(bytes).map(|_| ())
}

/// Explicit ordinary producer encoding. This is correlation only; the live
/// Prepared consumer separately requires native creation evidence.
pub(super) fn encode_created_stage(header: &Header, stage: &Stage) -> Result<Vec<u8>> {
    require(matches!(stage.source, Source::Endpoint { .. }))?;
    let mut bytes = encode(header, &Body::Stage(stage.clone()))?;
    bytes
        .get_mut(..4)
        .ok_or(Error::Invalid)?
        .copy_from_slice(b"QNS3");
    let end = bytes.len() - CHECKSUM_BYTES;
    let digest: [u8; 32] = Sha256::digest(bytes.get(..end).ok_or(Error::Invalid)?).into();
    bytes
        .get_mut(end..)
        .ok_or(Error::Invalid)?
        .copy_from_slice(&digest);
    validate(&bytes)?;
    Ok(bytes)
}

struct Writer(Vec<u8>);
impl Writer {
    fn put(&mut self, value: impl AsRef<[u8]>) {
        self.0.extend_from_slice(value.as_ref());
    }
    fn u8(&mut self, value: u8) {
        self.0.push(value);
    }
    fn u16(&mut self, value: u16) {
        self.put(value.to_le_bytes());
    }
    fn u32(&mut self, value: u32) {
        self.put(value.to_le_bytes());
    }
    fn u64(&mut self, value: u64) {
        self.put(value.to_le_bytes());
    }
    fn object(&mut self, v: &Object) {
        self.u64(v.dev);
        self.u64(v.ino);
        self.u32(v.kind);
        self.put(v.generation);
    }
    fn stamp(&mut self, v: &Stamp) {
        self.u64(v.length);
        for n in [v.modified.0, v.modified.1, v.changed.0, v.changed.1] {
            self.put(n.to_le_bytes());
        }
        self.u32(v.mode);
        self.u32(v.uid);
        self.u32(v.gid);
        self.u64(v.links);
    }
    fn content(&mut self, v: &Content) {
        self.put(v.digest);
        self.u64(v.length);
        self.u8(u8::from(v.executable));
    }
    fn state(&mut self, v: &State) {
        match v {
            State::Absent => self.put([0; 154]),
            State::Unknown => {
                self.u8(3);
                self.put([0; 153]);
            }
            State::Regular {
                object,
                stamp,
                content,
            } => {
                self.u8(1);
                self.object(object);
                self.stamp(stamp);
                self.content(content);
            }
            State::Unsupported { object, stamp } => {
                self.u8(2);
                self.object(object);
                self.stamp(stamp);
                self.put([0; 41]);
            }
        }
    }
    fn artifact(&mut self, v: &Artifact) {
        self.put(v.endpoint);
        self.u8(v.platform);
        self.put(v.identity);
    }
    fn recovery(&mut self, v: &Recovery) {
        self.put(v.connection);
        self.u64(v.operation);
        self.u32(v.slot);
    }
    fn optional_digest(&mut self, v: Option<&[u8; 32]>) {
        self.u8(u8::from(v.is_some()));
        self.put(v.copied().unwrap_or([0; 32]));
    }
    fn path(&mut self, v: &RelativePath) -> Result<()> {
        let windows = matches!(
            v.components().first().ok_or(Error::Invalid)?,
            NativeName::Windows(_)
        );
        self.u8(u8::from(windows));
        self.u8(u8::try_from(v.components().len()).map_err(|_| Error::Invalid)?);
        let units = v
            .components()
            .iter()
            .try_fold(v.components().len() - 1, |n, c| {
                n.checked_add(match c {
                    NativeName::Unix(v) => v.len(),
                    NativeName::Windows(v) => v.len(),
                })
            })
            .ok_or(Error::Invalid)?;
        require(units <= 4096)?;
        self.u16(u16::try_from(units).map_err(|_| Error::Invalid)?);
        for (index, c) in v.components().iter().enumerate() {
            if index > 0 {
                if windows { self.u16(92) } else { self.u8(47) }
            }
            match c {
                NativeName::Unix(v) => self.put(v),
                NativeName::Windows(v) => {
                    for u in v {
                        self.u16(*u);
                    }
                }
            }
        }
        Ok(())
    }
    fn leaf(&mut self, v: &NativeName) -> Result<()> {
        let length = match v {
            NativeName::Unix(v) => v.len(),
            NativeName::Windows(v) => v.len(),
        };
        require((1..=255).contains(&length))?;
        self.u8(u8::from(matches!(v, NativeName::Windows(_))));
        self.u16(u16::try_from(length).map_err(|_| Error::Invalid)?);
        match v {
            NativeName::Unix(v) => self.put(v),
            NativeName::Windows(v) => {
                for u in v {
                    self.u16(*u);
                }
            }
        }
        Ok(())
    }
    fn selection(&mut self, v: &Selection) {
        match v {
            Selection::None => self.put([0; 191]),
            Selection::Restore {
                recovery,
                artifact,
                content,
            } => {
                self.u8(1);
                self.put([0; 32]);
                self.recovery(recovery);
                self.artifact(artifact);
                self.content(content);
            }
            Selection::Resolve {
                conflict,
                revision,
                recovery,
                artifact,
                content,
            } => {
                self.u8(2);
                self.put(conflict.0);
                self.u64(conflict.1);
                self.u64(*revision);
                self.recovery(recovery);
                self.artifact(artifact);
                self.content(content);
            }
            Selection::Tombstone { conflict, revision } => {
                self.u8(3);
                self.put(conflict.0);
                self.u64(conflict.1);
                self.u64(*revision);
                self.put([0; 158]);
            }
        }
    }
}
#[expect(
    clippy::too_many_lines,
    reason = "The four bounded wire layouts remain adjacent to their common checksum and validation."
)]
pub(crate) fn encode(header: &Header, body: &Body) -> Result<Vec<u8>> {
    let mut w = Writer(Vec::with_capacity(MAX_BYTES));
    w.put(match body {
        Body::Intent(_) => b"QNI2",
        Body::Stage(_) => b"QNS2",
        Body::Displaced(_) => b"QND1",
        Body::Outcome(_) => b"QNO1",
    });
    w.u32(0);
    w.put(header.connection);
    w.u64(header.operation);
    w.u64(header.attempt);
    w.put(header.nonce);
    w.put(header.epoch);
    w.put(header.session);
    w.u64(header.admission);
    w.u8(u8::from(header.baseline.is_some()));
    w.u64(header.baseline.unwrap_or(0));
    w.u64(header.progress);
    w.u64(header.fence);
    match body {
        Body::Intent(v) => {
            w.put(v.endpoints[0]);
            w.put(v.endpoints[1]);
            w.u8(v.direction);
            w.u8(v.decision);
            w.object(&v.namespace);
            w.path(&v.path)?;
            for chain in &v.chains {
                require(chain.len() == v.path.components().len() && chain.len() <= 64)?;
                w.u8(u8::try_from(chain.len()).map_err(|_| Error::Invalid)?);
                for object in chain {
                    w.object(object);
                }
            }
            w.state(&v.expected[0]);
            w.state(&v.expected[1]);
            w.selection(&v.selection);
        }
        Body::Stage(v) => {
            w.put(v.intent);
            w.object(&v.object);
            w.content(&v.content);
            match &v.source {
                Source::Endpoint {
                    side,
                    parent,
                    object,
                    stamp,
                } => {
                    w.u8(0);
                    w.u8(*side);
                    w.object(parent);
                    w.object(object);
                    w.stamp(stamp);
                }
                Source::Recovery {
                    recovery,
                    artifact,
                    content,
                } => {
                    w.u8(1);
                    w.recovery(recovery);
                    w.artifact(artifact);
                    w.content(content);
                    w.put([0; 7]);
                }
            }
        }
        Body::Displaced(v) => {
            w.put(v.intent);
            w.optional_digest(v.stage.as_ref());
            w.object(&v.object);
            w.stamp(&v.stamp);
            w.u8(u8::from(v.matches_expected));
            w.u8(v.side);
            w.object(&v.parent);
            w.leaf(&v.leaf)?;
            w.u8(v.barriers);
        }
        Body::Outcome(v) => {
            require(v.recovery.len() <= 128)?;
            w.put(v.intent);
            w.optional_digest(v.stage.as_ref());
            w.optional_digest(v.displaced.as_ref());
            w.state(&v.states[0]);
            w.state(&v.states[1]);
            w.u8(v.kind);
            w.u8(v.barriers);
            w.u16(u16::try_from(v.recovery.len()).map_err(|_| Error::Invalid)?);
            for r in &v.recovery {
                w.u32(r.slot);
                w.u8(r.side);
                w.u8(r.role);
                w.put(r.binding);
            }
            w.u64(v.sequence);
        }
    }
    let length =
        w.0.len()
            .checked_add(CHECKSUM_BYTES)
            .ok_or(Error::Invalid)?;
    require(length <= MAX_BYTES)?;
    w.0.get_mut(4..8).ok_or(Error::Invalid)?.copy_from_slice(
        &u32::try_from(length)
            .map_err(|_| Error::Invalid)?
            .to_le_bytes(),
    );
    let digest: [u8; 32] = Sha256::digest(&w.0).into();
    w.put(digest);
    validate(&w.0)?;
    Ok(w.0)
}
impl Descriptor {
    /// Structural linkage only. The caller must fetch each exact recovery row
    /// and freshly inspect native objects before issuing any live proof.
    #[expect(
        clippy::too_many_lines,
        reason = "The four descriptor joins are audited together against one intent and attempt."
    )]
    pub fn joined(
        &self,
        intent: &Descriptor,
        stage: Option<&Descriptor>,
        displaced: Option<&Descriptor>,
    ) -> Result<()> {
        let Body::Intent(intent_body) = &intent.body else {
            return Err(Error::Invalid);
        };
        require(self.header.same_attempt(&intent.header))?;
        let target = 1_usize
            .checked_sub(usize::from(intent_body.direction))
            .ok_or(Error::Invalid)?;
        match &self.body {
            Body::Intent(_) => require(self == intent),
            Body::Stage(stage_record) => {
                require(stage_record.intent == intent.digest)?;
                if self.profile == Profile::CreatedRegular {
                    require(
                        matches!(intent_body.decision, 1 | 2)
                            && matches!(intent_body.selection, Selection::None)
                            && matches!(stage_record.source, Source::Endpoint { .. }),
                    )?;
                }
                match (&stage_record.source, &intent_body.selection) {
                    (
                        Source::Endpoint {
                            side,
                            parent,
                            object,
                            stamp,
                        },
                        Selection::None,
                    ) => {
                        require(
                            *side == intent_body.direction
                                && Some(parent)
                                    == intent_body
                                        .chains
                                        .get(usize::from(*side))
                                        .ok_or(Error::Invalid)?
                                        .last(),
                        )?;
                        let State::Regular {
                            object: expected,
                            stamp: before,
                            content,
                        } = intent_body
                            .expected
                            .get(usize::from(*side))
                            .ok_or(Error::Invalid)?
                        else {
                            return Err(Error::Invalid);
                        };
                        require(
                            object == expected
                                && stamp == before
                                && content == &stage_record.content,
                        )
                    }
                    (
                        Source::Recovery {
                            recovery,
                            artifact,
                            content,
                        },
                        Selection::Restore {
                            recovery: recovery_id,
                            artifact: artifact_id,
                            content: recovery_content,
                        }
                        | Selection::Resolve {
                            recovery: recovery_id,
                            artifact: artifact_id,
                            content: recovery_content,
                            ..
                        },
                    ) => require(
                        recovery == recovery_id
                            && artifact == artifact_id
                            && content == recovery_content
                            && content == &stage_record.content,
                    ),
                    _ => Err(Error::Invalid),
                }
            }
            Body::Displaced(displaced_record) => {
                require(
                    displaced_record.intent == intent.digest
                        && usize::from(displaced_record.side) == target
                        && intent_body.chains.get(target).ok_or(Error::Invalid)?.last()
                            == Some(&displaced_record.parent)
                        && intent_body.path.components().last() == Some(&displaced_record.leaf),
                )?;
                match (displaced_record.stage, stage) {
                    (None, None) => {}
                    (Some(hash), Some(stage_record)) => {
                        require(hash == stage_record.digest)?;
                        stage_record.joined(intent, None, None)?;
                    }
                    _ => return Err(Error::Invalid),
                }
                if displaced_record.matches_expected {
                    let State::Regular { object, stamp, .. } =
                        intent_body.expected.get(target).ok_or(Error::Invalid)?
                    else {
                        return Err(Error::Invalid);
                    };
                    require(
                        object == &displaced_record.object
                            && relocated_stamp(stamp, &displaced_record.stamp),
                    )?;
                }
                Ok(())
            }
            Body::Outcome(outcome) => {
                require(outcome.intent == intent.digest)?;
                match (outcome.stage, stage) {
                    (None, None) => {}
                    (Some(hash), Some(stage_record)) => {
                        require(hash == stage_record.digest)?;
                        stage_record.joined(intent, None, None)?;
                    }
                    _ => return Err(Error::Invalid),
                }
                match (outcome.displaced, displaced) {
                    (None, None) => {}
                    (Some(hash), Some(displaced_record)) => {
                        require(hash == displaced_record.digest)?;
                        displaced_record.joined(intent, stage, None)?;
                    }
                    _ => return Err(Error::Invalid),
                }
                if outcome.kind <= 2 {
                    require(outcome.barriers == 3)?;
                    if outcome.kind == 2 {
                        if matches!(intent_body.selection, Selection::Tombstone { .. }) {
                            require(
                                stage.is_none()
                                    && outcome.states.get(target) == Some(&State::Absent)
                                    && outcome.states.get(usize::from(intent_body.direction))
                                        == intent_body
                                            .expected
                                            .get(usize::from(intent_body.direction))
                                    && (displaced.is_some()
                                        || intent_body.expected.get(target)
                                            == Some(&State::Absent)),
                            )?;
                        } else {
                            require(
                                outcome.states == [State::Absent, State::Absent]
                                    && stage.is_none()
                                    && displaced.is_some(),
                            )?;
                        }
                    } else {
                        let Some(Descriptor {
                            body: Body::Stage(stage_record),
                            ..
                        }) = stage
                        else {
                            return Err(Error::Invalid);
                        };
                        let State::Regular {
                            object, content, ..
                        } = outcome.states.get(target).ok_or(Error::Invalid)?
                        else {
                            return Err(Error::Invalid);
                        };
                        require(
                            object == &stage_record.object && content == &stage_record.content,
                        )?;
                        let source = outcome
                            .states
                            .get(usize::from(intent_body.direction))
                            .ok_or(Error::Invalid)?;
                        if matches!(intent_body.selection, Selection::None) {
                            let State::Regular {
                                content: source, ..
                            } = source
                            else {
                                return Err(Error::Invalid);
                            };
                            require(source == content)?;
                        } else {
                            require(
                                Some(source)
                                    == intent_body.expected.get(usize::from(intent_body.direction)),
                            )?;
                        }
                        require(
                            (outcome.kind == 0
                                && displaced.is_none()
                                && intent_body.expected.get(target) == Some(&State::Absent))
                                || (outcome.kind == 1 && displaced.is_some()),
                        )?;
                    }
                }
                Ok(())
            }
        }
    }
}
fn relocated_stamp(a: &Stamp, b: &Stamp) -> bool {
    a.length == b.length
        && a.modified == b.modified
        && a.mode == b.mode
        && a.uid == b.uid
        && a.gid == b.gid
        && a.links == b.links
}

/// QRB1 is an exact artifact/source fact, not a reconstructed native proof.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RecoveryFact {
    pub intent: [u8; 32],
    pub recovery: Recovery,
    pub side: u8,
    pub role: u8,
    pub endpoint: [u8; 64],
    pub object: Object,
    pub content: Content,
    pub artifact: Artifact,
}
impl RecoveryFact {
    pub fn encode(&self) -> Result<Vec<u8>> {
        let mut w = Writer(Vec::with_capacity(312));
        w.put(b"QRB1");
        w.put(self.intent);
        w.recovery(&self.recovery);
        w.u8(self.side);
        w.u8(self.role);
        w.put(self.endpoint);
        w.object(&self.object);
        w.content(&self.content);
        w.artifact(&self.artifact);
        Self::decode(&w.0)?;
        Ok(w.0)
    }
    pub fn decode(bytes: &[u8]) -> Result<Self> {
        require(bytes.len() == 312)?;
        let mut c = Cursor { bytes, at: 0 };
        require(c.take(4)? == b"QRB1")?;
        let intent = c.array()?;
        let recovery = c.recovery()?;
        let side = c.side()?;
        let role = c.u8()?;
        require(role <= 3)?;
        let endpoint = c.array()?;
        let object = c.regular()?;
        let content = c.content()?;
        let artifact = c.artifact()?;
        require(c.at == bytes.len())?;
        Ok(Self {
            intent,
            recovery,
            side,
            role,
            endpoint,
            object,
            content,
            artifact,
        })
    }
    pub fn binding_digest(&self) -> Result<[u8; 32]> {
        Ok(Sha256::digest(self.encode()?).into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn object(kind: u32) -> Vec<u8> {
        let mut value = vec![0; 52];
        value[8..16].copy_from_slice(&7_u64.to_le_bytes());
        value[16..20].copy_from_slice(&kind.to_le_bytes());
        value[20..].fill(9);
        value
    }
    fn seal(mut bytes: Vec<u8>) -> Vec<u8> {
        let length = u32::try_from(bytes.len() + 32).unwrap();
        bytes[4..8].copy_from_slice(&length.to_le_bytes());
        let checksum: [u8; 32] = Sha256::digest(&bytes).into();
        bytes.extend(checksum);
        bytes
    }
    fn header(magic: [u8; 4]) -> Vec<u8> {
        let mut value = vec![0; 121];
        value[..4].copy_from_slice(&magic);
        value[8..24].fill(1);
        value[24..32].copy_from_slice(&1_u64.to_le_bytes());
        value[32..40].copy_from_slice(&1_u64.to_le_bytes());
        value[40..56].fill(2);
        value[56..72].fill(3);
        value[72..88].fill(4);
        value[88..96].copy_from_slice(&2_u64.to_le_bytes());
        value[105..113].copy_from_slice(&3_u64.to_le_bytes());
        value
    }
    fn regular() -> Vec<u8> {
        let mut value = vec![1];
        value.extend(object(0o100_000));
        let mut stamp = vec![0; 60];
        stamp[40..44].copy_from_slice(&0o100_600_u32.to_le_bytes());
        stamp[52..60].copy_from_slice(&1_u64.to_le_bytes());
        value.extend(stamp);
        value.extend([0; 41]);
        value
    }
    fn intent(path: &[u16], windows: bool, components: u8) -> Vec<u8> {
        let mut bytes = header(*b"QNI2");
        bytes.extend([5; 128]);
        bytes.extend([0, 1]); // Source-to-destination propagation.
        bytes.extend(object(0o040_000));
        bytes.extend([u8::from(windows), components]);
        bytes.extend(u16::try_from(path.len()).unwrap().to_le_bytes());
        for unit in path {
            if windows {
                bytes.extend(unit.to_le_bytes());
            } else {
                bytes.push(u8::try_from(*unit).unwrap());
            }
        }
        for _ in 0..2 {
            bytes.push(components);
            for _ in 0..components {
                bytes.extend(object(0o040_000));
            }
        }
        bytes.extend(regular());
        bytes.extend([0; 154]);
        bytes.extend([0; 191]);
        seal(bytes)
    }
    #[test]
    fn admission_metadata_is_bounded_lossless_and_distinct_from_native_authority() {
        let lengths = [vec![255; 15], vec![160], vec![1; 48]].concat();
        let mut path = Vec::new();
        for length in lengths {
            if !path.is_empty() {
                path.push(92);
            }
            path.extend(vec![120; length]);
        }
        let descriptor = decode(&intent(&path, true, 64)).unwrap();
        let Body::Intent(body) = descriptor.body else {
            panic!("intent");
        };
        let fact = Admission {
            connection: [8; 16],
            operation: 42,
            revision: 7,
            fence: 0,
            baseline: None,
            endpoints: body.endpoints,
            direction: body.direction,
            decision: body.decision,
            path: body.path,
            chains: body.chains,
            states: body.expected,
        };
        let encoded = fact.encode().unwrap();
        assert!(encoded.len() <= MAX_BYTES);
        assert_eq!(Admission::decode(&encoded).unwrap(), fact);
        let mut selected_initial = fact.clone();
        selected_initial.decision = 5;
        assert_eq!(
            Admission::decode(
                &selected_initial
                    .encode()
                    .expect("selected initial-preview metadata must remain truthful")
            )
            .unwrap(),
            selected_initial
        );
        assert!(
            decode(&encoded).is_err(),
            "historical admission cannot decode as a native namespace proof"
        );
        for prefix in 0..encoded.len() {
            assert!(Admission::decode(&encoded[..prefix]).is_err());
        }
        let mut trailing = encoded;
        trailing.push(0);
        assert!(Admission::decode(&trailing).is_err());
        let mut too_deep = fact.clone();
        too_deep.chains[0].push(too_deep.chains[1][0].clone());
        assert!(too_deep.encode().is_err());
        let mut unknown = fact;
        unknown.states[0] = State::Unknown;
        assert!(unknown.encode().is_err());
    }
    #[test]
    fn conflict_snapshot_retains_distinct_same_side_versions_and_rejects_unknowns() {
        let decoded = decode(&intent(&[120], false, 1)).unwrap();
        let Body::Intent(body) = decoded.body else {
            panic!("intent");
        };
        let mut fact = ConflictSnapshot {
            connection: [8; 16],
            operation: 42,
            admission: 2,
            revision: 7,
            fence: 0,
            baseline: None,
            endpoints: body.endpoints,
            path: body.path,
            chains: body.chains,
            versions: vec![
                CapturedVersion {
                    side: 0,
                    role: 0,
                    state: body.expected[0].clone(),
                    displaced: None,
                },
                CapturedVersion {
                    side: 1,
                    role: 0,
                    state: body.expected[0].clone(),
                    displaced: None,
                },
                CapturedVersion {
                    side: 1,
                    role: 1,
                    state: body.expected[0].clone(),
                    displaced: Some([9; 32]),
                },
            ],
        };
        if let State::Regular { object, .. } = &mut fact.versions[2].state {
            object.generation = [11; 32];
        }
        let encoded = fact
            .encode()
            .expect("bounded conflict snapshots must encode additional actual versions");
        assert_eq!(ConflictSnapshot::decode(&encoded).unwrap(), fact);
        assert!(Admission::decode(&encoded).is_err());
        assert!(decode(&encoded).is_err());
        for prefix in 0..encoded.len() {
            assert!(ConflictSnapshot::decode(&encoded[..prefix]).is_err());
        }
        let mut bad = fact.clone();
        bad.versions[0].state = State::Unknown;
        assert!(bad.encode().is_err());
        let mut bad = fact.clone();
        bad.versions[2].role = 0;
        assert!(bad.encode().is_err());
        let mut bad = fact.clone();
        bad.versions[2].displaced = None;
        assert!(bad.encode().is_err());
        let mut bad = fact;
        bad.versions.push(bad.versions[2].clone());
        bad.versions.push(bad.versions[2].clone());
        assert!(bad.encode().is_err());
    }
    #[test]
    fn selected_intent_preserves_observed_decision_and_joins_only_exact_private_source() {
        let original = decode(&intent(&[120], false, 1)).unwrap();
        let Body::Intent(mut body) = original.body.clone() else {
            panic!("intent");
        };
        let State::Regular {
            object, content, ..
        } = body.expected[0].clone()
        else {
            panic!("regular");
        };
        let recovery = Recovery {
            connection: original.header.connection,
            operation: 7,
            slot: 2,
        };
        let artifact = Artifact {
            endpoint: [5; 64],
            platform: 1,
            identity: [9; 24],
        };
        body.selection = Selection::Restore {
            recovery: recovery.clone(),
            artifact: artifact.clone(),
            content: content.clone(),
        };
        for decision in [0, 1, 2, 3, 4, 5, 10, 11, 12, 13, 14] {
            for direction in [0, 1] {
                body.decision = decision;
                body.direction = direction;
                let bytes = encode(&original.header, &Body::Intent(body.clone()))
                    .expect("selected target is independent of observed reconciliation direction");
                let selected = decode(&bytes).unwrap();
                assert_eq!(selected.body, Body::Intent(body.clone()));
                let stage = Stage {
                    intent: selected.digest,
                    object: object.clone(),
                    content: content.clone(),
                    source: Source::Recovery {
                        recovery: recovery.clone(),
                        artifact: artifact.clone(),
                        content: content.clone(),
                    },
                };
                let staged =
                    decode(&encode(&selected.header, &Body::Stage(stage.clone())).unwrap())
                        .unwrap();
                assert!(staged.joined(&selected, None, None).is_ok());
                let legacy = encode(&selected.header, &Body::Stage(stage.clone())).unwrap();
                let mut forged = legacy[..legacy.len() - 32].to_vec();
                forged[..4].copy_from_slice(b"QNS3");
                assert!(
                    decode(&seal(forged)).is_err(),
                    "selected recovery cannot claim the ordinary created-stage profile"
                );
                let mut wrong = stage;
                let Source::Recovery { recovery, .. } = &mut wrong.source else {
                    panic!("private source");
                };
                recovery.slot += 1;
                let wrong =
                    decode(&encode(&selected.header, &Body::Stage(wrong)).unwrap()).unwrap();
                assert!(wrong.joined(&selected, None, None).is_err());
            }
        }
        body.selection = Selection::None;
        body.decision = 5;
        assert!(
            encode(&original.header, &Body::Intent(body.clone())).is_err(),
            "initial preview cannot become ordinary execution"
        );
        body.decision = 1;
        body.direction = 1;
        assert!(
            encode(&original.header, &Body::Intent(body)).is_err(),
            "ordinary propagation still binds its direction"
        );
    }
    #[test]
    fn selected_outcome_records_target_success_without_claiming_both_sides_equal() {
        let original = decode(&intent(&[120], false, 1)).unwrap();
        let Body::Intent(mut body) = original.body.clone() else {
            panic!("intent");
        };
        let State::Regular {
            object,
            stamp,
            content,
        } = body.expected[0].clone()
        else {
            panic!("regular");
        };
        let mut chosen = content;
        chosen.digest = [17; 32];
        let recovery = Recovery {
            connection: original.header.connection,
            operation: 7,
            slot: 2,
        };
        let artifact = Artifact {
            endpoint: [5; 64],
            platform: 1,
            identity: [9; 24],
        };
        body.selection = Selection::Restore {
            recovery: recovery.clone(),
            artifact: artifact.clone(),
            content: chosen.clone(),
        };
        for source_absent in [false, true] {
            if source_absent {
                body.expected[0] = State::Absent;
            }
            let intent =
                decode(&encode(&original.header, &Body::Intent(body.clone())).unwrap()).unwrap();
            let staged = Stage {
                intent: intent.digest,
                object: object.clone(),
                content: chosen.clone(),
                source: Source::Recovery {
                    recovery: recovery.clone(),
                    artifact: artifact.clone(),
                    content: chosen.clone(),
                },
            };
            let stage = decode(&encode(&intent.header, &Body::Stage(staged)).unwrap()).unwrap();
            let outcome = Outcome {
                intent: intent.digest,
                stage: Some(stage.digest),
                displaced: None,
                states: [
                    body.expected[0].clone(),
                    State::Regular {
                        object: object.clone(),
                        stamp: stamp.clone(),
                        content: chosen.clone(),
                    },
                ],
                kind: 0,
                barriers: 3,
                recovery: vec![],
                sequence: 1,
            };
            let descriptor =
                decode(&encode(&intent.header, &Body::Outcome(outcome.clone())).unwrap()).unwrap();
            assert!(
                descriptor.joined(&intent, Some(&stage), None).is_ok(),
                "selected target success does not require matching the unchanged other endpoint"
            );
            let mut drift = outcome;
            drift.states[0] = if source_absent {
                original_regular(&original)
            } else {
                State::Absent
            };
            let changed = decode(&encode(&intent.header, &Body::Outcome(drift)).unwrap()).unwrap();
            assert!(changed.joined(&intent, Some(&stage), None).is_err());
        }
    }
    #[test]
    fn selected_tombstone_outcome_preserves_other_endpoint_and_accounts_target() {
        let original = decode(&intent(&[120], false, 1)).unwrap();
        for target_present in [false, true] {
            let Body::Intent(mut body) = original.body.clone() else {
                panic!("intent");
            };
            let State::Regular { object, stamp, .. } = body.expected[0].clone() else {
                panic!("regular");
            };
            body.selection = Selection::Tombstone {
                conflict: (original.header.connection, 7),
                revision: 2,
            };
            if target_present {
                body.expected[1] = body.expected[0].clone();
            }
            let intent =
                decode(&encode(&original.header, &Body::Intent(body.clone())).unwrap()).unwrap();
            let displaced = target_present.then(|| {
                decode(
                    &encode(
                        &intent.header,
                        &Body::Displaced(Displaced {
                            intent: intent.digest,
                            stage: None,
                            object,
                            stamp,
                            matches_expected: true,
                            side: 1,
                            parent: body.chains[1].last().unwrap().clone(),
                            leaf: NativeName::Unix(vec![120]),
                            barriers: 3,
                        }),
                    )
                    .unwrap(),
                )
                .unwrap()
            });
            let outcome = Outcome {
                intent: intent.digest,
                stage: None,
                displaced: displaced.as_ref().map(|value| value.digest),
                states: [body.expected[0].clone(), State::Absent],
                kind: 2,
                barriers: 3,
                recovery: vec![],
                sequence: 1,
            };
            let value =
                decode(&encode(&intent.header, &Body::Outcome(outcome.clone())).unwrap()).unwrap();
            assert!(
                value.joined(&intent, None, displaced.as_ref()).is_ok(),
                "selected tombstone accounts only its target, retaining the other exact state"
            );
            let mut drift = outcome.clone();
            drift.states[0] = State::Absent;
            assert!(
                decode(&encode(&intent.header, &Body::Outcome(drift)).unwrap())
                    .unwrap()
                    .joined(&intent, None, displaced.as_ref())
                    .is_err()
            );
            if target_present {
                let mut missing = outcome;
                missing.displaced = None;
                assert!(
                    decode(&encode(&intent.header, &Body::Outcome(missing)).unwrap())
                        .unwrap()
                        .joined(&intent, None, None)
                        .is_err()
                );
            }
        }
    }
    fn original_regular(intent: &Descriptor) -> State {
        let Body::Intent(body) = &intent.body else {
            panic!("intent");
        };
        body.expected[0].clone()
    }
    #[test]
    fn descriptor_decodes_lossless_maximum_path_and_both_ancestor_chains() {
        let lengths = [vec![255; 15], vec![160], vec![1; 48]].concat();
        let mut path = Vec::new();
        for length in lengths {
            if !path.is_empty() {
                path.push(92);
            }
            path.extend(vec![120; length]);
        }
        assert_eq!(path.len(), 4096);
        let encoded = intent(&path, true, 64);
        assert_eq!(encoded.len(), 15688);
        assert_eq!(validate(&encoded), Ok(()));
        let unix: Vec<_> = path
            .iter()
            .map(|v| if *v == 92 { 47 } else { *v })
            .collect();
        assert_eq!(intent(&unix, false, 64).len(), 11592);
        assert_eq!(validate(&intent(&unix, false, 64)), Ok(()));
        assert_eq!(validate(&intent(&[120, 0xd800], true, 1)), Ok(()));
        assert_eq!(validate(&intent(&[255], false, 1)), Ok(()));
    }
    #[test]
    fn descriptor_rejects_corruption_trailing_bytes_and_noncanonical_counts() {
        let bytes = intent(&[120], false, 1);
        assert_eq!(validate(&bytes), Ok(()));
        for index in [0, 4, 8, 24, 40, 120, 200] {
            let mut corrupt = bytes.clone();
            corrupt[index] ^= 1;
            assert!(validate(&corrupt).is_err());
        }
        let mut extra = bytes.clone();
        extra.push(0);
        assert!(validate(&extra).is_err());
        for (index, value) in [(32, 0), (32, 9), (96, 2), (97, 1), (304, 65), (305, 0)] {
            let mut invalid = bytes[..bytes.len() - 32].to_vec();
            invalid[index] = value;
            assert!(validate(&seal(invalid)).is_err(), "index {index}");
        }
        assert!(validate(&intent(&[46, 46], false, 1)).is_err());
        assert!(validate(&intent(&[120, 47, 120], false, 1)).is_err());
        assert!(validate(&intent(&vec![120; 256], false, 1)).is_err());
    }
    fn stage(intent: &[u8]) -> Vec<u8> {
        let mut bytes = header(*b"QNS2");
        bytes.extend(&intent[intent.len() - 32..]);
        bytes.extend(object(0o100_000));
        bytes.extend([0; 41]);
        bytes.extend([0, 0]);
        bytes.extend(object(0o040_000));
        bytes.extend(&regular()[1..113]);
        seal(bytes)
    }
    #[test]
    fn ordinary_created_stage_profile_decodes_and_binds_exact_stage_digest() {
        let intent_bytes = intent(&[120], false, 1);
        let legacy_bytes = stage(&intent_bytes);
        let mut payload = legacy_bytes[..legacy_bytes.len() - 32].to_vec();
        payload[..4].copy_from_slice(b"QNS3");
        let created_bytes = seal(payload);
        let created = decode(&created_bytes).expect("ordinary created-stage profile");
        let intent = decode(&intent_bytes).unwrap();
        assert_eq!(created.joined(&intent, None, None), Ok(()));
        let legacy = decode(&legacy_bytes).unwrap();
        let displaced = decode(&displaced(&intent_bytes, &legacy_bytes)).unwrap();
        assert!(displaced.joined(&intent, Some(&created), None).is_err());
        assert_eq!(displaced.joined(&intent, Some(&legacy), None), Ok(()));
        assert_eq!(encode(&legacy.header, &legacy.body).unwrap(), legacy_bytes);
        let Body::Stage(body) = &created.body else {
            panic!("stage");
        };
        assert_eq!(
            encode_created_stage(&created.header, body).unwrap(),
            created_bytes
        );
        // The neutral K/O formats bind the exact selected stage profile digest.
        let legacy_k_bytes = super::tests::displaced(&intent_bytes, &legacy_bytes);
        let new_k_bytes = super::tests::displaced(&intent_bytes, &created_bytes);
        let new_k = decode(&new_k_bytes).unwrap();
        assert_eq!(new_k.joined(&intent, Some(&created), None), Ok(()));
        assert!(new_k.joined(&intent, Some(&legacy), None).is_err());
        for (stage_bytes, k_bytes, good_stage, good_k, wrong_stage) in [
            (
                &legacy_bytes,
                &legacy_k_bytes,
                &legacy,
                &displaced,
                &created,
            ),
            (&created_bytes, &new_k_bytes, &created, &new_k, &legacy),
        ] {
            let outcome = decode(&outcome(&intent_bytes, stage_bytes, k_bytes, 1)).unwrap();
            assert_eq!(
                outcome.joined(&intent, Some(good_stage), Some(good_k)),
                Ok(())
            );
            assert!(
                outcome
                    .joined(&intent, Some(wrong_stage), Some(good_k))
                    .is_err()
            );
        }
    }
    fn displaced(intent: &[u8], stage: &[u8]) -> Vec<u8> {
        let mut bytes = header(*b"QND1");
        bytes.extend(&intent[intent.len() - 32..]);
        bytes.push(1);
        bytes.extend(&stage[stage.len() - 32..]);
        bytes.extend(&regular()[1..113]);
        bytes.extend([0, 1]);
        bytes.extend(object(0o040_000));
        bytes.extend([0, 1, 0, b'x', 3]);
        seal(bytes)
    }
    fn outcome(intent: &[u8], stage: &[u8], displaced: &[u8], count: u16) -> Vec<u8> {
        let mut bytes = header(*b"QNO1");
        bytes.extend(&intent[intent.len() - 32..]);
        bytes.push(1);
        bytes.extend(&stage[stage.len() - 32..]);
        bytes.push(1);
        bytes.extend(&displaced[displaced.len() - 32..]);
        bytes.extend(regular());
        bytes.extend(regular());
        bytes.extend([1, 3]);
        bytes.extend(count.to_le_bytes());
        for slot in 0..count {
            bytes.extend(u32::from(slot).to_le_bytes());
            bytes.extend([1, 1]);
            bytes.extend([8; 32]);
        }
        bytes.extend(9_u64.to_le_bytes());
        seal(bytes)
    }
    #[test]
    fn codec_roundtrips_all_four_whole_file_layouts_and_exact_recovery_limit() {
        let intent = intent(&[120], false, 1);
        let stage = stage(&intent);
        let displaced = displaced(&intent, &stage);
        let final_bytes = outcome(&intent, &stage, &displaced, 128);
        assert_eq!(stage.len(), 444);
        assert_eq!(displaced.len(), 389);
        assert_eq!(final_bytes.len(), 5435);
        for bytes in [&intent, &stage, &displaced, &final_bytes] {
            let decoded = decode(bytes).unwrap();
            assert_eq!(encode(&decoded.header, &decoded.body).unwrap(), *bytes);
        }
        assert!(validate(&outcome(&intent, &stage, &displaced, 129)).is_err());
    }
    #[test]
    fn join_rejects_operation_attempt_path_and_descriptor_substitution() {
        let intent_bytes = intent(&[120], false, 1);
        let stage_bytes = stage(&intent_bytes);
        let intent = decode(&intent_bytes).unwrap();
        let stage = decode(&stage_bytes).unwrap();
        assert_eq!(stage.joined(&intent, None, None), Ok(()));
        for index in [8, 24, 32, 40, 88] {
            let mut bytes = stage_bytes[..stage_bytes.len() - 32].to_vec();
            bytes[index] ^= 1;
            if let Ok(changed) = decode(&seal(bytes)) {
                assert!(changed.joined(&intent, None, None).is_err());
            }
        }
        let mut changed = intent_bytes[..intent_bytes.len() - 32].to_vec();
        changed[307] = b'y';
        let changed = decode(&seal(changed)).unwrap();
        assert!(stage.joined(&changed, None, None).is_err());
        let mut changed = stage_bytes[..stage_bytes.len() - 32].to_vec();
        changed[247] = 1;
        let changed = decode(&seal(changed)).unwrap();
        assert!(changed.joined(&intent, None, None).is_err());
    }

    #[test]
    fn recovery_fact_binds_actual_version_artifact_and_full_windows_identity() {
        let mut raw = Vec::new();
        raw.extend(b"QRB1");
        raw.extend([1; 32]);
        raw.extend([2; 16]);
        raw.extend(3_u64.to_le_bytes());
        raw.extend(4_u32.to_le_bytes());
        raw.extend([1, 2]);
        raw.extend([5; 64]);
        raw.extend(object(0o100_000));
        raw.extend([6; 32]);
        raw.extend(7_u64.to_le_bytes());
        raw.push(0);
        raw.extend([8; 64]);
        raw.push(1);
        raw.extend(9_u64.to_le_bytes());
        raw.extend([10; 16]);
        assert_eq!(raw.len(), 312);
        let fact = RecoveryFact::decode(&raw).unwrap();
        assert_eq!(fact.encode().unwrap(), raw);
        let digest = fact.binding_digest().unwrap();
        for offset in [4, 36, 52, 60, 64, 65, 66, 138, 182, 214, 223, 288, 311] {
            let mut changed = raw.clone();
            changed[offset] ^= 1;
            if let Ok(fact) = RecoveryFact::decode(&changed) {
                assert_ne!(fact.binding_digest().unwrap(), digest, "offset {offset}");
            }
        }
        let mut unix = raw.clone();
        unix[287] = 0;
        assert!(RecoveryFact::decode(&unix).is_err()); // Windows upper ID cannot masquerade as Unix padding.
        assert!(RecoveryFact::decode(&raw[..311]).is_err());
        raw.push(0);
        assert!(RecoveryFact::decode(&raw).is_err());
    }
}
