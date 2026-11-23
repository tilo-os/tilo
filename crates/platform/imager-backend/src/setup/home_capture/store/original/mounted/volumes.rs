//! Per-volume root service records. Volumes retain their own operation
//! leases through primary publication/ACK; incomplete matching retries never
//! overwrite a conflicting record or constitute an export epoch.
use super::*;
use crate::setup::migration_handoff::{SourceVolume, VolumeNamespace};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct VolumeProof {
    schema: u32,
    primary: SourceProof,
    volume: SourceVolume,
    sha256: [u8; 32],
}
#[derive(PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct VolumeRecord {
    schema: u32,
    kind: String,
    primary: SourceProof,
    caller: OriginalCallerFacts,
    entries: Vec<MountedEntry>,
}
struct Entry {
    path: PathBuf,
    selected: File,
    record: MountedEntry,
}
struct VolumeStore {
    namespace: VolumeNamespace,
    store: Store,
    file: File,
    bytes: Vec<u8>,
    proof: VolumeProof,
}
pub(in crate::setup::home_capture) struct VolumeStores {
    primary_file: File,
    primary_home: PathBuf,
    primary_plan: Plan,
    primary_source: Arc<PinnedDirectory>,
    primary_bytes: Vec<u8>,
    caller: OriginalCallerFacts,
    entries: Vec<Entry>,
    volumes: Vec<VolumeStore>,
}
impl VolumeRecord {
    fn validate(&self, primary: &SourceIntent, volume: &SourceVolume) -> io::Result<()> {
        primary.validate()?;
        if self.schema != 1
            || self.kind != "original-mounted-volume"
            || self.primary != primary.proof()?
            || self.caller != primary.original
            || self.entries.is_empty()
            || self.entries.len() > 256
        {
            return Err(failure("Volume record differs from primary caller/Home"));
        }
        let mut previous: Option<PathBuf> = None;
        for entry in &self.entries {
            let path = entry.placement.decode().map_err(io::Error::other)?;
            crate::setup::migration_handoff::relative(&path)?;
            entry.binding.validate()?;
            if entry.binding.volume() != volume
                || entry.volume.is_some()
                || previous
                    .as_ref()
                    .is_some_and(|old| old >= &path || path.starts_with(old))
            {
                return Err(failure("Volume record has conflicting source placements"));
            }
            previous = Some(path);
        }
        Ok(())
    }
}
impl VolumeProof {
    pub(super) fn validate(
        &self,
        primary: &SourceIntent,
        binding: &EntryBinding,
    ) -> io::Result<()> {
        if self.schema != 1
            || self.primary != primary.proof()?
            || self.volume != *binding.volume()
            || self.volume == primary.home.home.volume
        {
            return Err(failure("Volume hint differs from exact foreign source"));
        }
        Ok(())
    }
}
impl VolumeStores {
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
            return Err(failure("Volume peer differs from original caller"));
        }
        let directory = storage(&primary_store.records)?;
        let primary_file = directory.open_read(
            OsStr::new(&format!("{}.json", primary_store.operation)),
            Privacy::OwnerOnly,
        )?;
        let primary_bytes = read_private(&primary_file)?;
        let expected = serde_json::to_vec(&DurableRecord::Original(primary.clone()))?;
        if primary_bytes != expected {
            return Err(failure("Volume primary record changed"));
        }
        let (_, mounted) = roots
            .split_first()
            .ok_or_else(|| failure("Volume primary grant missing"))?;
        let mut entries = Vec::new();
        for root in mounted {
            check_cancel(cancel)?;
            if entries.len() >= 256 {
                return Err(failure("Volume count exceeds bound"));
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
                    volume: None,
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
            volumes: Vec::new(),
        };
        for entry in &result.entries {
            if entry.record.binding.volume() == &primary.home.home.volume {
                continue;
            }
            if result
                .volumes
                .iter()
                .any(|volume| &volume.proof.volume == entry.record.binding.volume())
            {
                continue;
            }
            check_cancel(cancel)?;
            if peer.original_caller_facts()? != result.caller {
                return Err(failure("Volume caller retired"));
            }
            let namespace =
                VolumeNamespace::admit(&entry.path, &entry.selected, &entry.record.binding)?;
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
                    "Volume record is outside exact fixed filesystem namespace",
                ));
            }
            let record = VolumeRecord {
                schema: 1,
                kind: "original-mounted-volume".into(),
                primary: primary.proof()?,
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
                return Err(failure("Volume record exceeds bound"));
            }
            let proof = VolumeProof {
                schema: 1,
                primary: record.primary.clone(),
                volume: entry.record.binding.volume().clone(),
                sha256: sha2::Sha256::digest(&bytes).into(),
            };
            let held = storage(&store.records)?;
            let name = format!("{}.mounted-volume.json", primary_store.operation);
            check_cancel(cancel)?;
            crate::record::publish_or_match(
                &held,
                &name,
                &bytes,
                MAX_RECORD,
                |metadata| {
                    metadata.uid() == 0
                        && metadata.nlink() == 1
                        && metadata.mode().trailing_zeros() >= 6
                },
                || failure("Volume conflicts with protected matching retry"),
            )?;
            let file = held.open_read(OsStr::new(&name), Privacy::OwnerOnly)?;
            result.volumes.push(VolumeStore {
                namespace,
                store,
                file,
                bytes,
                proof,
            });
        }
        result.verify(primary_store, &peer.original_peer(), cancel)?;
        Ok(result)
    }
    pub(super) fn proof_for(&self, entry: &EntryBinding) -> Option<VolumeProof> {
        self.volumes
            .iter()
            .find(|volume| &volume.proof.volume == entry.volume())
            .map(|volume| volume.proof.clone())
    }
    pub(super) fn verify(
        &self,
        primary_store: &Store,
        peer: &crate::capture::process::OriginalCaller,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        check_cancel(cancel)?;
        if peer.original_caller_facts()? != self.caller {
            return Err(failure("Volume actual peer changed before ACK"));
        }
        self.primary_source.validate()?;
        let source = crate::setup::migration_handoff::capture(
            &self.primary_home,
            &self.primary_plan.operation_id,
            self.primary_plan.categories,
        )?;
        if source.home != self.primary_plan.home {
            return Err(failure("Volume primary Home grant changed before ACK"));
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
            return Err(failure("Volume primary lineage changed"));
        }
        for volume in &self.volumes {
            volume.namespace.validate()?;
            volume
                .store
                .check_original_output_operation(&primary_store.operation)?;
            let held = storage(&volume.store.records)?;
            let named = held.open_read(
                OsStr::new(&format!("{}.mounted-volume.json", primary_store.operation)),
                Privacy::OwnerOnly,
            )?;
            if stamp(&named.metadata()?) != stamp(&volume.file.metadata()?)
                || read_private(&named)? != volume.bytes
            {
                return Err(failure("Volume protected record changed"));
            }
            if fs_native::snapshot_lease::mount_id(&volume.store.records.file)?
                != fs_native::snapshot_lease::mount_id(&volume.namespace.root().file)?
            {
                return Err(failure("Volume records crossed a mounted namespace"));
            }
        }
        for entry in &self.entries {
            if crate::setup::migration_handoff::capture_mounted_entry(
                &entry.path,
                &entry.selected,
                entry.record.binding.kind(),
            )? != entry.record.binding
            {
                return Err(failure("Volume approved source changed before ACK"));
            }
        }
        if peer.original_caller_facts()? != self.caller {
            return Err(failure("Volume peer changed during final review"));
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
        return Err(failure("Volume fixed records namespace crosses a mount"));
    }
    Ok(child)
}

