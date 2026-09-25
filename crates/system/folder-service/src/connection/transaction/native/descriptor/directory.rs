//! Directory history has its own bodies; no decoded value is live authority.
use super::{Cursor, Error, Header, Object, RelativePath, Result, Sha256, Stamp, require};
use sha2::Digest;

pub(crate) const PROFILE: [u8; 32] = [
    0xc6, 0x15, 0xd4, 0x56, 0x01, 0x07, 0x81, 0xe6, 0x5a, 0xa1, 0xe8, 0x80, 0xfc, 0x28, 0x6e, 0x33,
    0x27, 0x7d, 0xbf, 0x3e, 0x1a, 0x2f, 0xad, 0x84, 0x18, 0x22, 0xee, 0x7a, 0x37, 0x94, 0x0f, 0x81,
];
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Leaf {
    pub object: Object,
    pub stamp: Stamp,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Intent {
    pub endpoints: [[u8; 64]; 2],
    pub direction: u8,
    pub decision: u8,
    pub namespace: Object,
    pub admission: [u8; 32],
    pub path: RelativePath,
    pub chains: [Vec<Object>; 2],
    pub states: [Option<Leaf>; 2],
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Stage {
    pub intent: [u8; 32],
    pub created: Object,
    pub source: Object,
    pub target_parent: Object,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Body {
    Intent(Intent),
    Stage(Stage),
    Outcome(Outcome),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Outcome {
    pub intent: [u8; 32],
    pub stage: [u8; 32],
    pub states: [Object; 2],
    pub sequence: u64,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Admission {
    pub connection: [u8; 16],
    pub operation: u64,
    pub revision: u64,
    pub fence: u64,
    pub baseline: u64,
    pub endpoints: [[u8; 64]; 2],
    pub direction: u8,
    pub decision: u8,
    pub path: RelativePath,
    pub chains: [Vec<Object>; 2],
    pub states: [Option<Leaf>; 2],
}
impl Admission {
    pub(crate) fn decode(bytes: &[u8]) -> Result<Self> {
        let value = HistoricalAdmission::parse(bytes, ParseMode::Native)?;
        Ok(Self {
            connection: value.connection,
            operation: value.operation,
            revision: value.revision,
            fence: value.fence,
            baseline: value.baseline.ok_or(Error::Invalid)?,
            endpoints: value.endpoints,
            direction: value.direction,
            decision: value.decision,
            path: value.path,
            chains: value.chains,
            states: value.states,
        })
    }
    pub(crate) fn joined(&self, bytes: &[u8], intent: &Descriptor) -> Result<()> {
        HistoricalAdmission {
            connection: self.connection,
            operation: self.operation,
            revision: self.revision,
            fence: self.fence,
            baseline: Some(self.baseline),
            endpoints: self.endpoints,
            direction: self.direction,
            decision: self.decision,
            path: self.path.clone(),
            chains: self.chains.clone(),
            states: self.states.clone(),
        }
        .joined(bytes, intent)
    }
}
#[derive(Clone, Debug)]
pub(crate) struct Descriptor {
    pub header: Header,
    pub body: Body,
    pub digest: [u8; 32],
}
fn payload(bytes: &[u8], cap: usize) -> Result<Cursor<'_>> {
    require((40..=cap).contains(&bytes.len()))?;
    let (body, checksum) = bytes.split_at(bytes.len() - 32);
    require(checksum == <[u8; 32]>::from(Sha256::digest(body)))?;
    require(
        u32::from_le_bytes(
            bytes
                .get(4..8)
                .ok_or(Error::Invalid)?
                .try_into()
                .map_err(|_| Error::Invalid)?,
        ) as usize
            == bytes.len(),
    )?;
    Ok(Cursor { bytes: body, at: 0 })
}
fn directory(c: &mut Cursor<'_>) -> Result<Object> {
    let value = c.directory()?;
    require(value.generation != [0; 32])?;
    Ok(value)
}
fn leaf(c: &mut Cursor<'_>) -> Result<Option<Leaf>> {
    match c.u8()? {
        0 => Ok(None),
        1 => {
            let object = directory(c)?;
            let stamp = c.stamp()?;
            require(stamp.mode & 0o170_000 == 0o040_000 && stamp.links > 0)?;
            Ok(Some(Leaf { object, stamp }))
        }
        _ => Err(Error::Invalid),
    }
}
fn header(c: &mut Cursor<'_>, mode: ParseMode) -> Result<Header> {
    let value = Header {
        connection: c.array()?,
        operation: c.u64()?,
        attempt: c.u64()?,
        nonce: c.array()?,
        epoch: c.array()?,
        session: c.array()?,
        admission: c.u64()?,
        baseline: if c.boolean()? {
            Some(c.u64()?)
        } else {
            c.zero(8)?;
            None
        },
        progress: c.u64()?,
        fence: c.u64()?,
    };
    require(
        value.operation > 0
            && (1..=8).contains(&value.attempt)
            && value.nonce != [0; 16]
            && value.epoch != [0; 16]
            && value.session != [0; 16]
            && value.admission > 0
            && value.progress >= value.admission
            && valid_baseline(value.baseline, value.admission - 1, mode),
    )?;
    Ok(value)
}
pub(crate) fn decode(bytes: &[u8]) -> Result<Descriptor> {
    decode_with_mode(bytes, ParseMode::Native)
}
fn decode_with_mode(bytes: &[u8], mode: ParseMode) -> Result<Descriptor> {
    let mut c = payload(bytes, super::MAX_BYTES)?;
    let magic = c.array::<4>()?;
    c.u32()?;
    let header = header(&mut c, mode)?;
    let body = match &magic {
        b"QCI1" => {
            let endpoints = [c.array()?, c.array()?];
            let direction = c.side()?;
            let decision = c.u8()?;
            require(decision == direction + 1)?;
            let namespace = directory(&mut c)?;
            let admission = c.array()?;
            require(admission != [0; 32])?;
            c.zero(34)?; // Chain authorization is unavailable in this slice.
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
            let states = [leaf(&mut c)?, leaf(&mut c)?];
            require(
                states
                    .get(usize::from(direction))
                    .ok_or(Error::Invalid)?
                    .is_some()
                    && states
                        .get(1 - usize::from(direction))
                        .ok_or(Error::Invalid)?
                        .is_none(),
            )?;
            Body::Intent(Intent {
                endpoints,
                direction,
                decision,
                namespace,
                admission,
                path,
                chains,
                states,
            })
        }
        b"QCS1" => {
            require(bytes.len() == 375)?;
            let stage = Stage {
                intent: c.array()?,
                created: directory(&mut c)?,
                source: directory(&mut c)?,
                target_parent: directory(&mut c)?,
            };
            require(c.u8()? == 1 && c.array::<32>()? == PROFILE && c.u8()? == 3)?;
            Body::Stage(stage)
        }
        b"QCO1" => {
            require(bytes.len() == 333)?;
            let intent = c.array()?;
            let stage = c.array()?;
            require(c.u8()? == 1)?;
            let source = directory(&mut c)?;
            require(c.u8()? == 1)?;
            let destination = directory(&mut c)?;
            require(c.u8()? == 3 && c.u8()? == 3)?;
            let sequence = c.u64()?;
            require(sequence > 0)?;
            Body::Outcome(Outcome {
                intent,
                stage,
                states: [source, destination],
                sequence,
            })
        }
        _ => return Err(Error::Invalid),
    };
    require(c.at == c.bytes.len())?;
    Ok(Descriptor {
        header,
        body,
        digest: bytes
            .get(bytes.len() - 32..)
            .ok_or(Error::Invalid)?
            .try_into()
            .map_err(|_| Error::Invalid)?,
    })
}
impl Descriptor {
    pub(crate) fn joined(&self, intent: &Self) -> Result<()> {
        let Body::Intent(before) = &intent.body else {
            return Err(Error::Invalid);
        };
        require(
            self.header.same_attempt(&intent.header)
                && self.header.epoch == intent.header.epoch
                && self.header.session == intent.header.session
                && self.header.progress == intent.header.progress
                && self.header.fence == intent.header.fence,
        )?;
        let Body::Stage(stage) = &self.body else {
            return Err(Error::Invalid);
        };
        let source = before
            .states
            .get(usize::from(before.direction))
            .ok_or(Error::Invalid)?
            .as_ref()
            .ok_or(Error::Invalid)?;
        require(
            stage.intent == intent.digest
                && stage.source == source.object
                && Some(&stage.target_parent)
                    == before
                        .chains
                        .get(1 - usize::from(before.direction))
                        .ok_or(Error::Invalid)?
                        .last(),
        )
    }
}

impl Descriptor {
    pub(crate) fn joined_outcome(&self, intent: &Self, stage: &Self) -> Result<()> {
        stage.joined(intent)?;
        let Body::Intent(before) = &intent.body else {
            return Err(Error::Invalid);
        };
        let Body::Stage(prepared) = &stage.body else {
            return Err(Error::Invalid);
        };
        let Body::Outcome(outcome) = &self.body else {
            return Err(Error::Invalid);
        };
        let side = usize::from(before.direction);
        require(
            self.header.same_attempt(&intent.header)
                && self.header.fence == intent.header.fence
                && self.header.progress >= intent.header.progress
                && outcome.intent == intent.digest
                && outcome.stage == stage.digest
                && outcome.states.get(side).ok_or(Error::Invalid)?
                    == &before
                        .states
                        .get(side)
                        .ok_or(Error::Invalid)?
                        .as_ref()
                        .ok_or(Error::Invalid)?
                        .object
                && outcome.states.get(1 - side).ok_or(Error::Invalid)? == &prepared.created,
        )
    }
}

fn finish(mut writer: super::Writer) -> Result<Vec<u8>> {
    let length = writer.0.len().checked_add(32).ok_or(Error::Invalid)?;
    require(length <= super::MAX_BYTES)?;
    writer
        .0
        .get_mut(4..8)
        .ok_or(Error::Invalid)?
        .copy_from_slice(
            &u32::try_from(length)
                .map_err(|_| Error::Invalid)?
                .to_le_bytes(),
        );
    writer.put(<[u8; 32]>::from(Sha256::digest(&writer.0)));
    Ok(writer.0)
}
fn write_path_states(
    writer: &mut super::Writer,
    path: &RelativePath,
    chains: &[Vec<Object>; 2],
    states: &[Option<Leaf>; 2],
) -> Result<()> {
    writer.path(path)?;
    for chain in chains {
        require(chain.len() == path.components().len() && chain.len() <= 64)?;
        writer.u8(u8::try_from(chain.len()).map_err(|_| Error::Invalid)?);
        for object in chain {
            writer.object(object);
        }
    }
    for state in states {
        writer.u8(u8::from(state.is_some()));
        if let Some(state) = state {
            writer.object(&state.object);
            writer.stamp(&state.stamp);
        }
    }
    Ok(())
}
impl Admission {
    pub(crate) fn encode(&self) -> Result<Vec<u8>> {
        let mut writer = super::Writer(Vec::with_capacity(super::MAX_BYTES));
        writer.put(b"QNA2");
        writer.u32(0);
        writer.put(self.connection);
        writer.u64(self.operation);
        writer.u64(self.revision);
        writer.u64(self.fence);
        writer.u8(1);
        writer.u64(self.baseline);
        for endpoint in self.endpoints {
            writer.put(endpoint);
        }
        writer.u8(self.direction);
        writer.u8(self.decision);
        write_path_states(&mut writer, &self.path, &self.chains, &self.states)?;
        let bytes = finish(writer)?;
        Self::decode(&bytes)?;
        Ok(bytes)
    }
}
pub(crate) fn encode(header: &Header, body: &Body) -> Result<Vec<u8>> {
    let mut writer = super::Writer(Vec::with_capacity(super::MAX_BYTES));
    writer.put(match body {
        Body::Intent(_) => b"QCI1",
        Body::Stage(_) => b"QCS1",
        Body::Outcome(_) => b"QCO1",
    });
    writer.u32(0);
    writer.put(header.connection);
    writer.u64(header.operation);
    writer.u64(header.attempt);
    writer.put(header.nonce);
    writer.put(header.epoch);
    writer.put(header.session);
    writer.u64(header.admission);
    writer.u8(u8::from(header.baseline.is_some()));
    writer.u64(header.baseline.unwrap_or(0));
    writer.u64(header.progress);
    writer.u64(header.fence);
    match body {
        Body::Intent(value) => {
            for endpoint in value.endpoints {
                writer.put(endpoint);
            }
            writer.u8(value.direction);
            writer.u8(value.decision);
            writer.object(&value.namespace);
            writer.put(value.admission);
            writer.put([0; 34]);
            write_path_states(&mut writer, &value.path, &value.chains, &value.states)?;
        }
        Body::Stage(value) => {
            writer.put(value.intent);
            writer.object(&value.created);
            writer.object(&value.source);
            writer.object(&value.target_parent);
            writer.u8(1);
            writer.put(PROFILE);
            writer.u8(3);
        }
        Body::Outcome(value) => {
            writer.put(value.intent);
            writer.put(value.stage);
            for object in &value.states {
                writer.u8(1);
                writer.object(object);
            }
            writer.put([3, 3]);
            writer.u64(value.sequence);
        }
    }
    let bytes = finish(writer)?;
    decode(&bytes)?;
    Ok(bytes)
}

/// Structural historical syntax only; the store must validate first-anchor closure.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::connection) struct HistoricalAdmission {
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
    pub states: [Option<Leaf>; 2],
}
#[derive(Clone, Copy)]
enum ParseMode {
    Native,
    Historical,
}
fn valid_baseline(baseline: Option<u64>, ceiling: u64, mode: ParseMode) -> bool {
    match baseline {
        Some(value) => value > 0 && value <= ceiling,
        None => matches!(mode, ParseMode::Historical),
    }
}
impl HistoricalAdmission {
    pub(in crate::connection) fn decode(bytes: &[u8]) -> Result<Self> {
        Self::parse(bytes, ParseMode::Historical)
    }
    fn parse(bytes: &[u8], mode: ParseMode) -> Result<Self> {
        let mut c = payload(bytes, super::MAX_BYTES)?;
        require(c.array::<4>()? == *b"QNA2")?;
        c.u32()?;
        let connection = c.array()?;
        let operation = c.u64()?;
        let revision = c.u64()?;
        let fence = c.u64()?;
        let baseline = if c.boolean()? {
            Some(c.u64()?)
        } else {
            c.zero(8)?;
            None
        };
        require(operation > 0 && revision > 0 && valid_baseline(baseline, revision, mode))?;
        let endpoints = [c.array()?, c.array()?];
        let direction = c.side()?;
        let decision = c.u8()?;
        require(decision == direction + 1)?;
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
        let states = [leaf(&mut c)?, leaf(&mut c)?];
        require(
            states
                .get(usize::from(direction))
                .ok_or(Error::Invalid)?
                .is_some()
                && states
                    .get(1 - usize::from(direction))
                    .ok_or(Error::Invalid)?
                    .is_none()
                && c.at == c.bytes.len(),
        )?;
        Ok(Self {
            connection,
            operation,
            revision,
            fence,
            baseline,
            endpoints,
            direction,
            decision,
            path,
            chains,
            states,
        })
    }
    pub(in crate::connection) fn joined(&self, bytes: &[u8], intent: &Descriptor) -> Result<()> {
        let Body::Intent(body) = &intent.body else {
            return Err(Error::Invalid);
        };
        require(
            self.connection == intent.header.connection
                && self.operation == intent.header.operation
                && self.revision.checked_add(1) == Some(intent.header.admission)
                && self.fence == intent.header.fence
                && self.baseline == intent.header.baseline
                && self.endpoints == body.endpoints
                && self.direction == body.direction
                && self.decision == body.decision
                && self.path == body.path
                && self.chains == body.chains
                && self.states == body.states
                && body.admission == <[u8; 32]>::from(Sha256::digest(bytes)),
        )
    }
}
/// Parses inert syntax; acceptance requires the store's complete retained joins.
pub(in crate::connection) fn decode_history(bytes: &[u8]) -> Result<Descriptor> {
    decode_with_mode(bytes, ParseMode::Historical)
}
