//! Only this owner can dispatch production native operations. Its FDs never leave the broker.
use super::{
    BTreeSet, Candidate, Digest, Duration, HeldClaim, Instant, Sha256, claim_candidates,
    credentials, digest, inventory, io, limits, non_ntfs_mounts, refusal, revalidate, selection,
};
use std::fmt::Write;
type Finished = (Vec<(String, String)>, Vec<Issue>);
use crate::{
    limits::{NativeOperation, ReadRequest},
    manifest::{self, Manifest},
};
use qos_ntfs_protocol::{Entry, Identity, Issue};
pub(super) struct ExclusiveReadSession {
    claims: Vec<HeldClaim>,
    selected: usize,
    inventory: Vec<Candidate>,
    mounts: BTreeSet<String>,
    credentials: (u32, u32),
    root: u64,
    pub(super) identity: Identity,
    native: crate::protocol::NativeIdentity,
    pub(super) nonce: String,
    pub(super) manifest: Manifest,
    original_facts: Vec<serde_json::Value>,
    generation: u64,
    deadline: Instant,
    observation: Option<Instant>,
    bytes: u64,
    logical: u64,
    requests: u32,
    invalid: bool,
    pub(super) cancellation_fd: Option<i32>,
    stream: crate::stream::Stream,
}
fn deadline(seconds: u64) -> io::Result<Instant> {
    Instant::now()
        .checked_add(Duration::from_secs(seconds))
        .ok_or_else(|| refusal("deadline overflow"))
}
pub(super) fn random() -> io::Result<String> {
    let mut bytes = [0u8; 32];
    if unsafe { libc::getrandom(bytes.as_mut_ptr().cast(), bytes.len(), 0) } != 32 {
        return Err(io::Error::last_os_error());
    }
    let mut nonce = String::new();
    for byte in bytes {
        write!(nonce, "{byte:02x}").map_err(io::Error::other)?;
    }
    Ok(nonce)
}
pub(super) fn serve(args: &[String]) -> io::Result<()> {
    let [verb, idflag, id, rootflag, root, uidflag, uid] = args else {
        return Err(refusal(
            "expected serve-read-selected --inventory-id ID --root-mft RECORD --client-uid UID",
        ));
    };
    if verb != "serve-read-selected" || uidflag != "--client-uid" {
        return Err(refusal("invalid selected read command"));
    }
    let selected_args = vec![
        "inspect-selected".into(),
        idflag.clone(),
        id.clone(),
        rootflag.clone(),
        root.clone(),
    ];
    let (_, root) = selection(&selected_args)?;
    let uid: u32 = uid.parse().map_err(io::Error::other)?;
    let session = ExclusiveReadSession::acquire(id, root)?;
    // Acquisition alarm is not a lease timeout. Every subsequent syscall has an operation deadline.
    unsafe {
        libc::alarm(0);
    }
    super::ipc::serve(session, uid)
}
impl ExclusiveReadSession {
    fn acquire(id: &str, root: u64) -> io::Result<Self> {
        let credentials = credentials()?;
        let (inventory, unresolved) = inventory()?;
        if !unresolved.is_empty() {
            return Err(refusal("unresolved inventory"));
        }
        let mut identities = BTreeSet::new();
        for c in &inventory {
            if !identities.insert((&c.hardware, c.start, c.sectors)) {
                return Err(refusal("ambiguous hardware partition identity"));
            }
        }
        let mounts = non_ntfs_mounts()?;
        let (claims, selected) = claim_candidates(&inventory, id)?;
        let claim = claims
            .get(selected)
            .ok_or_else(|| refusal("selected claim lost"))?;
        let end = deadline(20)?;
        let first = limits::run_operation(
            &claim.device,
            claim.observation.bytes,
            NativeOperation::Manifest { root },
            limits::READ_BYTES,
            Some(credentials),
            end,
        )?;
        let second = limits::run_operation(
            &claim.device,
            claim.observation.bytes,
            NativeOperation::Manifest { root },
            limits::READ_BYTES,
            Some(credentials),
            end,
        )?;
        if first.status != 0 || second.status != 0 || first.facts != second.facts {
            return Err(refusal("metadata admission disagreement"));
        }
        if !first.facts.iter().any(|f| {
            f.get("kind").and_then(serde_json::Value::as_str) == Some("volume")
                && f.get("sector_size").and_then(serde_json::Value::as_i64)
                    == Some(i64::from(claim.observation.sector_size))
        }) {
            return Err(refusal("sector geometry disagreement"));
        }
        let native = crate::protocol::identity(&first, root)?;
        let c = &claim.candidate;
        let volume = digest(&[
            b"persistent-volume-v1",
            c.hardware.as_bytes(),
            &c.start.to_le_bytes(),
            &c.sectors.to_le_bytes(),
            native.volume.as_bytes(),
        ]);
        let persistent_root = digest(&[
            b"persistent-root-v1",
            volume.as_bytes(),
            native.root.as_bytes(),
        ]);
        let manifest = manifest::verify(&first, root)?;
        let inventory_bytes = u64::try_from(claims.len())
            .map_err(io::Error::other)?
            .saturating_mul(4096);
        let mut s = Self {
            claims,
            selected,
            inventory,
            mounts,
            credentials,
            root,
            identity: Identity {
                volume,
                root: persistent_root,
                upcase: native.upcase_sha256.clone(),
            },
            native,
            nonce: random()?,
            manifest,
            original_facts: first.facts,
            generation: 0,
            deadline: deadline(1800)?,
            observation: None,
            bytes: first
                .stats
                .bytes
                .saturating_add(second.stats.bytes)
                .saturating_add(inventory_bytes),
            logical: 0,
            requests: 0,
            invalid: false,
            cancellation_fd: None,
            stream: crate::stream::Stream::default(),
        };
        s.revalidate()?;
        Ok(s)
    }
    pub(super) fn deadline(&self, seconds: u64) -> io::Result<Instant> {
        let mut end = deadline(seconds)?.min(self.deadline);
        if let Some(observation) = self.observation {
            end = end.min(observation);
        }
        if self.invalid || end <= Instant::now() {
            return Err(refusal("expired or invalidated session"));
        }
        Ok(end)
    }
    fn revalidate(&mut self) -> io::Result<()> {
        self.deadline(20)?;
        revalidate(&self.claims)?;
        let (inventory, unresolved) = inventory()?;
        if inventory != self.inventory
            || !unresolved.is_empty()
            || non_ntfs_mounts()? != self.mounts
        {
            return Err(refusal("session inventory changed"));
        }
        self.bytes = self
            .bytes
            .checked_add(
                u64::try_from(self.claims.len())
                    .map_err(io::Error::other)?
                    .saturating_mul(4096),
            )
            .ok_or_else(|| refusal("budget overflow"))?;
        if self.bytes > 16 * 1024 * 1024 * 1024 {
            return Err(refusal("physical session read budget"));
        }
        Ok(())
    }
    fn run(&mut self, operation: NativeOperation) -> io::Result<crate::protocol::Report> {
        self.revalidate()?;
        let end = self.deadline(20)?;
        let claim = self
            .claims
            .get(self.selected)
            .ok_or_else(|| refusal("claim lost"))?;
        let budget = (16_u64 * 1024 * 1024 * 1024)
            .checked_sub(self.bytes)
            .ok_or_else(|| refusal("physical budget"))?
            .min(limits::READ_BYTES);
        let report = limits::run_cancellable(
            &claim.device,
            claim.observation.bytes,
            operation,
            budget,
            Some(self.credentials),
            end,
            self.cancellation_fd,
        )?;
        self.bytes = self
            .bytes
            .checked_add(report.stats.bytes)
            .ok_or_else(|| refusal("physical budget overflow"))?;
        if report.status != 0 {
            return Err(refusal("native operation refused"));
        }
        self.revalidate()?;
        Ok(report)
    }
    pub(super) fn begin(&mut self) -> io::Result<u64> {
        if self.observation.is_some() {
            return Err(refusal("observation already active"));
        }
        self.observation = Some(deadline(600)?);
        self.logical = 0;
        self.stream = crate::stream::Stream::default();
        let report = self.run(NativeOperation::Manifest { root: self.root })?;
        if report.facts != self.original_facts {
            return Err(refusal("manifest changed under claim"));
        }
        self.manifest = manifest::verify(&report, self.root)?;
        self.generation = self
            .generation
            .checked_add(1)
            .ok_or_else(|| refusal("generation exhausted"))?;
        for e in &mut self.manifest.entries {
            e.token = digest(&[
                self.nonce.as_bytes(),
                &self.generation.to_le_bytes(),
                &e.record.to_le_bytes(),
                &e.sequence.to_le_bytes(),
                &serde_json::to_vec(&e.path)?,
                e.metadata_digest.as_bytes(),
            ]);
            if e.content_readable && e.size == 0 {
                self.stream
                    .hashes
                    .insert(e.token.clone(), format!("{:x}", Sha256::digest([])));
            }
        }
        Ok(self.generation)
    }
    pub(super) fn read(&mut self, token: &str, offset: u64, length: u32) -> io::Result<Vec<u8>> {
        if self.observation.is_none() || length == 0 || length > 1_048_576 || self.requests >= 4096
        {
            return Err(refusal("read transaction limit"));
        }
        let e: Entry = self
            .manifest
            .entries
            .iter()
            .find(|e| e.token == token)
            .cloned()
            .ok_or_else(|| refusal("unknown object token"))?;
        if !e.content_readable
            || offset > e.size
            || u64::from(length) > e.size.saturating_sub(offset)
        {
            return Err(refusal("content range refused"));
        }
        self.logical = self
            .logical
            .checked_add(u64::from(length))
            .ok_or_else(|| refusal("logical budget overflow"))?;
        if self.logical > qos_ntfs_protocol::HASH_BYTES {
            return Err(refusal("observation hash budget"));
        }
        self.requests = self
            .requests
            .checked_add(1)
            .ok_or_else(|| refusal("request budget"))?;
        let request = ReadRequest {
            serial: u64::from_str_radix(&self.native.serial, 16).map_err(io::Error::other)?,
            root_record: self.root,
            root_creation: self.native.root_creation,
            root_sequence: u16::try_from(self.native.root_sequence).map_err(io::Error::other)?,
            object_record: e.record,
            object_sequence: e.sequence,
            size: e.size,
            offset,
            length,
            reserved: 0,
        };
        let contiguous = self.stream.contiguous(token, offset);
        if !contiguous {
            self.health_boundary()?;
        }
        let report = self.run(NativeOperation::ReadChunk { request })?;
        if manifest::metadata_digest(&report.facts, e.record)? != e.metadata_digest {
            return Err(refusal("object metadata changed"));
        }
        let bytes = limits::decode_chunk(&report, length)?;
        self.stream.consume(token, e.size, offset, &bytes);
        if !self.stream.active() {
            self.health_boundary()?;
        }
        Ok(bytes)
    }
    fn health_boundary(&mut self) -> io::Result<()> {
        let report = self.run(NativeOperation::Manifest { root: self.root })?;
        if report.facts != self.original_facts {
            return Err(refusal(
                "health or manifest changed at transaction boundary",
            ));
        }
        Ok(())
    }
    pub(super) fn finish(&mut self) -> io::Result<Finished> {
        if self.observation.is_none() {
            return Err(refusal("no observation"));
        }
        let report = self.run(NativeOperation::Manifest { root: self.root })?;
        if report.facts != self.original_facts {
            return Err(refusal("postflight changed"));
        }
        let issues = self
            .manifest
            .entries
            .iter()
            .filter(|e| !e.directory && !self.stream.hashes.contains_key(&e.token))
            .map(|e| Issue {
                path: Some(e.path.clone()),
                reason: "content-hash-incomplete".into(),
            })
            .collect();
        self.observation = None;
        self.stream.clear_active();
        Ok((
            std::mem::take(&mut self.stream.hashes)
                .into_iter()
                .collect(),
            issues,
        ))
    }
    pub(super) fn invalidate(&mut self) {
        self.invalid = true;
        self.stream = crate::stream::Stream::default();
    }
}
