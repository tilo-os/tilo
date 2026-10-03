use super::super::internal_download::SelectedManifest;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    sync::atomic::{AtomicBool, Ordering},
};

pub(in crate::setup) const INTERVAL: u64 = 64 * 1024 * 1024;
pub(in crate::setup) const MAX_RECORD: usize = 4096;
pub(in crate::setup) const MAX_SCAN: usize = 16;
/// Providers must refuse after this many exclusive-create collisions, retaining
/// unknown entries. Journal and payload parents must be distinct pinned parents.
pub(in crate::setup) const MAX_COLLISIONS: usize = 16;
/// 2^32-1 transitions exceed the 16,384 intervals needed for a 1-TiB component,
/// including three attempts. Arbitrarily repeated pauses eventually refuse.
const MAX_GENERATION: u64 = u32::MAX as u64;
/// Every working leaf a provider mints is this prefix and 32 lowercase hex
/// digits.
const WORKING_LEAF_PREFIX: &str = ".tilo-part-";
/// A working leaf's exact length, which a checkpoint record stores in full.
const WORKING_LEAF_LEN: usize = WORKING_LEAF_PREFIX.len() + 32;
const RECORD_LEN: usize = layout::CHECKSUM + 32;

/// Where each field of a checkpoint record starts, in the order `encode`
/// writes them. Tests corrupt records field by field through these.
pub(in crate::setup) mod layout {
    pub(in crate::setup) const KIND: usize = 8;
    pub(in crate::setup) const BINDING: usize = KIND + 1;
    pub(in crate::setup) const PARENT: usize = BINDING + 32;
    pub(in crate::setup) const FILE: usize = PARENT + 32;
    pub(in crate::setup) const LEAF: usize = FILE + 32;
    pub(in crate::setup) const GENERATION: usize = LEAF + super::WORKING_LEAF_LEN;
    pub(in crate::setup) const EPOCH: usize = GENERATION + 8;
    pub(in crate::setup) const LENGTH: usize = EPOCH + 8;
    pub(in crate::setup) const FULL_SIZE: usize = LENGTH + 8;
    pub(in crate::setup) const DIGEST: usize = FULL_SIZE + 8;
    pub(in crate::setup) const CHECKSUM: usize = DIGEST + 32;
}

/// The working leaf named by the 128 bits of `name`.
pub(in crate::setup) fn working_leaf(name: u128) -> String {
    format!("{WORKING_LEAF_PREFIX}{name:032x}")
}

pub(in crate::setup) fn validate_leaf(
    selected: &SelectedManifest,
    leaf: &str,
) -> Result<(), String> {
    if leaf.len() != WORKING_LEAF_LEN
        || !leaf.strip_prefix(WORKING_LEAF_PREFIX).is_some_and(|name| {
            name.bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        })
        || (0..2).any(|i| {
            selected
                .bundle()
                .component(i)
                .is_some_and(|c| c.filename == leaf)
        })
    {
        return Err("Unsafe or final-component-colliding working leaf".into());
    }
    Ok(())
}

/// Native providers mint these only after validating their locked, private,
/// pinned namespace and held ordinary file. Decoded record fields never mint it.
/// IDs are canonical provider-specific digests of the full native identities;
/// providers also retain and compare the original native identities themselves.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(in crate::setup) struct Ownership {
    pub parent: [u8; 32],
    pub file: [u8; 32],
    pub leaf: String,
}
/// Only Session can issue this borrowed receipt after successful preparation.
/// It is neither constructible nor recoverable from serialized record hints.
pub(in crate::setup) struct PreparationComplete<'a> {
    record: &'a Record,
}
impl PreparationComplete<'_> {
    pub(super) fn authorizes(
        &self,
        selected: &SelectedManifest,
        index: usize,
        ownership: &Ownership,
    ) -> Result<bool, String> {
        Ok(
            binding(selected, index)? == (self.record.binding, self.record.full_size)
                && ownership == &self.record.ownership,
        )
    }
}
/// Native implementations remain private. All methods are synchronous.
/// `scan` must bound each entry BEFORE allocation/read to MAX_RECORD and stop on
/// visitor error, including unknown entries. Never remove undecodable entries.
/// `publish` must exclusively create/write/flush/publish one bounded record and
/// sync its namespace according to the native contract before returning Ok.
/// It retains `keep` (latest+previous, or reset fence+successor), and may remove
/// only previously validated owned journal entries. Error may mean publication
/// happened: caller is poisoned until independently recovered. A third bounded
/// in-progress entry can survive failure. Never silently repair unknown residue.
/// `retain` may retire only identities positively validated by this scan, and
/// syncs the changed journal namespace before returning. Creation/verification
/// must establish the payload name's parent durability before any publication.
/// Journal names live in a separate private parent from payload/final names.
pub(in crate::setup) trait Provider {
    /// State-only and infallible: called before attempts and during failed Drop.
    fn revoke_preparation(&mut self) {}
    fn prepared(&mut self, _proof: PreparationComplete<'_>) -> Result<(), String> {
        Ok(())
    }

