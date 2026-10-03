//! Borrowed bounded native sender payload. Independent of browser outbox JSON.

use super::observation::{self, Facts, Lifecycle};

pub const MAX_ROWS: usize = 256;
pub const MAX_BYTES: usize = 4 * 1024 * 1024;
pub(super) const MAX_OBSERVATION: usize = 256 * 1024;
const EMPTY: &[u8; 8] = b"NSO1\x01\x00\x00\x00";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutboxFailure {
    Invalid,
    Capacity,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct CallerView<'a> {
    pub root: [u8; 32],
    pub device: [u8; 32],
    pub app: &'a str,
    pub record: &'a str,
    pub agent: Option<&'a str>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TargetView<'a> {
    pub host: [u8; 16],
    pub peer: u64,
    pub app: &'a str,
    pub record: &'a str,
    pub root: [u8; 32],
    pub device: [u8; 32],
    pub binding: [u8; 32],
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RowView<'a> {
    pub invocation: [u8; 16],
    pub caller: CallerView<'a>,
    pub target: TargetView<'a>,
    pub revision: &'a str,
    pub resource: &'a str,
    pub action: &'a str,
    pub argument: Option<&'a str>,
    pub observation: Option<ObservationView<'a>>,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ObservationView<'a> {
    pub id: u64,
    pub uncertain: bool,
    pub cancellation_requested: bool,
    pub json: &'a str,
    pub(super) facts: Facts,
}
impl ObservationView<'_> {
    fn extra_bytes(&self) -> usize {
        14 + self.json.len()
    }
}

/// Inert validated view; the caller retains the original bytes.
pub struct NativeSenderOutboxV1<'a> {
    bytes: &'a [u8],
    rows: usize,
    capacity_bound: usize,
}