/// Borrowed existing readonly mounts only. Held roots/records remain alive
/// throughout the signed worker callback; this type owns no unmount authority.
pub(super) struct VolumeReads {
    volumes: Vec<ReadVolume>,
}
struct ReadVolume {
    namespace: VolumeNamespace,
    file: File,
    bytes: Vec<u8>,
    record: VolumeRecord,
    proof: VolumeProof,
}
impl VolumeReads {
    pub(super) fn qualification_owned_roots(&self) -> io::Result<()> {
        if self.volumes.len() != 1 {
            return Err(failure(
                "Qualification requires exactly one genuine foreign volume",
            ));
        }
        for volume in &self.volumes {
            volume.namespace.qualification_owned_root()?;
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
                let Some(proof) = &entry.volume else {
                    continue;
                };
                proof.validate(primary, &entry.binding)?;
                if let Some(existing) = result
                    .volumes
                    .iter()
                    .find(|volume| volume.proof.volume == proof.volume)
                {
                    if existing.proof != *proof {
                        return Err(failure("One volume has conflicting proofs"));
                    }
                    continue;
                }
                result
                    .volumes
                    .push(ReadVolume::review(primary, entries, entry, own_missing)?);
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
        for volume in self.volumes {
            let ReadVolume {
                namespace,
                file,
                bytes,
                record,
                proof,
            } = volume;
            drop((file, bytes, record, proof));
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
        let proof = entry
            .volume
            .as_ref()
            .ok_or_else(|| failure("Foreign volume missing"))?;
        let volume = self
            .volumes
            .iter()
            .find(|volume| volume.proof == *proof)
            .ok_or_else(|| failure("Foreign volume was not independently reopened"))?;
        if !volume.record.entries.iter().any(|approved| {
            approved.placement == entry.placement && approved.binding == entry.binding
        }) {
            return Err(failure("Foreign source placement is not protected"));
        }
        volume.namespace.open_entry(&entry.binding)
    }
    pub(super) fn verify(
        &self,
        primary: &SourceIntent,
        cancel: Option<&std::sync::atomic::AtomicBool>,
    ) -> io::Result<()> {
        for volume in &self.volumes {
            check_reader(cancel)?;
            volume.namespace.validate()?;
            volume.record.validate(primary, &volume.proof.volume)?;
            let current = volume.namespace.open_record(&primary.home.operation_id)?;
            if stamp(&current.metadata()?) != stamp(&volume.file.metadata()?)
                || read_private(&current)? != volume.bytes
            {
                return Err(failure(
                    "Foreign protected record replaced during source use",
                ));
            }
        }
        check_reader(cancel)
    }
}

impl ReadVolume {
    fn review(
        primary: &SourceIntent,
        entries: &[MountedEntry],
        entry: &MountedEntry,
        own_missing: bool,
    ) -> io::Result<Self> {
        let proof = entry
            .volume
            .as_ref()
            .ok_or_else(|| failure("Volume proof missing"))?;
        let namespace = if own_missing {
            VolumeNamespace::mount_for_export(&entry.binding)?
        } else {
            VolumeNamespace::reopen_read_only(&entry.binding)?
        };
        let reviewed = (|| {
            let file = namespace.open_record(&primary.home.operation_id)?;
            let bytes = read_private(&file)?;
            if <[u8; 32]>::from(sha2::Sha256::digest(&bytes)) != proof.sha256 {
                return Err(failure("Foreign volume's protected bytes differ"));
            }
            let record: VolumeRecord = serde_json::from_slice(&bytes)?;
            record.validate(primary, &proof.volume)?;
            let expected: Vec<_> = entries
                .iter()
                .filter(|candidate| candidate.binding.volume() == &proof.volume)
                .map(|candidate| MountedEntry {
                    placement: candidate.placement.clone(),
                    binding: candidate.binding.clone(),
                    volume: None,
                })
                .collect();
            if record.entries != expected {
                return Err(failure(
                    "Volume placements differ from protected primary table",
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
                proof: proof.clone(),
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
