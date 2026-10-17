//! Retained, independently reopened protected mounted-source read custody.
//! No public JSON, source owner, or caller-selected descriptor constructs this.
use super::*;
use std::sync::Mutex;

/// All protected records and exact readonly inputs survive a supervised worker,
/// even after the callback which attached its admission has returned. This owns
/// foreign-volume unmount rights only in the explicit boot-export constructor.
pub(in crate::setup::home_capture) struct MountedReadEpoch {
    state: Mutex<ReadState>,
}
struct ReadState {
    sources: Vec<OriginalMountedSource>,
    participants: ParticipantReads,
    record_file: File,
    record_bytes: Vec<u8>,
    record: MountedRecord,
    // Close inputs and protected foreign records before the primary source lease.
    primary: SourceReadContext,
}
impl MountedReadEpoch {
    pub(in crate::setup::home_capture) fn qualification_owned_roots(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure("Mounted read custody poisoned"))?;
        state.verify(cancel)?;
        state.participants.qualification_owned_roots()?;
        let expected = [Path::new("mounted-directory"), Path::new("mounted-file")];
        if state.sources.len() != expected.len() {
            return Err(failure(
                "Qualification requires both selected mounted kinds",
            ));
        }
        for (source, placement) in state.sources.iter().zip(expected) {
            let m = source.file.metadata()?;
            if source.placement != placement || m.uid() != 1003 || m.gid() != 1002 {
                return Err(failure(
                    "Qualification mounted source differs from owned fixture",
                ));
            }
        }
        if !state.sources[0].file.metadata()?.is_dir()
            || !state.sources[1].file.metadata()?.is_file()
        {
            return Err(failure(
                "Qualification mounted directory and regular file required",
            ));
        }
        state.verify(cancel)
    }

    #[cfg(all(test, feature = "native-capture-qualification"))]
    pub(super) fn reopen(
        primary: &SourceReadContext,
        witness: &MountedWitness,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Self> {
        Self::reopen_mode(primary, witness, cancel, false)
    }
    pub(super) fn reopen_for_export(
        primary: &SourceReadContext,
        witness: &MountedWitness,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Self> {
        Self::reopen_mode(primary, witness, cancel, true)
    }
    fn reopen_mode(
        primary: &SourceReadContext,
        witness: &MountedWitness,
        cancel: &std::sync::atomic::AtomicBool,
        own_missing: bool,
    ) -> io::Result<Self> {
        check_cancel(cancel)?;
        primary.revalidate()?;
        if witness.schema != 1 || witness.primary != primary.intent.witness()? {
            return Err(failure("Mounted witness belongs to another source intent"));
        }
        // Reopen records rather than cloning their file descriptions: each
        // epoch owns independent offsets, with its actual mount lease retained.
        let primary = primary.reopen_witness(&witness.primary)?;
        let namespace = primary
            .intent
            .namespace
            .decode()
            .map_err(io::Error::other)?;
        let record_file = primary.source.open_original_mounted_record(&namespace)?;
        let record_bytes = read_private(&record_file)?;
        if <[u8; 32]>::from(sha2::Sha256::digest(&record_bytes)) != witness.sha256 {
            return Err(failure("Mounted source protected record differs"));
        }
        let record: MountedRecord = serde_json::from_slice(&record_bytes)?;
        record.validate(&primary.intent)?;
        let participants = if own_missing {
            ParticipantReads::review_for_export(&primary.intent, &record.entries, cancel)?
        } else {
            ParticipantReads::review(&primary.intent, &record.entries, Some(cancel))?
        };
        let mut sources = Vec::with_capacity(record.entries.len());
        let opened = (|| {
            for entry in &record.entries {
                check_cancel(cancel)?;
                sources.push(OriginalMountedSource {
                    placement: entry.placement.decode().map_err(io::Error::other)?,
                    kind: entry.binding.kind(),
                    file: if entry.participant.is_some() {
                        participants.open_entry(entry)?
                    } else {
                        primary.source.open_original_mounted_entry(&entry.binding)?
                    },
                });
            }
            Ok(())
        })();
        if let Err(error) = opened {
            drop(sources);
            participants.retire()?;
            return Err(error);
        }
        let state = ReadState {
            sources,
            participants,
            record_file,
            record_bytes,
            record,
            primary,
        };
        if let Err(error) = state.verify(cancel) {
            state.retire()?;
            return Err(error);
        }
        Ok(Self {
            state: Mutex::new(state),
        })
    }
    pub(in crate::setup::home_capture) fn retire(self: Arc<Self>) -> io::Result<()> {
        let owned = match Arc::try_unwrap(self) {
            Ok(owned) => owned,
            Err(shared) => {
                std::mem::forget(shared);
                return Err(failure("Mounted source epoch still has custody leases"));
            }
        };
        match owned.state.into_inner() {
            Ok(state) => state.retire(),
            Err(poisoned) => {
                std::mem::forget(poisoned.into_inner());
                Err(failure("Mounted source retirement custody poisoned"))
            }
        }
    }
    pub(in crate::setup::home_capture) fn table_sha256(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<[u8; 32]> {
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        self.with_sources(cancel, |sources| {
            use std::os::unix::ffi::OsStrExt;
            let identities = sources
                .iter()
                .map(|source| {
                    let m = source.file.metadata()?;
                    Ok(launcher::owned_service::OriginalMountedIdentity {
                        placement: source.placement.as_os_str().as_bytes().to_vec(),
                        identity: (m.dev(), m.ino(), m.uid(), m.gid(), m.mode()),
                    })
                })
                .collect::<io::Result<Vec<_>>>()?;
            launcher::owned_service::original_mounted_table_sha256(&identities)
        })
    }
    pub(in crate::setup::home_capture) fn launch_inputs(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<Vec<launcher::owned_service::OriginalMountedInput>> {
        self.with_sources(cancel, |sources| {
            sources
                .iter()
                .map(|source| {
                    launcher::owned_service::OriginalMountedInput::from_reviewed(
                        source.placement.clone(),
                        source.file.try_clone()?,
                    )
                })
                .collect()
        })
    }
    pub(in crate::setup::home_capture) fn append_source_geometry(
        &self,
        plans: &mut Vec<Vec<u8>>,
        cancel: &std::sync::atomic::AtomicBool,
    ) -> io::Result<()> {
        let state = self
            .state
            .lock()
            .map_err(|_| failure("Mounted read custody poisoned"))?;
        state.verify(cancel)?;
        let entries = state
            .record
            .entries
            .iter()
            .map(|entry| entry.binding.clone())
            .collect::<Vec<_>>();
        let result = crate::setup::migration_handoff::append_mounted_geometry(
            plans,
            &state.record.primary.operation,
            &entries,
        );
        state.verify_integrity()?;
        check_cancel(cancel)?;
        result
    }
    /// Reviews serialize exact retained record descriptions; the callback runs
    /// unlocked so supervisor custody checks can re-enter during actual work.
    /// Caller cancellation stays live; metadata checks don't mint a false flag.
    pub(in crate::setup::home_capture) fn with_sources<T>(
        &self,
        cancel: &std::sync::atomic::AtomicBool,
        use_sources: impl FnOnce(&[OriginalMountedSource]) -> io::Result<T>,
    ) -> io::Result<T> {
        let sources = {
            let state = self
                .state
                .lock()
                .map_err(|_| failure("Mounted read custody poisoned"))?;
            state.verify(cancel)?;
            state
                .sources
                .iter()
                .map(|source| {
                    Ok(OriginalMountedSource {
                        placement: source.placement().to_owned(),
                        kind: source.kind(),
                        file: source.file().try_clone()?,
                    })
                })
                .collect::<io::Result<Vec<_>>>()?
        };
        let result = use_sources(&sources);
        drop(sources);
        // Failed work still reattests protected custody. No completed receipt
        // or cleanup acknowledgement follows merely from dropping this guard.
        self.state
            .lock()
            .map_err(|_| failure("Mounted read custody poisoned"))?
            .verify_integrity()?;
        check_cancel(cancel)?;
        result
    }
}
impl ReadState {
    fn retire(self) -> io::Result<()> {
        let Self {
            sources,
            participants,
            record_file,
            record_bytes,
            record,
            primary,
        } = self;
        // The boot admission still holds the primary owning source lease; this
        // epoch closes its independent clone before retiring foreign mounts.
        drop((sources, record_file, record_bytes, record, primary));
        participants.retire()
    }
    fn verify(&self, cancel: &std::sync::atomic::AtomicBool) -> io::Result<()> {
        check_cancel(cancel)?;
        self.verify_integrity()?;
        check_cancel(cancel)
    }
    // Integrity is bounded by the finite protected table and record limits.
    // Cancellation cannot suppress post-use custody checks or authorize cleanup.
    fn verify_integrity(&self) -> io::Result<()> {
        self.primary.revalidate()?;
        self.record.validate(&self.primary.intent)?;
        let namespace = self
            .primary
            .intent
            .namespace
            .decode()
            .map_err(io::Error::other)?;
        let current = self
            .primary
            .source
            .open_original_mounted_record(&namespace)?;
        if stamp(&self.record_file.metadata()?) != stamp(&current.metadata()?)
            || read_private(&self.record_file)? != self.record_bytes
            || read_private(&current)? != self.record_bytes
        {
            return Err(failure("Mounted record changed during source use"));
        }
        self.participants.verify(&self.primary.intent, None)?;
        for (entry, source) in self.record.entries.iter().zip(&self.sources) {
            let renewed = if entry.participant.is_some() {
                self.participants.open_entry(entry)?
            } else {
                self.primary
                    .source
                    .open_original_mounted_entry(&entry.binding)?
            };
            if !same_entry(&source.file.metadata()?, &renewed.metadata()?)
                || fs_native::snapshot_lease::mount_id(&source.file)?
                    != fs_native::snapshot_lease::mount_id(&renewed)?
            {
                return Err(failure("Mounted source changed during use"));
            }
        }
        self.participants.verify(&self.primary.intent, None)?;
        self.primary.revalidate()
    }
}