impl<'a> NativeSenderOutboxV1<'a> {
    pub const fn empty_bytes() -> &'static [u8] {
        EMPTY
    }

    pub fn decode(bytes: &'a [u8]) -> Result<Self, OutboxFailure> {
        if bytes.len() > MAX_BYTES {
            return Err(OutboxFailure::Capacity);
        }
        if bytes.len() < EMPTY.len() || bytes[..6] != EMPTY[..6] {
            return Err(OutboxFailure::Invalid);
        }
        let rows = usize::from(u16::from_le_bytes([bytes[6], bytes[7]]));
        if rows > MAX_ROWS {
            return Err(OutboxFailure::Capacity);
        }
        let mut cursor = Cursor(&bytes[EMPTY.len()..]);
        let mut previous = None;
        let mut capacity_bound = EMPTY.len();
        for _ in 0..rows {
            let before = cursor.0.len();
            let row = cursor.row()?;
            let base = before - cursor.0.len() - row.observation.map_or(0, |v| v.extra_bytes());
            capacity_bound = capacity_bound
                .checked_add(base + MAX_OBSERVATION + 14)
                .ok_or(OutboxFailure::Capacity)?;
            if previous.is_some_and(|id| id >= row.invocation) {
                return Err(OutboxFailure::Conflict);
            }
            previous = Some(row.invocation);
        }
        cursor.end()?;
        if capacity_bound > MAX_BYTES {
            return Err(OutboxFailure::Capacity);
        }
        Ok(Self {
            bytes,
            rows,
            capacity_bound,
        })
    }

    pub fn len(&self) -> usize {
        self.rows
    }

    pub fn is_empty(&self) -> bool {
        self.rows == 0
    }

    pub fn as_bytes(&self) -> &'a [u8] {
        self.bytes
    }

    pub(super) fn rows(&self) -> impl Iterator<Item = Result<RowView<'a>, OutboxFailure>> + '_ {
        let mut cursor = Cursor(&self.bytes[EMPTY.len()..]);
        (0..self.rows).map(move |_| cursor.row())
    }
    pub(crate) fn find(&self, invocation: &[u8; 16]) -> Result<Option<RowView<'a>>, OutboxFailure> {
        let mut cursor = Cursor(&self.bytes[EMPTY.len()..]);
        for _ in 0..self.rows {
            let row = cursor.row()?;
            if &row.invocation == invocation {
                return Ok(Some(row));
            }
            if &row.invocation > invocation {
                break;
            }
        }
        Ok(None)
    }
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, count: usize) -> Result<&'a [u8], OutboxFailure> {
        let (value, rest) = self
            .0
            .split_at_checked(count)
            .ok_or(OutboxFailure::Invalid)?;
        self.0 = rest;
        Ok(value)
    }
    fn number<const N: usize>(&mut self) -> Result<[u8; N], OutboxFailure> {
        self.take(N)?.try_into().map_err(|_| OutboxFailure::Invalid)
    }
    fn identity<const N: usize>(&mut self) -> Result<[u8; N], OutboxFailure> {
        let value = self.number()?;
        if value == [0; N] {
            return Err(OutboxFailure::Invalid);
        }
        Ok(value)
    }
    fn text(
        &mut self,
        length: usize,
        limit: usize,
        empty: bool,
        identifier: bool,
    ) -> Result<&'a str, OutboxFailure> {
        if length > limit {
            return Err(OutboxFailure::Capacity);
        }
        let text = std::str::from_utf8(self.take(length)?).map_err(|_| OutboxFailure::Invalid)?;
        if (!empty && text.is_empty())
            || text.contains('\0')
            || (identifier && text.bytes().any(|b| b.is_ascii_control()))
        {
            return Err(OutboxFailure::Invalid);
        }
        Ok(text)
    }
    fn short(&mut self, empty: bool) -> Result<&'a str, OutboxFailure> {
        let length = usize::from(u16::from_le_bytes(self.number()?));
        self.text(length, 512, empty, true)
    }
    fn end(&self) -> Result<(), OutboxFailure> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(OutboxFailure::Invalid)
        }
    }
    fn row(&mut self) -> Result<RowView<'a>, OutboxFailure> {
        let length = usize::try_from(u32::from_le_bytes(self.number()?))
            .map_err(|_| OutboxFailure::Capacity)?;
        if length > MAX_BYTES {
            return Err(OutboxFailure::Capacity);
        }
        let mut row = Cursor(self.take(length)?);
        let invocation = row.identity()?;
        let root = row.identity()?;
        let device = row.identity()?;
        let app = row.short(false)?;
        let record = row.short(true)?;
        let agent_length = u16::from_le_bytes(row.number()?);
        let agent = if agent_length == u16::MAX {
            None
        } else {
            Some(row.text(usize::from(agent_length), 512, false, true)?)
        };
        let caller = CallerView {
            root,
            device,
            app,
            record,
            agent,
        };
        let host = row.identity()?;
        let peer = u64::from_le_bytes(row.number()?);
        if peer == 0 || peer == u64::MAX {
            return Err(OutboxFailure::Invalid);
        }
        let target = TargetView {
            host,
            peer,
            app: row.short(false)?,
            record: row.short(true)?,
            root: row.identity()?,
            device: row.identity()?,
            binding: row.identity()?,
        };
        let revision_length = usize::from(u16::from_le_bytes(row.number()?));
        let revision = row.text(revision_length, 512, false, false)?;
        let resource = row.short(false)?;
        let action = row.short(false)?;
        let argument_length = u32::from_le_bytes(row.number()?);
        let argument = if argument_length == u32::MAX {
            None
        } else {
            Some(row.text(
                usize::try_from(argument_length).map_err(|_| OutboxFailure::Capacity)?,
                32 * 1024,
                true,
                false,
            )?)
        };
        let observation = match row.take(1)? {
            [0] => None,
            [1] => {
                let id = u64::from_le_bytes(row.number()?);
                if id == 0 {
                    return Err(OutboxFailure::Invalid);
                }
                let uncertain = match row.take(1)? {
                    [0] => false,
                    [1] => true,
                    _ => return Err(OutboxFailure::Invalid),
                };
                let cancellation_requested = match row.take(1)? {
                    [0] => false,
                    [1] => true,
                    _ => return Err(OutboxFailure::Invalid),
                };
                let length = usize::try_from(u32::from_le_bytes(row.number()?))
                    .map_err(|_| OutboxFailure::Capacity)?;
                let json = row.text(length, MAX_OBSERVATION, false, false)?;
                let facts =
                    observation::validate(json, &invocation, uncertain, cancellation_requested)?;
                Some(ObservationView {
                    id,
                    uncertain,
                    cancellation_requested,
                    json,
                    facts,
                })
            }
            _ => return Err(OutboxFailure::Invalid),
        };
        row.end()?;
        Ok(RowView {
            invocation,
            caller,
            target,
            revision,
            resource,
            action,
            argument,
            observation,
        })
    }
}

