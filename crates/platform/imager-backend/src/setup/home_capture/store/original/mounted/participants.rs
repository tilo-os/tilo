//! Per-volume root service records. Participants retain their own operation
//! leases through primary publication/ACK; incomplete matching retries never
//! overwrite a conflicting record or constitute an export epoch.
use super::*;
use crate::setup::migration_handoff::{ParticipantNamespace, SourceVolume};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ParticipantWitness {
    schema: u32,
    primary: SourceWitness,
    volume: SourceVolume,
    sha256: [u8; 32],
}
#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ParticipantRecord {
    schema: u32,
    kind: String,
    primary: SourceWitness,
    caller: OriginalCallerFacts,
    entries: Vec<MountedEntry>,
}
struct Entry {
    path: PathBuf,
    selected: File,
    record: MountedEntry,
}
struct Participant {
    namespace: ParticipantNamespace,
    store: Store,
    file: File,
    bytes: Vec<u8>,
    witness: ParticipantWitness,
}
pub(in crate::setup::home_capture) struct ParticipantSet {
    primary_file: File,
    primary_home: PathBuf,
    primary_plan: Plan,
    primary_source: Arc<PinnedDirectory>,
    primary_bytes: Vec<u8>,
    caller: OriginalCallerFacts,
    entries: Vec<Entry>,
    participants: Vec<Participant>,
}
impl ParticipantRecord {
    fn validate(&self, primary: &SourceIntent, volume: &SourceVolume) -> io::Result<()> {
        primary.validate()?;
        if self.schema != 1
            || self.kind != "original-mounted-participant"
            || self.primary != primary.witness()?
            || self.caller != primary.original
            || self.entries.is_empty()
            || self.entries.len() > 256
        {
            return Err(failure(
                "Participant record differs from primary caller/Home",
            ));
        }
        let mut previous: Option<PathBuf> = None;
        for entry in &self.entries {
            let path = entry.placement.decode().map_err(io::Error::other)?;
            crate::setup::migration_handoff::relative(&path)?;
            entry.binding.validate()?;
            if entry.binding.volume() != volume
                || entry.participant.is_some()
                || previous
                    .as_ref()
                    .is_some_and(|old| old >= &path || path.starts_with(old))
            {
                return Err(failure(
                    "Participant record has conflicting source placements",
                ));
            }
            previous = Some(path);
        }
        Ok(())
    }
}
impl ParticipantWitness {
    pub(super) fn validate(
        &self,
        primary: &SourceIntent,
        binding: &EntryBinding,
    ) -> io::Result<()> {
        if self.schema != 1
            || self.primary != primary.witness()?
            || self.volume != *binding.volume()
            || self.volume == primary.home.home.volume
        {
            return Err(failure(
                "Participant hint differs from exact foreign source",
            ));
        }
        Ok(())
    }
}
impl ParticipantSet {
    pub(super) fn stage(
        primary_store: &Store,
        home: &Path,
        primary: &SourceIntent,
        roots: &[crate::setup::GrantedRoot<'_>],
        peer: &HostChannel,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Self> {
        if rustix::process::geteuid().as_raw() != 0
            || primary_store.operation != primary.home.operation_id
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        check_cancel(cancel)?;
        let caller = peer.original_caller_facts()?;
        if caller != primary.original {
            return Err(failure("Participant peer differs from original caller"));
        }
        let directory = storage(&primary_store.records)?;
        let primary_file = directory.open_read(
            OsStr::new(&format!("{}.json", primary_store.operation)),
            Privacy::OwnerOnly,
        )?;
        let primary_bytes = read_private(&primary_file)?;
        let expected = serde_json::to_vec(&DurableRecord::Original(primary.clone()))?;
        if primary_bytes != expected {
            return Err(failure("Participant primary record changed"));
        }
        let (_, mounted) = roots
            .split_first()
            .ok_or_else(|| failure("Participant primary grant missing"))?;
        let mut entries = Vec::new();
        for root in mounted {
            check_cancel(cancel)?;
            if entries.len() >= 256 {
                return Err(failure("Participant count exceeds bound"));
            }
            let path = home.join(root.placement());
            let kind = match root.kind() {
                RootKind::Directory => EntryKind::Directory,
                RootKind::File => EntryKind::File,
            };
            let binding =
                crate::setup::migration_handoff::capture_mounted_entry(&path, root.file(), kind)?;
            entries.push(Entry {
                path,
                selected: root.file().try_clone()?,
                record: MountedEntry {
                    placement: NativePath::encode(root.placement()),
                    binding,
                    participant: None,
                },
            });
        }
        let mut result = Self {
            primary_file,
            primary_home: home.to_path_buf(),
            primary_plan: primary.home.clone(),
            primary_source: PinnedDirectory::open_absolute(home)?,
            primary_bytes,
            caller,
            entries,
            participants: Vec::new(),
        };
        for entry in &result.entries {
            if entry.record.binding.volume() == &primary.home.home.volume {
                continue;
            }
            if result
                .participants
                .iter()
                .any(|participant| &participant.witness.volume == entry.record.binding.volume())
            {
                continue;
            }
            check_cancel(cancel)?;
            if peer.original_caller_facts()? != result.caller {
                return Err(failure("Participant caller retired"));
            }
            let namespace =
                ParticipantNamespace::admit(&entry.path, &entry.selected, &entry.record.binding)?;
            // Fixed root-controlled children only; never chmod/chown a user's
            // filesystem root, accept a payload locator or cross another mount.
            let mut parent = namespace.root().clone();
            for (name, private) in [
                ("var", false),
                ("lib", false),
                ("tilo-imager", false),
                ("home-capture", true),
            ] {
                parent = directory_child(&parent, name, private, namespace.root())?;
            }
            let mut store = Store::open(parent, &primary_store.operation)?;
            let records = namespace
                .point()
                .join("var/lib/tilo-imager/home-capture/records");
            store.records_path = Some(records.clone());
            if crate::setup::migration_handoff::mounted_record_namespace(
                &entry.record.binding,
                &records,
                &store.records,
            )? != Path::new("var/lib/tilo-imager/home-capture/records")
            {
                return Err(failure(
                    "Participant record is outside exact fixed filesystem namespace",
                ));
            }
            let record = ParticipantRecord {
                schema: 1,
                kind: "original-mounted-participant".into(),
                primary: primary.witness()?,
                caller: result.caller.clone(),
                entries: result
                    .entries
                    .iter()
                    .filter(|candidate| {
                        candidate.record.binding.volume() == entry.record.binding.volume()
                    })
                    .map(|candidate| candidate.record.clone())
                    .collect(),
            };
            record.validate(primary, entry.record.binding.volume())?;
            let bytes = serde_json::to_vec(&record)?;
            if bytes.len() as u64 > MAX_RECORD {
                return Err(failure("Participant record exceeds bound"));
            }
            let witness = ParticipantWitness {
                schema: 1,
                primary: record.primary.clone(),
                volume: entry.record.binding.volume().clone(),
                sha256: sha2::Sha256::digest(&bytes).into(),
            };
            let held = storage(&store.records)?;
            let name = format!("{}.mounted-participant.json", primary_store.operation);
            check_cancel(cancel)?;
            match held.open_read(OsStr::new(&name), Privacy::OwnerOnly) {
                Ok(file) => {
                    if read_private(&file)? != bytes {
                        return Err(failure(
                            "Participant conflicts with protected matching retry",
                        ));
                    }
                }
                Err(error) if error.kind() == io::ErrorKind::NotFound => held
                    .publish(
                        OsStr::new(&name),
                        &bytes,
                        PublishMode::IfAbsent,
                        Privacy::OwnerOnly,
                    )
                    .map_err(io::Error::other)?,
                Err(error) => return Err(error),
            }
            let file = held.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
            result.participants.push(Participant {
                namespace,
                store,
                file,
                bytes,
                witness,
            });
        }
        result.verify(primary_store, &peer.original_peer(), cancel)?;
        Ok(result)
    }
    pub(super) fn witness_for(&self, entry: &EntryBinding) -> Option<ParticipantWitness> {
        self.participants
            .iter()
            .find(|part| &part.witness.volume == entry.volume())
            .map(|part| part.witness.clone())
    }
    pub(super) fn verify(
        &self,
        primary_store: &Store,
        peer: &crate::capture::process::OriginalCaller,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        if peer.original_caller_facts()? != self.caller {
            return Err(failure("Participant actual peer changed before ACK"));
        }
        self.primary_source.validate()?;
        let source = crate::setup::migration_handoff::capture(
            &self.primary_home,
            &self.primary_plan.operation_id,
            self.primary_plan.categories,
        )?;
        if source.home != self.primary_plan.home {
            return Err(failure("Participant primary Home grant changed before ACK"));
        }
        primary_store.check_original_output_operation(&primary_store.operation)?;
        let held = storage(&primary_store.records)?;
        let named = held.open_read(
            OsStr::new(&format!("{}.json", primary_store.operation)),
            Privacy::OwnerOnly,
        )?;
        if stamp(&named.metadata()?) != stamp(&self.primary_file.metadata()?)
            || read_private(&named)? != self.primary_bytes
        {
            return Err(failure("Participant primary lineage changed"));
        }
        for participant in &self.participants {
            participant.namespace.validate()?;
            participant
                .store
                .check_original_output_operation(&primary_store.operation)?;
            let held = storage(&participant.store.records)?;
            let named = held.open_read(
                OsStr::new(&format!(
                    "{}.mounted-participant.json",
                    primary_store.operation
                )),
                Privacy::OwnerOnly,
            )?;
            if stamp(&named.metadata()?) != stamp(&participant.file.metadata()?)
                || read_private(&named)? != participant.bytes
            {
                return Err(failure("Participant protected record changed"));
            }
            if fs_native::snapshot_lease::mount_id(&participant.store.records.file)?
                != fs_native::snapshot_lease::mount_id(&participant.namespace.root().file)?
            {
                return Err(failure("Participant records crossed a mounted namespace"));
            }
        }
        for entry in &self.entries {
            if crate::setup::migration_handoff::capture_mounted_entry(
                &entry.path,
                &entry.selected,
                entry.record.binding.kind(),
            )? != entry.record.binding
            {
                return Err(failure("Participant approved source changed before ACK"));
            }
        }
        if peer.original_caller_facts()? != self.caller {
            return Err(failure("Participant peer changed during final review"));
        }
        check_cancel(cancel)
    }
}
fn directory_child(
    parent: &Arc<PinnedDirectory>,
    name: &str,
    private: bool,
    source_root: &Arc<PinnedDirectory>,
) -> io::Result<Arc<PinnedDirectory>> {
    let child = directory(parent, name, private)?;
    if fs_native::snapshot_lease::mount_id(&child.file)?
        != fs_native::snapshot_lease::mount_id(&source_root.file)?
    {
        return Err(failure(
            "Participant fixed records namespace crosses a mount",
        ));
    }
    Ok(child)
}

/// Borrowed existing readonly mounts only. Held roots/records remain alive
/// throughout the signed worker callback; this type owns no unmount authority.
pub(super) struct ParticipantReads {
    volumes: Vec<ReadParticipant>,
}
struct ReadParticipant {
    namespace: ParticipantNamespace,
    file: File,
    bytes: Vec<u8>,
    record: ParticipantRecord,
    witness: ParticipantWitness,
}
impl ParticipantReads {
    pub(super) fn qualification_owned_roots(&self) -> io::Result<()> {
        if self.volumes.len() != 1 {
            return Err(failure(
                "Qualification requires exactly one genuine foreign volume",
            ));
        }
        for part in &self.volumes {
            part.namespace.qualification_owned_root()?;
        }
        Ok(())
    }

    pub(super) fn review(
        primary: &SourceIntent,
        entries: &[MountedEntry],
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> io::Result<Self> {
        Self::review_mode(primary, entries, cancel, false)
    }
    pub(super) fn review_for_export(
        primary: &SourceIntent,
        entries: &[MountedEntry],
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Self> {
        Self::review_mode(primary, entries, Some(cancel), true)
    }
    fn review_mode(
        primary: &SourceIntent,
        entries: &[MountedEntry],
        cancel: Option<&std::sync::atomic::AtomicBool>,
        own_missing: bool,
    ) -> io::Result<Self> {
        let mut result = Self {
            volumes: Vec::new(),
        };
        let reviewed = (|| {
            for entry in entries {
                check_reader(cancel)?;
                let Some(witness) = &entry.participant else {
                    continue;
                };
                witness.validate(primary, &entry.binding)?;
                if let Some(existing) = result
                    .volumes
                    .iter()
                    .find(|part| part.witness.volume == witness.volume)
                {
                    if existing.witness != *witness {
                        return Err(failure("One participant volume has conflicting witnesses"));
                    }
                    continue;
                }
                result.volumes.push(ReadParticipant::review(
                    primary,
                    entries,
                    entry,
                    own_missing,
                )?);
            }
            result.verify(primary, cancel)
        })();
        match reviewed {
            Ok(()) => Ok(result),
            Err(error) => {
                result.retire()?;
                Err(error)
            }
        }
    }
    pub(super) fn retire(self) -> io::Result<()> {
        let mut error = None;
        for participant in self.volumes {
            let ReadParticipant {
                namespace,
                file,
                bytes,
                record,
                witness,
            } = participant;
            drop((file, bytes, record, witness));
            if let Err(current) = namespace.retire() {
                if error.is_none() {
                    error = Some(current);
                }
            }
        }
        match error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
    pub(super) fn open_entry(&self, entry: &MountedEntry) -> io::Result<File> {
        let witness = entry
            .participant
            .as_ref()
            .ok_or_else(|| failure("Foreign participant missing"))?;
        let part = self
            .volumes
            .iter()
            .find(|part| part.witness == *witness)
            .ok_or_else(|| failure("Foreign participant was not independently reopened"))?;
        if !part.record.entries.iter().any(|approved| {
            approved.placement == entry.placement && approved.binding == entry.binding
        }) {
            return Err(failure("Foreign source placement is not protected"));
        }
        part.namespace.open_entry(&entry.binding)
    }
    pub(super) fn verify(
        &self,
        primary: &SourceIntent,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> io::Result<()> {
        for participant in &self.volumes {
            check_reader(cancel)?;
            participant.namespace.validate()?;
            participant
                .record
                .validate(primary, &participant.witness.volume)?;
            let current = participant
                .namespace
                .open_record(&primary.home.operation_id)?;
            if stamp(&current.metadata()?) != stamp(&participant.file.metadata()?)
                || read_private(&current)? != participant.bytes
            {
                return Err(failure(
                    "Foreign protected record replaced during source use",
                ));
            }
        }
        check_reader(cancel)
    }
}

impl ReadParticipant {
    fn review(
        primary: &SourceIntent,
        entries: &[MountedEntry],
        entry: &MountedEntry,
        own_missing: bool,
    ) -> io::Result<Self> {
        let witness = entry
            .participant
            .as_ref()
            .ok_or_else(|| failure("Participant witness missing"))?;
        let namespace = if own_missing {
            ParticipantNamespace::mount_for_export(&entry.binding)?
        } else {
            ParticipantNamespace::reopen_read_only(&entry.binding)?
        };
        let reviewed = (|| {
            let file = namespace.open_record(&primary.home.operation_id)?;
            let bytes = read_private(&file)?;
            if <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != witness.sha256 {
                return Err(failure("Foreign volume protected participant bytes differ"));
            }
            let record: ParticipantRecord = serde_json::from_slice(&bytes)?;
            record.validate(primary, &witness.volume)?;
            let expected: Vec<_> = entries
                .iter()
                .filter(|candidate| candidate.binding.volume() == &witness.volume)
                .map(|candidate| MountedEntry {
                    placement: candidate.placement.clone(),
                    binding: candidate.binding.clone(),
                    participant: None,
                })
                .collect();
            if record.entries != expected {
                return Err(failure(
                    "Participant placements differ from protected primary table",
                ));
            }
            Ok((file, bytes, record))
        })();
        match reviewed {
            Ok((file, bytes, record)) => Ok(Self {
                namespace,
                file,
                bytes,
                record,
                witness: witness.clone(),
            }),
            Err(error) => {
                namespace.retire()?;
                Err(error)
            }
        }
    }
}

fn check_reader(cancel: Option<&std::sync::atomic::AtomicBool>) -> io::Result<()> {
    if let Some(cancel) = cancel {
        check_cancel(cancel)?;
    }
    Ok(())
}
