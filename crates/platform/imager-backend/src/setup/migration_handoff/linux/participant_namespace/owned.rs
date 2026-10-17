//! Actual owned readonly foreign-volume custody, never a synthetic Home.
use super::*;
pub(super) struct OwnedParticipantMount(Option<Arc<MountedVolume>>);
impl Drop for OwnedParticipantMount {
    fn drop(&mut self) {
        if let Some(owned) = self.0.take() {
            retain_failed_mount(owned);
        }
    }
}
impl OwnedParticipantMount {
    pub(super) fn bind_facts(&self, facts: &mut Facts) -> io::Result<()> {
        self.0
            .as_ref()
            .ok_or_else(|| refusal("Participant mount custody retired"))?
            .service
            .bind_facts(facts)
    }

    fn retire(mut self) -> io::Result<()> {
        retire_owned_mount(
            self.0
                .take()
                .ok_or_else(|| refusal("Participant mount custody retired"))?,
        )
    }
}
impl ParticipantNamespace {
    /// Boot-only export preparation on an exact protected participant binding.
    /// Existing readonly full-root mounts remain borrowed. Newly mounted volumes
    /// use the same held-device kernel no-replay backend as authenticated Home.
    pub fn mount_for_export(entry: &EntryBinding) -> io::Result<Self> {
        entry.validate()?;
        if rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let facts = Facts::read()?;
        facts.matching_volume(entry.volume())?;
        if facts.mounts.iter().any(|mount| {
            facts
                .volume(mount)
                .is_ok_and(|(volume, _)| volume == *entry.volume())
        }) {
            return Self::reopen_read_only(entry);
        }
        let (_point, owned) = super::super::mount_root_role_volume(entry.volume(), &facts)?;
        let current = (|| {
            let mut current = Facts::read()?;
            owned.service.bind_facts(&mut current)?;
            Ok::<_, io::Error>(current)
        })();
        // No automatic Drop acknowledgment. Incomplete or substituted mount
        // evidence quarantines its actual service custody through retirement.
        let result = current.and_then(|facts| {
            let mut resolved = Self::reopen_read_only_with(entry, &facts)?;
            resolved.owned = Some(OwnedParticipantMount(Some(owned.clone())));
            resolved.validate()?;
            Ok(resolved)
        });
        match result {
            Ok(resolved) => Ok(resolved),
            Err(error) => {
                retire_owned_mount(owned)?;
                Err(error)
            }
        }
    }
    /// Consuming retirement only after every source/record/input is closed.
    /// Borrowed mounts carry no release authority; failed owned release remains
    /// quarantined and never becomes a successful cleanup or completed receipt.
    pub fn retire(self) -> io::Result<()> {
        let Self {
            root,
            point,
            mount,
            volume,
            read_only,
            owned,
        } = self;
        drop((root, point, mount, volume, read_only));
        match owned {
            Some(owned) => owned.retire(),
            None => Ok(()),
        }
    }
}