/// Immutable mutation bytes. Decoding/encoding is not a storage receipt.
pub(crate) struct EncodedOutbox {
    bytes: Vec<u8>,
}
impl EncodedOutbox {
    #[cfg(test)]
    pub(crate) fn bytes(&self) -> &[u8] {
        &self.bytes
    }
    pub(crate) fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }
}
fn checked_text(
    text: &str,
    limit: usize,
    empty: bool,
    identifier: bool,
) -> Result<usize, OutboxFailure> {
    if text.len() > limit {
        return Err(OutboxFailure::Capacity);
    }
    if (!empty && text.is_empty())
        || text.contains('\0')
        || (identifier && text.bytes().any(|b| b.is_ascii_control()))
    {
        return Err(OutboxFailure::Invalid);
    }
    Ok(text.len())
}
impl RowView<'_> {
    pub(super) fn encoded_len(&self) -> Result<usize, OutboxFailure> {
        if self.invocation == [0; 16]
            || self.caller.root == [0; 32]
            || self.caller.device == [0; 32]
            || self.target.host == [0; 16]
            || self.target.root == [0; 32]
            || self.target.device == [0; 32]
            || self.target.binding == [0; 32]
            || self.target.peer == 0
            || self.target.peer == u64::MAX
        {
            return Err(OutboxFailure::Invalid);
        }
        // Fixed length prefix, UUIDs, roots/devices/binding, peer, optional
        // agent/argument length fields and absent-observation discriminant.
        let mut length = 4 + 16 + 32 + 32 + 2 + 16 + 8 + 32 + 32 + 32 + 4 + 1;
        for (text, empty, identifier) in [
            (self.caller.app, false, true),
            (self.caller.record, true, true),
            (self.target.app, false, true),
            (self.target.record, true, true),
            (self.revision, false, false),
            (self.resource, false, true),
            (self.action, false, true),
        ] {
            length += 2 + checked_text(text, 512, empty, identifier)?;
        }
        if let Some(text) = self.caller.agent {
            length += checked_text(text, 512, false, true)?;
        }
        if let Some(text) = self.argument {
            length += checked_text(text, 32 * 1024, true, false)?;
        }
        if let Some(observation) = self.observation {
            length += observation.extra_bytes();
        }
        Ok(length)
    }
    fn encode(&self, output: &mut Vec<u8>, length: usize) {
        fn short(output: &mut Vec<u8>, text: &str) {
            output.extend_from_slice(&(text.len() as u16).to_le_bytes());
            output.extend_from_slice(text.as_bytes());
        }
        output.extend_from_slice(&((length - 4) as u32).to_le_bytes());
        output.extend_from_slice(&self.invocation);
        output.extend_from_slice(&self.caller.root);
        output.extend_from_slice(&self.caller.device);
        short(output, self.caller.app);
        short(output, self.caller.record);
        match self.caller.agent {
            Some(text) => short(output, text),
            None => output.extend_from_slice(&u16::MAX.to_le_bytes()),
        }
        output.extend_from_slice(&self.target.host);
        output.extend_from_slice(&self.target.peer.to_le_bytes());
        short(output, self.target.app);
        short(output, self.target.record);
        output.extend_from_slice(&self.target.root);
        output.extend_from_slice(&self.target.device);
        output.extend_from_slice(&self.target.binding);
        short(output, self.revision);
        short(output, self.resource);
        short(output, self.action);
        match self.argument {
            Some(text) => {
                output.extend_from_slice(&(text.len() as u32).to_le_bytes());
                output.extend_from_slice(text.as_bytes());
            }
            None => output.extend_from_slice(&u32::MAX.to_le_bytes()),
        }
        match self.observation {
            None => output.push(0),
            Some(observation) => {
                output.push(1);
                output.extend_from_slice(&observation.id.to_le_bytes());
                output.push(u8::from(observation.uncertain));
                output.push(u8::from(observation.cancellation_requested));
                output.extend_from_slice(&(observation.json.len() as u32).to_le_bytes());
                output.extend_from_slice(observation.json.as_bytes());
            }
        }
    }
}
impl NativeSenderOutboxV1<'_> {
    pub(crate) fn insertion_bytes(&self, row: &RowView<'_>) -> Result<usize, OutboxFailure> {
        if row.observation.is_some() {
            return Err(OutboxFailure::Invalid);
        }
        if self.rows == MAX_ROWS {
            return Err(OutboxFailure::Capacity);
        }
        if self.find(&row.invocation)?.is_some() {
            return Err(OutboxFailure::Conflict);
        }
        let length = self
            .bytes
            .len()
            .checked_add(row.encoded_len()?)
            .ok_or(OutboxFailure::Capacity)?;
        if self
            .capacity_bound
            .checked_add(row.encoded_len()? + MAX_OBSERVATION + 14)
            .ok_or(OutboxFailure::Capacity)?
            > MAX_BYTES
        {
            return Err(OutboxFailure::Capacity);
        }
        Ok(length)
    }
    pub(crate) fn insert(&self, row: &RowView<'_>) -> Result<EncodedOutbox, OutboxFailure> {
        let needed = self.insertion_bytes(row)?;
        // No strings or payloads are copied before the capacity checks above.
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(needed)
            .map_err(|_| OutboxFailure::Capacity)?;
        bytes.extend_from_slice(&EMPTY[..6]);
        bytes.extend_from_slice(&((self.rows + 1) as u16).to_le_bytes());
        let mut cursor = Cursor(&self.bytes[EMPTY.len()..]);
        let mut inserted = false;
        for _ in 0..self.rows {
            let start = cursor.0;
            let old = cursor.row()?;
            if !inserted && old.invocation > row.invocation {
                row.encode(&mut bytes, needed - self.bytes.len());
                inserted = true;
            }
            bytes.extend_from_slice(&start[..start.len() - cursor.0.len()]);
        }
        if !inserted {
            row.encode(&mut bytes, needed - self.bytes.len());
        }
        debug_assert_eq!(bytes.len(), needed);
        Ok(EncodedOutbox { bytes })
    }
}