    fn ownership(&self) -> Ownership;
    fn verify(&mut self, file: &File, ownership: &Ownership) -> Result<(), String>;
    fn scan(&mut self, visit: &mut dyn FnMut(&[u8]) -> Result<(), String>) -> Result<(), String>;
    fn retain(&mut self, keep: &[u64]) -> Result<(), String>;
    fn publish(&mut self, generation: u64, bytes: &[u8], keep: &[u64]) -> Result<(), String>;
}
/// Opt-in authority from a fully validated exclusive creation in this process.
/// Implementations consume private freshness BEFORE checking the requested
/// selection. Reopening, decoded records and caller assertions cannot mint it.
pub(in crate::setup) trait FreshProvider: Provider {
    fn take_fresh(
        &mut self,
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<Ownership, String>;
    /// Infallible, state-only, irreversible for this provider. Never clear an
    /// existing uncertainty poison, and never perform filesystem I/O here.
    fn initialization_failed(&mut self);
}

/// Settle an admitted fresh creation before cancellation can abandon its owner.
/// No operation ID, cancellation reset, payload read, or preparation receipt.
/// This is bounded metadata work, not a bounded-latency fsync guarantee.
pub(in crate::setup) fn initialize_fresh<P: FreshProvider>(
    selected: &SelectedManifest,
    index: usize,
    provider: &mut P,
    file: &mut File,
) -> Result<(), String> {
    provider.revoke_preparation();
    let result = (|| {
        let ownership = provider.take_fresh(selected, index)?;
        if ownership != provider.ownership() {
            return Err("Fresh ownership mismatch".into());
        }
        let mut record = Record::initial(selected, index, ownership)?;
        provider.verify(file, &record.ownership)?;
        provider.scan(&mut |_| Err("Fresh ownership has journal history".into()))?;
        let metadata = file.metadata().map_err(io)?;
        if !metadata.is_file() || metadata.len() != 0 {
            return Err("Fresh ownership requires an empty ordinary file".into());
        }
        file.flush().map_err(io)?;
        #[cfg(test)]
        super::tests::fault("initialize-flush")?;
        file.sync_all().map_err(io)?;
        provider.verify(file, &record.ownership)?;
        record.generation = 1;
        provider.publish(1, &record.encode(), &[1])
    })();
    if result.is_err() {
        provider.initialization_failed();
    }
    // The internal empty scan/publication grants no retirement or final rights.
    provider.revoke_preparation();
    result
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::setup) enum Kind {
    Prefix = 1,
    Reset = 2,
    Final = 3,
}
#[derive(Clone, Debug)]
// The checksum detects local record damage; only SelectedManifest authenticates
// release bytes. Decoding never supplies file or namespace capabilities.
pub(super) struct Record {
    binding: [u8; 32],
    ownership: Ownership,
    generation: u64,
    epoch: u64,
    length: u64,
    full_size: u64,
    digest: [u8; 32],
    kind: Kind,
}
fn binding(selected: &SelectedManifest, index: usize) -> Result<([u8; 32], u64), String> {
    let c = selected
        .bundle()
        .component(index)
        .ok_or("Invalid component index")?;
    let mut h = Sha256::new();
    // All variable fields are length-prefixed; metadata comes from sealed selection.
    for field in [
        selected.manifest_sha256().as_bytes(),
        c.role.as_bytes(),
        c.filename.as_bytes(),
        c.sha256.as_bytes(),
    ] {
        h.update((field.len() as u64).to_le_bytes());
        h.update(field);
    }
    h.update((index as u64).to_le_bytes());
    h.update(c.byte_size.to_le_bytes());
    Ok((h.finalize().into(), c.byte_size))
}
/// Selection-checked metadata only; never an acquired namespace capability.
pub(super) struct UntrustedRecordHint<'a> {
    pub(super) leaf: &'a str,
    pub(super) parent_digest: &'a [u8; 32],
    pub(super) file_digest: &'a [u8; 32],
    pub(super) generation: u64,
}
impl Record {
    fn initial(
        selected: &SelectedManifest,
        index: usize,
        ownership: Ownership,
    ) -> Result<Self, String> {
        validate_leaf(selected, &ownership.leaf)?;
        let (binding, full_size) = binding(selected, index)?;
        Ok(Self {
            binding,
            ownership,
            generation: 0,
            epoch: 0,
            length: 0,
            full_size,
            digest: empty_digest(),
            kind: Kind::Prefix,
        })
    }
    pub(super) fn hint_for(
        &self,
        selected: &SelectedManifest,
        index: usize,
    ) -> Result<UntrustedRecordHint<'_>, String> {
        if binding(selected, index)? != (self.binding, self.full_size) {
            return Err("Checkpoint hint selection mismatch".into());
        }
        validate_leaf(selected, &self.ownership.leaf)?;
        Ok(UntrustedRecordHint {
            leaf: &self.ownership.leaf,
            parent_digest: &self.ownership.parent,
            file_digest: &self.ownership.file,
            generation: self.generation,
        })
    }

    fn encode(&self) -> Vec<u8> {
        let mut b = Vec::with_capacity(300);
        b.extend_from_slice(b"TILOCP01");
        b.push(self.kind as u8);
        b.extend_from_slice(&self.binding);
        b.extend_from_slice(&self.ownership.parent);
        b.extend_from_slice(&self.ownership.file);
        b.extend_from_slice(self.ownership.leaf.as_bytes());
        for v in [self.generation, self.epoch, self.length, self.full_size] {
            b.extend_from_slice(&v.to_le_bytes());
        }
        b.extend_from_slice(&self.digest);
        let checksum = Sha256::digest(&b);
        b.extend_from_slice(&checksum);
        b
    }
    pub(super) fn decode(b: &[u8]) -> Result<Self, String> {
        // Exact fixed layout rejects trailing, missing, unknown and noncanonical forms.
        if b.len() != RECORD_LEN
            || b.len() > MAX_RECORD
            || &b[..8] != b"TILOCP01"
            || Sha256::digest(&b[..b.len() - 32])[..] != b[b.len() - 32..]
        {
            return Err("Invalid bounded checkpoint record".into());
        }
        let kind = match b[8] {
            1 => Kind::Prefix,
            2 => Kind::Reset,
            3 => Kind::Final,
            _ => return Err("Unknown checkpoint kind".into()),
        };
        let mut offset = 9;
        fn take<const N: usize>(b: &[u8], offset: &mut usize) -> [u8; N] {
            let v = b[*offset..*offset + N].try_into().unwrap();
            *offset += N;
            v
        }
        let binding = take(b, &mut offset);
        let parent = take(b, &mut offset);
        let file = take(b, &mut offset);
        let leaf = String::from_utf8(take::<WORKING_LEAF_LEN>(b, &mut offset).to_vec())
            .map_err(|_| "Invalid leaf encoding")?;
        let generation = u64::from_le_bytes(take(b, &mut offset));
        let epoch = u64::from_le_bytes(take(b, &mut offset));
        let length = u64::from_le_bytes(take(b, &mut offset));
        let full_size = u64::from_le_bytes(take(b, &mut offset));
        let digest = take(b, &mut offset);
        if generation == 0
            || generation > MAX_GENERATION
            || epoch >= generation
            || (kind != Kind::Reset
                && epoch > 0
                && epoch
                    .checked_add(2)
                    .is_none_or(|minimum| generation < minimum))
            || (generation == 1 && (kind != Kind::Prefix || length != 0))
            || length > full_size
            || full_size == 0
            || full_size > 1 << 40
            || (kind == Kind::Prefix
                && (length == full_size || (length == 0 && (generation != 1 || epoch != 0))))
            || (kind == Kind::Final && length != full_size)
            || (kind == Kind::Reset && (epoch == 0 || length != 0 || digest != empty_digest()))
        {
            return Err("Impossible checkpoint fields".into());
        }
        Ok(Self {
            binding,
            ownership: Ownership { parent, file, leaf },
            generation,
            epoch,
            length,
            full_size,
            digest,
            kind,
        })
    }
}
fn empty_digest() -> [u8; 32] {
    Sha256::digest([]).into()
}
fn stopped(cancel: &AtomicBool) -> Result<(), String> {
    if cancel.load(Ordering::Acquire) {
        Err("Internal transfer cancelled".into())
    } else {
        Ok(())
    }
}
fn io(e: std::io::Error) -> String {
    format!("Checkpoint file error: {e}")
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::setup) struct BoundaryId {
    pub operation: u64,
    pub component: usize,
    pub attempt: u8,
    pub epoch: u64,
    pub generation: u64,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::setup) struct Ack {
    pub id: BoundaryId,
}
#[derive(Clone, Debug)]
pub(in crate::setup) struct Candidate {
    pub length: u64,
    pub digest: [u8; 32],
    pub kind: Kind,
}
pub(in crate::setup) trait Observer {
    fn begin_attempt(&mut self, id: BoundaryId, file: &File) -> Result<(), String>;
    fn boundary(
        &mut self,
        id: BoundaryId,
        file: &mut File,
        candidate: Candidate,
    ) -> Result<Ack, String>;
}
/// One operation only; publication errors poison it even if acknowledgement was
/// lost. A new authenticated operation must scan surviving records independently.
pub(in crate::setup) struct Session<P: Provider> {
    provider: P,
    base: Record,
    operation: u64,
    index: usize,
    attempt: Option<u8>,
    poisoned: bool,
    recovered: bool,
}
impl<P: Provider> Session<P> {
    pub fn new(
        selected: &SelectedManifest,
        index: usize,
        operation: u64,
        mut provider: P,
    ) -> Result<Self, String> {
        provider.revoke_preparation();
        if operation == 0 {
            return Err("Zero operation ID".into());
        }
        let ownership = provider.ownership();
        let base = Record::initial(selected, index, ownership)?;
        Ok(Self {
            provider,
            base,
            operation,
            index,
            attempt: None,
            poisoned: false,
            recovered: false,
        })
    }
    pub fn prepare<'a>(
        &'a mut self,
        selected: &'a SelectedManifest,
        file: &'a mut File,
        cancel: &AtomicBool,
    ) -> Result<PreparedPrefix<'a>, String> {
        self.provider.revoke_preparation();
        stopped(cancel)?;
        if self.recovered || self.poisoned || binding(selected, self.index)?.0 != self.base.binding
        {
            return Err("Session already used or selection mismatch".into());
        }
        self.provider.verify(file, &self.base.ownership)?;
        let mut records = Vec::new();
        let mut count = 0;
        self.provider.scan(&mut |bytes| {
            count += 1;
            if count > MAX_SCAN {
                return Err("Checkpoint scan limit exceeded".into());
            }
            let record = Record::decode(bytes)?;
            if record.binding != self.base.binding
                || record.full_size != self.base.full_size
                || record.ownership != self.base.ownership
            {
                return Err("Unknown checkpoint selection or ownership".into());
            }
            records.push(record);
            Ok(())
        })?;
        if records.len() > 3 {
            return Err("Too many checkpoint survivors".into());
        }
        records.sort_by_key(|r| r.generation);
        for pair in records.windows(2) {
            let (a, b) = (&pair[0], &pair[1]);
            if a.kind == Kind::Final
                || b.generation != a.generation + 1
                || (b.kind == Kind::Reset && b.epoch != a.epoch + 1)
                || (b.kind != Kind::Reset && (b.epoch != a.epoch || b.length <= a.length))
            {
                return Err("Conflicting checkpoint history".into());
            }
        }
        let retained: Vec<_> = records.iter().rev().take(2).map(|r| r.generation).collect();
        if let Some(record) = records.pop() {
            self.base = record;
        }
        let metadata = file.metadata().map_err(io)?;
        if !metadata.is_file()
            || metadata.len() > self.base.full_size
            || metadata.len() < self.base.length
        {
            return Err("Checkpoint held file length mismatch".into());
        }
        if self.base.generation == 0 {
            if metadata.len() != 0 {
                return Err("Unrecorded payload is unknown residue".into());
            }
            file.flush().map_err(io)?;
            stopped(cancel)?;
            file.sync_all().map_err(io)?;
            stopped(cancel)?;
            self.publish(
                file,
                Candidate {
                    length: 0,
                    digest: empty_digest(),
                    kind: Kind::Prefix,
                },
            )?;
        }
        let history: Vec<_> = records
            .iter()
            .filter(|r| r.epoch == self.base.epoch)
            .map(|r| (r.length, r.digest))
            .collect();
        let hashes = Hashes::seed(
            file,
            self.base.length,
            self.base.digest,
            &history,
            cancel,
            "restart",
        )?;
        stopped(cancel)?;
        self.provider.verify(file, &self.base.ownership)?;
        // Normalize a surviving uncertain successor only after bytes and the
        // entire bounded history validate. Never prune an unknown entry.
        if count > 2 {
            self.poisoned = true;
            self.provider.retain(&retained)?;
            self.poisoned = false;
        }
        // An ordinary record permits trimming only its uncommitted tail. A reset
        // record durably revokes all old prefix authority, before or after truncation.
        if file.metadata().map_err(io)?.len() != self.base.length {
            file.set_len(self.base.length).map_err(io)?;
            file.sync_all().map_err(io)?;
        }
        file.seek(SeekFrom::Start(self.base.length)).map_err(io)?;
        stopped(cancel)?;
        // This receipt is the final fallible step, after all history/prefix,
        // normalization, trimming, cursor and cancellation work. Native scanning
        // alone never establishes successful preparation.
        self.poisoned = true;
        if let Err(error) = self
            .provider
            .prepared(PreparationComplete { record: &self.base })
        {
            self.provider.revoke_preparation();
            return Err(error);
        }
        self.poisoned = false;
        self.recovered = true;
        let id = self.id(0);
        let committed = self.base.length;
        let final_candidate = self.base.kind == Kind::Final;
        Ok(PreparedPrefix {
            selected,
            index: self.index,
            file,
            observer: self,
            id,
            hashes,
            committed,
            final_candidate,
        })
    }
    fn id(&self, attempt: u8) -> BoundaryId {
        BoundaryId {
            operation: self.operation,
            component: self.index,
            attempt,
            epoch: self.base.epoch,
            generation: self.base.generation,
        }
    }
    fn publish(&mut self, file: &mut File, candidate: Candidate) -> Result<(), String> {
        self.provider.verify(file, &self.base.ownership)?;
        let generation = self
            .base
            .generation
            .checked_add(1)
            .filter(|g| *g <= MAX_GENERATION)
            .ok_or("Checkpoint generation exhausted")?;
        let epoch = if candidate.kind == Kind::Reset {
            self.base
                .epoch
                .checked_add(1)
                .ok_or("Checkpoint epoch exhausted")?
        } else {
            self.base.epoch
        };
        let next = Record {
            generation,
            epoch,
            length: candidate.length,
            digest: candidate.digest,
            kind: candidate.kind,
            ..self.base.clone()
        };
        // Keep the previous record until publication commits, even at reset.
        let keep = if self.base.generation == 0 {
            vec![generation]
        } else {
            vec![self.base.generation, generation]
        };
        self.poisoned = true;
        self.provider.publish(generation, &next.encode(), &keep)?;
        self.base = next;
        self.poisoned = false;
        Ok(())
    }
}
impl<P: Provider> Drop for Session<P> {
    fn drop(&mut self) {
        if !self.recovered {
            // Infallible state-only revocation; no cleanup or filesystem I/O.
            self.provider.revoke_preparation();
        }
    }
}
impl<P: Provider> Observer for Session<P> {
    fn begin_attempt(&mut self, id: BoundaryId, file: &File) -> Result<(), String> {
        if self.poisoned
            || !self.recovered
            || id != self.id(id.attempt)
            || id.attempt >= 3
            || self.attempt.is_some_and(|a| id.attempt != a + 1)
            || (self.attempt.is_none() && id.attempt != 0)
        {
            return Err("Stale checkpoint attempt".into());
        }
        self.provider.verify(file, &self.base.ownership)?;
        self.attempt = Some(id.attempt);
        Ok(())
    }
    fn boundary(&mut self, id: BoundaryId, file: &mut File, c: Candidate) -> Result<Ack, String> {
        if self.poisoned
            || !self.recovered
            || self.attempt != Some(id.attempt)
            || id != self.id(id.attempt)
            || c.length > self.base.full_size
            || (c.kind == Kind::Prefix
                && (c.length <= self.base.length || c.length == self.base.full_size))
            || (c.kind == Kind::Final && c.length != self.base.full_size)
            || (c.kind == Kind::Reset && (c.length != 0 || c.digest != empty_digest()))
            || self.base.kind == Kind::Final
        {
            return Err("Stale or impossible checkpoint boundary".into());
        }
        if c.kind != Kind::Reset && file.metadata().map_err(io)?.len() != c.length {
            return Err("Checkpoint candidate length mismatch".into());
        }
        self.publish(file, c)?;
        Ok(Ack {
            id: self.id(id.attempt),
        })
    }
}

