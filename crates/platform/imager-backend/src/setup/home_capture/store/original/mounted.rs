//! Immutable mounted-root registration on the existing protected source Store.
//! No caller-authored group facts and no coherent epoch or output are minted.
use super::*;
#[cfg(all(test, feature = "native-capture-qualification"))]
use crate::setup::GrantedHomeRoots;
mod epoch;
mod participants;
use crate::setup::{
    RootKind,
    migration::check_cancel,
    migration_handoff::{EntryBinding, EntryKind},
};
pub(in crate::setup::home_capture) use epoch::MountedReadEpoch;
use participants::{ParticipantReads, ParticipantSet, ParticipantWitness};
use sha2::Digest;
use std::os::unix::fs::MetadataExt;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MountedEntry {
    placement: NativePath,
    pub(super) binding: EntryBinding,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    participant: Option<ParticipantWitness>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct MountedRecord {
    schema: u32,
    kind: String,
    primary: SourceWitness,
    pub(super) caller: OriginalCallerFacts,
    pub(super) entries: Vec<MountedEntry>,
}
/// A hint authenticates only after reopening the protected SAME-source record.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MountedWitness {
    schema: u32,
    primary: SourceWitness,
    sha256: [u8; 32],
}
impl MountedWitness {
    pub fn primary(&self) -> &SourceWitness {
        &self.primary
    }
    pub fn matches_source(&self, source: &SourceWitness) -> io::Result<()> {
        if self.schema != 1 || &self.primary != source || self.sha256 == [0; 32] {
            return Err(failure(
                "Mounted witness differs from original source declaration",
            ));
        }
        Ok(())
    }
    pub(in crate::setup::home_capture) fn operation(&self) -> &str {
        &self.primary.operation
    }
}
impl MountedRecord {
    fn validate(&self, primary: &SourceIntent) -> io::Result<()> {
        primary.validate()?;
        if !matches!(self.schema, 1 | 2)
            || self.kind != "original-mounted-root-set"
            || self.primary != primary.witness()?
            || self.caller != primary.original
            || self.entries.is_empty()
            || self.entries.len() > 256
        {
            return Err(failure(
                "Mounted-root record differs from protected primary",
            ));
        }
        self.caller.validate(primary.original.uid())?;
        let mut placements = Vec::new();
        for entry in &self.entries {
            let placement = entry.placement.decode().map_err(io::Error::other)?;
            crate::setup::migration_handoff::relative(&placement)?;
            entry.binding.validate()?;
            if entry.binding.volume() == &primary.home.home.volume {
                if entry.participant.is_some() {
                    return Err(failure("Primary-volume entry cannot borrow a participant"));
                }
            } else {
                if self.schema != 2 {
                    return Err(failure(
                        "Legacy mounted record cannot authorize foreign roots",
                    ));
                }
                entry
                    .participant
                    .as_ref()
                    .ok_or_else(|| failure("Foreign root protected participant missing"))?
                    .validate(primary, &entry.binding)?;
            }
            if placements
                .iter()
                .any(|old: &PathBuf| placement.starts_with(old) || old.starts_with(&placement))
            {
                return Err(failure(
                    "Mounted roots overlap or lack same-source protected custody",
                ));
            }
            if placements
                .last()
                .is_some_and(|previous| previous >= &placement)
            {
                return Err(failure("Mounted root placement order is not canonical"));
            }
            placements.push(placement);
        }
        Ok(())
    }
    fn witness(&self) -> io::Result<MountedWitness> {
        Ok(MountedWitness {
            schema: 1,
            primary: self.primary.clone(),
            sha256: sha2::Sha256::digest(serde_json::to_vec(self)?).into(),
        })
    }
}
fn read_private(file: &File) -> io::Result<Vec<u8>> {
    use std::io::{Seek, SeekFrom};
    let metadata = file.metadata()?;
    if metadata.uid() != 0
        || metadata.nlink() != 1
        || !metadata.is_file()
        || metadata.mode() & 0o077 != 0
        || metadata.len() > MAX_RECORD
    {
        return Err(io::ErrorKind::PermissionDenied.into());
    }
    let mut reader = file.try_clone()?;
    reader.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    reader.take(MAX_RECORD + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_RECORD || stamp(&metadata) != stamp(&file.metadata()?) {
        return Err(failure("Protected mounted record changed"));
    }
    Ok(bytes)
}
fn stamp(m: &std::fs::Metadata) -> (u64, u64, u32, u32, u64, i64, i64, i64, i64) {
    (
        m.dev(),
        m.ino(),
        m.uid(),
        m.mode(),
        m.len(),
        m.mtime(),
        m.mtime_nsec(),
        m.ctime(),
        m.ctime_nsec(),
    )
}
impl Store {
    /// Existing authenticated root service only. The old primary registration
    /// must already exist and match the fresh actual peer; sources may have
    /// different owners, but their owner IDs NEVER become reader credentials.
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup::home_capture) fn register_mounted_roots(
        &self,
        home: &Path,
        plan: &Plan,
        roots: &GrantedHomeRoots,
        peer: &HostChannel,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<MountedWitness> {
        roots.with_roots(cancel, |granted| {
            self.register_mounted_entries(home, plan, granted, peer, cancel)
                .map(|(witness, _custody, _file, _bytes)| witness)
        })
    }
    pub(in crate::setup::home_capture) fn register_transferred_mounted_roots(
        self: &Arc<Self>,
        home: &Path,
        plan: &Plan,
        inputs: Vec<(NativePath, EntryKind, File)>,
        peer: &HostChannel,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<MountedRegistration> {
        let directory = storage(&self.records)?;
        let name = format!("{}.json", self.operation);
        let file = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        let primary: SourceIntent =
            match serde_json::from_slice::<DurableRecord>(&read_private(&file)?)? {
                DurableRecord::Original(intent) => intent,
                DurableRecord::Capture(_) => {
                    return Err(failure("Snapshot cannot register mounted inputs"));
                }
            };
        primary.validate()?;
        let caller = peer.original_caller_facts()?;
        if primary.home != *plan
            || primary.original != caller
            || plan.operation_id != self.operation
        {
            return Err(failure(
                "Transferred roots differ from protected source/caller",
            ));
        }
        let held = PinnedDirectory::open_absolute(home)?;
        let report = crate::setup::migration_review::MountedHomeLocations::discover_original(
            &held,
            home,
            &primary.omissions,
            cancel,
        )?;
        let roots =
            crate::setup::migration_review::TransferredHomeRoots::admit(report, inputs, cancel)?;
        roots.with_roots(cancel, |granted| {
            let (witness, participants, aggregate_file, aggregate_bytes) =
                self.register_mounted_entries(home, plan, granted, peer, cancel)?;
            Ok(MountedRegistration {
                primary: self.clone(),
                participants,
                aggregate_file,
                aggregate_bytes,
                witness,
                peer: peer.original_peer(),
            })
        })
    }
    fn register_mounted_entries(
        &self,
        home: &Path,
        plan: &Plan,
        granted: &[crate::setup::GrantedRoot<'_>],
        peer: &HostChannel,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<(MountedWitness, ParticipantSet, File, Vec<u8>)> {
        if rustix::process::geteuid().as_raw() != 0 || self.operation != plan.operation_id {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let caller = peer.original_caller_facts()?;
        caller.validate(plan.home_uid())?;
        let directory = storage(&self.records)?;
        directory.validate_privacy(Privacy::OwnerOnly)?;
        let primary_name = format!("{}.json", self.operation);
        let primary_file = directory.open_read(OsStr::new(&primary_name), Privacy::OwnerOnly)?;
        let primary_bytes = read_private(&primary_file)?;
        let primary: SourceIntent = match serde_json::from_slice::<DurableRecord>(&primary_bytes)? {
            DurableRecord::Original(intent) => intent,
            DurableRecord::Capture(_) => {
                return Err(failure(
                    "Snapshot record cannot register original mounted roots",
                ));
            }
        };
        primary.validate()?;
        if primary.home != *plan || primary.original != caller {
            return Err(failure(
                "Primary source/peer differs from registered intent",
            ));
        }
        let records = self
            .records_path
            .as_ref()
            .ok_or_else(|| failure("Broker records namespace missing"))?;
        if NativePath::encode(&crate::setup::migration_handoff::original_record_namespace(
            plan,
            home,
            records,
            &self.records,
        )?) != primary.namespace
        {
            return Err(failure("Primary record namespace changed"));
        }
        let root = granted
            .first()
            .ok_or_else(|| failure("Primary grant missing"))?;
        let held = PinnedDirectory::open_absolute(home)?;
        let actual = root.file().metadata()?;
        let expected = held.file.metadata()?;
        if !root.placement().as_os_str().is_empty()
            || root.kind() != RootKind::Directory
            || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
        {
            return Err(failure("Mounted-root bundle belongs to a different Home"));
        }
        // Stage every actual foreign filesystem before primary publication.
        // Leases/held grants remain alive until final primary ACK below.
        let participants = ParticipantSet::stage(self, home, &primary, granted, peer, cancel)?;
        let mut entries = Vec::new();
        for root in &granted[1..] {
            crate::setup::migration::check_cancel(cancel)?;
            peer.validate_peer()?;
            let kind = match root.kind() {
                RootKind::Directory => EntryKind::Directory,
                RootKind::File => EntryKind::File,
            };
            let binding = crate::setup::migration_handoff::capture_mounted_entry(
                &home.join(root.placement()),
                root.file(),
                kind,
            )?;
            let participant = participants.witness_for(&binding);
            if binding.volume() == &primary.home.home.volume {
                if NativePath::encode(&crate::setup::migration_handoff::mounted_record_namespace(
                    &binding,
                    records,
                    &self.records,
                )?) != primary.namespace
                {
                    return Err(failure("Mounted source protected namespace differs"));
                }
            } else if participant.is_none() {
                return Err(failure(
                    "Foreign root independently protected participant missing",
                ));
            }
            entries.push(MountedEntry {
                placement: NativePath::encode(root.placement()),
                binding,
                participant,
            });
        }
        let record = MountedRecord {
            schema: if entries.iter().any(|entry| entry.participant.is_some()) {
                2
            } else {
                1
            },
            kind: "original-mounted-root-set".into(),
            primary: primary.witness()?,
            caller: caller.clone(),
            entries,
        };
        record.validate(&primary)?;
        let bytes = serde_json::to_vec(&record)?;
        if bytes.len() as u64 > MAX_RECORD {
            return Err(failure("Mounted record exceeds protected bound"));
        }
        if peer.original_caller_facts()? != caller {
            return Err(failure("Mounted registration peer changed"));
        }
        let name = format!("{}.mounted-roots.json", self.operation);
        participants.verify(self, &peer.original_peer(), cancel)?;
        crate::setup::migration::check_cancel(cancel)?;
        match directory.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
            Ok(file) => {
                if read_private(&file)? != bytes {
                    return Err(failure(
                        "Mounted registration conflicts with protected record",
                    ));
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => directory
                .publish(
                    OsStr::new(&name),
                    &bytes,
                    PublishMode::IfAbsent,
                    Privacy::OwnerOnly,
                )
                .map_err(io::Error::other)?,
            Err(error) => return Err(error),
        }
        let named = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        if read_private(&named)? != bytes
            || read_private(&primary_file)? != primary_bytes
            || peer.original_caller_facts()? != caller
        {
            return Err(failure(
                "Mounted registration changed before acknowledgement",
            ));
        }
        let current_primary = directory.open_read(OsStr::new(&primary_name), Privacy::OwnerOnly)?;
        if stamp(&current_primary.metadata()?) != stamp(&primary_file.metadata()?) {
            return Err(failure(
                "Primary record replaced during mounted registration",
            ));
        }
        self.records.validate_destination()?;
        participants.verify(self, &peer.original_peer(), cancel)?;
        Ok((record.witness()?, participants, named, bytes))
    }
}
impl SourceReadContext {
    /// Actual source inputs for the namespace bootstrap. This callback retains
    /// protected record and readonly source custody throughout use. Caller IDs
    /// remain the original admitted reader, not any mounted-entry owner.
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup::home_capture) fn with_mounted_sources<T>(
        &self,
        witness: &MountedWitness,
        cancel: &std::sync::atomic::AtomicBool,
        use_sources: impl FnOnce(&[OriginalMountedSource]) -> io::Result<T>,
    ) -> io::Result<T> {
        self.retain_mounted_sources(witness, cancel)?
            .with_sources(cancel, use_sources)
    }

    /// Native retained custody for the later signed namespace admission. JSON
    /// witnesses only select an exact independently verified protected record.
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup::home_capture) fn retain_mounted_sources(
        &self,
        witness: &MountedWitness,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<MountedReadEpoch> {
        MountedReadEpoch::reopen(self, witness, cancel)
    }

    /// Explicit boot export action: protected records must authenticate before
    /// any missing foreign volume is mounted through the existing UDisks owner.
    pub(in crate::setup::home_capture) fn retain_mounted_for_export(
        &self,
        witness: &MountedWitness,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<MountedReadEpoch> {
        MountedReadEpoch::reopen_for_export(self, witness, cancel)
    }
    pub fn append_mounted_source_geometry(
        &self,
        witness: &MountedWitness,
        plans: &mut Vec<Vec<u8>>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        let epoch = Arc::new(self.retain_mounted_for_export(witness, cancel)?);
        let result = epoch.append_source_geometry(plans, cancel);
        epoch.retire()?;
        result
    }

    /// Reopen the finite mounted record in this existing readonly same-source
    /// lease. Returned data contains metadata, not a group-setting capability.
    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(in crate::setup::home_capture) fn verify_mounted_witness(
        &self,
        witness: &MountedWitness,
    ) -> io::Result<()> {
        self.revalidate()?;
        if witness.schema != 1 || witness.primary != self.intent.witness()? {
            return Err(failure("Mounted witness belongs to another source intent"));
        }
        let namespace = self.intent.namespace.decode().map_err(io::Error::other)?;
        let file = self.source.open_original_mounted_record(&namespace)?;
        let bytes = read_private(&file)?;
        if <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != witness.sha256 {
            return Err(failure("Mounted witness protected bytes differ"));
        }
        let record: MountedRecord = serde_json::from_slice(&bytes)?;
        record.validate(&self.intent)?;
        // Existing bounded metadata-only verifier has no cancellation input.
        // Cancellable source callbacks supply their live flag separately.
        let participants = ParticipantReads::review(&self.intent, &record.entries, None)?;
        let renewed = self.source.open_original_mounted_record(&namespace)?;
        if stamp(&file.metadata()?) != stamp(&renewed.metadata()?)
            || read_private(&renewed)? != bytes
        {
            return Err(failure("Mounted source record changed during reopen"));
        }
        participants.verify(&self.intent, None)?;
        self.revalidate()
    }
}

/// Cannot be minted from a descriptor supplied by a public action. Only the
/// protected readonly source-context reopen above constructs this projection.
pub(in crate::setup::home_capture) struct OriginalMountedSource {
    placement: PathBuf,
    kind: EntryKind,
    file: File,
}
impl OriginalMountedSource {
    pub(in crate::setup::home_capture) fn placement(&self) -> &Path {
        &self.placement
    }
    pub(in crate::setup::home_capture) fn kind(&self) -> EntryKind {
        self.kind
    }
    pub(in crate::setup::home_capture) fn file(&self) -> &File {
        &self.file
    }
}
fn same_entry(left: &std::fs::Metadata, right: &std::fs::Metadata) -> bool {
    (left.dev(), left.ino(), left.uid(), left.gid(), left.mode())
        == (
            right.dev(),
            right.ino(),
            right.uid(),
            right.gid(),
            right.mode(),
        )
}

/// Device-local custody only. Never Serialize/Deserialize and never constructed
/// from a witness: the actual operation and per-volume leases survive RPC ACK.
pub(in crate::setup::home_capture) struct MountedRegistration {
    primary: Arc<Store>,
    participants: ParticipantSet,
    aggregate_file: File,
    aggregate_bytes: Vec<u8>,
    witness: MountedWitness,
    peer: crate::capture::process::OriginalCaller,
}
impl MountedRegistration {
    pub(in crate::setup::home_capture) fn original_caller(
        &self,
    ) -> io::Result<OriginalCallerFacts> {
        self.peer.original_caller_facts()
    }
    pub(in crate::setup::home_capture) fn witness(&self) -> &MountedWitness {
        &self.witness
    }
    pub(in crate::setup::home_capture) fn validate_ack(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        self.participants
            .verify(&self.primary, &self.peer, cancel)?;
        self.primary.records.validate_destination()?;
        let directory = storage(&self.primary.records)?;
        let name = format!("{}.mounted-roots.json", self.primary.operation);
        let named = directory.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
        if stamp(&named.metadata()?) != stamp(&self.aggregate_file.metadata()?)
            || read_private(&named)? != self.aggregate_bytes
            || read_private(&self.aggregate_file)? != self.aggregate_bytes
        {
            return Err(failure("Mounted aggregate changed before acknowledgement"));
        }
        self.primary.records.validate_destination()?;
        self.participants.verify(&self.primary, &self.peer, cancel)
    }
}