impl NativeSenderOutboxV1<'_> {
    fn replacement<'b>(
        &'b self,
        id: &[u8; 16],
        response: &'b runtime_collab::PublishedInvocationResult,
    ) -> Result<RowView<'b>, OutboxFailure> {
        let mut row = self.find(id)?.ok_or(OutboxFailure::Invalid)?;
        if response.id == 0 {
            return Err(OutboxFailure::Invalid);
        }
        let facts = observation::validate(
            &response.update_json,
            id,
            response.uncertain,
            response.cancellation_requested,
        )?;
        let new = ObservationView {
            id: response.id,
            uncertain: response.uncertain,
            cancellation_requested: response.cancellation_requested,
            json: &response.update_json,
            facts,
        };
        if let Some(old) = row.observation {
            if old.id != new.id
                || (old.facts.consumable && old != new)
                || (old.cancellation_requested && !new.cancellation_requested)
                || matches!((old.facts.sequence,new.facts.sequence),(Some(a),Some(b)) if b<a)
                || (old.facts.sequence.is_some() && new.facts.lifecycle == Lifecycle::Accepted)
            {
                return Err(OutboxFailure::Conflict);
            }
        }
        row.observation = Some(new);
        Ok(row)
    }
    fn mutation_bytes(
        &self,
        id: &[u8; 16],
        replacement: Option<&RowView<'_>>,
    ) -> Result<usize, OutboxFailure> {
        let old = self.find(id)?.ok_or(OutboxFailure::Invalid)?;
        self.bytes
            .len()
            .checked_sub(old.encoded_len()?)
            .and_then(|length| {
                length.checked_add(replacement.map_or(Ok(0), |v| v.encoded_len()).ok()?)
            })
            .ok_or(OutboxFailure::Capacity)
    }
    #[cfg(test)]
    pub(crate) fn observation_bytes(
        &self,
        id: &[u8; 16],
        response: &runtime_collab::PublishedInvocationResult,
    ) -> Result<usize, OutboxFailure> {
        let replacement = self.replacement(id, response)?;
        self.mutation_bytes(id, Some(&replacement))
    }
    pub(crate) fn with_observation(
        &self,
        id: &[u8; 16],
        response: &runtime_collab::PublishedInvocationResult,
    ) -> Result<EncodedOutbox, OutboxFailure> {
        let replacement = self.replacement(id, response)?;
        self.mutate(id, Some(&replacement))
    }
    pub(crate) fn consumption_bytes(&self, id: &[u8; 16]) -> Result<usize, OutboxFailure> {
        let row = self.find(id)?.ok_or(OutboxFailure::Invalid)?;
        if !row.observation.is_some_and(|v| v.facts.consumable) {
            return Err(OutboxFailure::Invalid);
        }
        self.mutation_bytes(id, None)
    }
    pub(crate) fn consume(&self, id: &[u8; 16]) -> Result<EncodedOutbox, OutboxFailure> {
        self.consumption_bytes(id)?;
        self.mutate(id, None)
    }
    fn mutate(
        &self,
        id: &[u8; 16],
        replacement: Option<&RowView<'_>>,
    ) -> Result<EncodedOutbox, OutboxFailure> {
        let needed = self.mutation_bytes(id, replacement)?;
        if needed > MAX_BYTES {
            return Err(OutboxFailure::Capacity);
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(needed)
            .map_err(|_| OutboxFailure::Capacity)?;
        bytes.extend_from_slice(&EMPTY[..6]);
        bytes.extend_from_slice(
            &((self.rows - usize::from(replacement.is_none())) as u16).to_le_bytes(),
        );
        let mut cursor = Cursor(&self.bytes[EMPTY.len()..]);
        for _ in 0..self.rows {
            let start = cursor.0;
            let old = cursor.row()?;
            if &old.invocation == id {
                if let Some(row) = replacement {
                    row.encode(&mut bytes, row.encoded_len()?);
                }
            } else {
                bytes.extend_from_slice(&start[..start.len() - cursor.0.len()]);
            }
        }
        debug_assert_eq!(bytes.len(), needed);
        Ok(EncodedOutbox { bytes })
    }
}