/// Two independent hashers, fed independently from bytes; no serialized SHA
/// state or streaming-to-readback clone is ever accepted.
pub(in crate::setup) struct Hashes {
    stream: Sha256,
    readback: Sha256,
    pub length: u64,
    watermark: u64,
}
impl Hashes {
    pub fn empty() -> Self {
        Self {
            stream: Sha256::new(),
            readback: Sha256::new(),
            length: 0,
            watermark: 0,
        }
    }
    fn seed(
        file: &mut File,
        length: u64,
        digest: [u8; 32],
        history: &[(u64, [u8; 32])],
        cancel: &AtomicBool,
        phase: &'static str,
    ) -> Result<Self, String> {
        let mut stream = Sha256::new();
        let mut readback = Sha256::new();
        let mut checkpoints = history.iter().peekable();
        let mut offset = 0;
        let mut valid = true;
        // Check each retained current-epoch digest at its byte boundary during
        // the same prefix pass. Older reset epochs intentionally have no authority.
        while checkpoints.peek().is_some_and(|p| p.0 == 0) {
            valid &= checkpoints.next().unwrap().1 == empty_digest();
        }
        read_range(file, 0, length, cancel, phase, |mut bytes| {
            while !bytes.is_empty() {
                // A boundary already behind the cursor can never match; read on
                // and let the final "no unconsumed checkpoint" check refuse it.
                let take = checkpoints.peek().map_or(bytes.len(), |p| {
                    p.0.checked_sub(offset)
                        .map_or(bytes.len(), |gap| gap.min(bytes.len() as u64) as usize)
                });
                stream.update(&bytes[..take]);
                readback.update(&bytes[..take]);
                offset += take as u64;
                bytes = &bytes[take..];
                while checkpoints.peek().is_some_and(|p| p.0 == offset) {
                    let expected = checkpoints.next().unwrap().1;
                    valid &= <[u8; 32]>::from(stream.clone().finalize()) == expected
                        && <[u8; 32]>::from(readback.clone().finalize()) == expected;
                }
            }
        })?;
        if !valid
            || checkpoints.next().is_some()
            || <[u8; 32]>::from(stream.clone().finalize()) != digest
            || <[u8; 32]>::from(readback.clone().finalize()) != digest
        {
            return Err("Checkpoint prefix digest mismatch".into());
        }
        Ok(Self {
            stream,
            readback,
            length,
            watermark: length,
        })
    }
    pub fn written(&mut self, bytes: &[u8]) {
        self.stream.update(bytes);
        self.length += bytes.len() as u64;
    }
    pub fn digest(&self) -> [u8; 32] {
        self.stream.clone().finalize().into()
    }
    pub fn retry(&mut self, file: &mut File, cancel: &AtomicBool) -> Result<(), String> {
        if file.metadata().map_err(io)?.len() != self.length {
            return Err("Retry prefix length changed".into());
        }
        *self = Self::seed(file, self.length, self.digest(), &[], cancel, "retry")?;
        Ok(())
    }
    pub fn pending(&self) -> u64 {
        self.length - self.watermark
    }
    pub fn verify_tail(&mut self, file: &mut File, cancel: &AtomicBool) -> Result<(), String> {
        stopped(cancel)?;
        file.flush().map_err(io)?;
        #[cfg(test)]
        super::tests::fault("data-flush")?;
        stopped(cancel)?;
        file.sync_all().map_err(io)?;
        #[cfg(test)]
        super::tests::fault("data-sync")?;
        stopped(cancel)?;
        if file.metadata().map_err(io)?.len() != self.length {
            return Err("Checkpoint file length changed".into());
        }
        let mut next = self.readback.clone();
        read_range(file, self.watermark, self.length, cancel, "periodic", |b| {
            next.update(b)
        })?;
        if <[u8; 32]>::from(next.clone().finalize()) != self.digest() {
            return Err("Checkpoint independent readback mismatch".into());
        }
        if file.metadata().map_err(io)?.len() != self.length {
            return Err("Checkpoint file length changed".into());
        }
        self.readback = next;
        self.watermark = self.length;
        Ok(())
    }
}
fn read_range(
    file: &mut File,
    start: u64,
    end: u64,
    cancel: &AtomicBool,
    phase: &'static str,
    mut consume: impl FnMut(&[u8]),
) -> Result<(), String> {
    let cursor = file.stream_position().map_err(io)?;
    let result = (|| {
        file.seek(SeekFrom::Start(start)).map_err(io)?;
        let mut pos = start;
        let mut buffer = [0; 64 * 1024];
        while pos < end {
            stopped(cancel)?;
            let requested = (end - pos).min(buffer.len() as u64) as usize;
            let n = file.read(&mut buffer[..requested]).map_err(io)?;
            #[cfg(test)]
            super::tests::read_event(phase, requested as u64, n as u64);
            #[cfg(not(test))]
            let _ = phase;
            if n == 0 {
                return Err("Checkpoint prefix truncated".into());
            }
            consume(&buffer[..n]);
            pos += n as u64;
        }
        stopped(cancel)
    })();
    let restore = file.seek(SeekFrom::Start(cursor)).map_err(io);
    result?;
    restore?;
    Ok(())
}
/// Holds the exclusive file reborrow from recovery through transport. Cannot be
/// reconstructed from a record, moved to another file or reused for an operation.
pub(in crate::setup) struct PreparedPrefix<'a> {
    pub(super) selected: &'a SelectedManifest,
    pub(super) index: usize,
    pub(super) file: &'a mut File,
    pub(super) observer: &'a mut dyn Observer,
    pub(super) id: BoundaryId,
    pub(super) hashes: Hashes,
    pub(super) committed: u64,
    pub(super) final_candidate: bool,
}

/// Transport may consume this state, but cannot construct a PreparedPrefix.
pub(in crate::setup) struct TransferParts<'a> {
    pub selected: &'a SelectedManifest,
    pub index: usize,
    pub file: &'a mut File,
    pub observer: &'a mut dyn Observer,
    pub id: BoundaryId,
    pub hashes: Hashes,
    pub committed: u64,
    pub final_candidate: bool,
}
impl<'a> PreparedPrefix<'a> {
    pub fn committed(&self) -> u64 {
        self.committed
    }
    pub fn into_parts(self) -> TransferParts<'a> {
        let Self {
            selected,
            index,
            file,
            observer,
            id,
            hashes,
            committed,
            final_candidate,
        } = self;
        TransferParts {
            selected,
            index,
            file,
            observer,
            id,
            hashes,
            committed,
            final_candidate,
        }
    }
}
