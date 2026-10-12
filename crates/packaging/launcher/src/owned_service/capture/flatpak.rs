//! Additive authenticated Flatpak source projection; never a serialized grant.
use super::CaptureAdmission;
use crate::owned_service::ServiceAuthority;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io,
    os::{
        fd::{AsRawFd, RawFd},
        unix::fs::{FileExt, MetadataExt},
    },
    sync::Arc,
};
pub const ORIGINAL_FLATPAK_INVENTORY_PATH: &str = "/run/tilo/migration/flatpak-inventory.json";
pub const ORIGINAL_FLATPAK_SOURCE_PATH: &str = "/run/tilo/migration/flatpak-source";
const INVENTORY_FD: RawFd = 1088;
const FIRST_SOURCE_FD: RawFd = 1089;
// Matches canonical bounded HandoffInventory; only its FD crosses the request.
const MAX_BYTES: u64 = 8 * 1024 * 1024;
const SEALS: i32 = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalFlatpakProjection {
    pub index: u32,
    pub device: u64,
    pub inode: u64,
    pub owner: u32,
}
/// These facts must be included in the authenticated OriginalReadClaim digest.
/// Root-authored inventory bytes alone are not a source access grant.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalFlatpakBinding {
    pub read_claim_digest: [u8; 32],
    pub inventory_sha256: [u8; 32],
    pub inventory_len: u64,
    pub original_uid: u32,
    pub projections: Vec<OriginalFlatpakProjection>,
}
pub(super) struct FlatpakSources {
    binding: OriginalFlatpakBinding,
    inventory: File,
    inventory_identity: (u64, u64, u32, u64),
    sources: Vec<File>,
    custody: Arc<dyn ServiceAuthority>,
}
fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}
impl FlatpakSources {
    fn check_inventory(&self) -> io::Result<()> {
        let metadata = self.inventory.metadata()?;
        // SAFETY: descriptor is held; F_GET_SEALS observes kernel seal facts.
        let seals = unsafe { libc::fcntl(self.inventory.as_raw_fd(), libc::F_GET_SEALS) };
        if !metadata.is_file()
            || (
                metadata.dev(),
                metadata.ino(),
                metadata.uid(),
                metadata.len(),
            ) != self.inventory_identity
            || seals < 0
            || metadata.uid() != 0
            || metadata.len() != self.binding.inventory_len
            || seals & SEALS != SEALS
        {
            return Err(refused());
        }
        let mut bytes =
            vec![0; usize::try_from(self.binding.inventory_len).map_err(|_| refused())?];
        self.inventory.read_exact_at(&mut bytes, 0)?;
        if <[u8; 32]>::from(Sha256::digest(&bytes)) != self.binding.inventory_sha256 {
            return Err(refused());
        }
        Ok(())
    }
    pub(super) fn check(&self) -> io::Result<()> {
        if unsafe { libc::geteuid() } != 0 {
            return Err(refused());
        }
        self.custody.check()?;
        if !self.custody.network_hosts()?.is_empty() {
            return Err(refused());
        }
        self.check_inventory()?;
        for (file, expected) in self.sources.iter().zip(&self.binding.projections) {
            let metadata = file.metadata()?;
            let flags = unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GETFL) };
            if flags < 0
                || flags & libc::O_ACCMODE != libc::O_RDONLY
                || !metadata.is_dir()
                || (metadata.dev(), metadata.ino(), metadata.uid())
                    != (expected.device, expected.inode, expected.owner)
            {
                return Err(refused());
            }
            let mut facts: libc::statvfs = unsafe { std::mem::zeroed() };
            if unsafe { libc::fstatvfs(file.as_raw_fd(), &raw mut facts) } < 0
                || facts.f_flag & libc::ST_RDONLY == 0
            {
                return Err(refused());
            }
        }
        Ok(())
    }
    pub(super) fn check_inventory_scratch(
        &self,
        scratch: &super::inventory_scratch::Scratch,
    ) -> io::Result<()> {
        for source in &self.sources {
            scratch.refuse_overlap(source)?;
        }
        Ok(())
    }
    pub(super) fn pairs(&self) -> Vec<(RawFd, RawFd)> {
        let mut pairs = vec![(self.inventory.as_raw_fd(), INVENTORY_FD)];
        pairs.extend(
            self.sources
                .iter()
                .enumerate()
                .map(|(index, file)| (file.as_raw_fd(), FIRST_SOURCE_FD + index as RawFd)),
        );
        pairs
    }
    pub(super) fn splice(&self, argv: &mut Vec<String>, ro_paths: &mut Vec<String>) {
        let mut mounts = vec![
            "--dir".into(),
            ORIGINAL_FLATPAK_SOURCE_PATH.into(),
            "--ro-bind-fd".into(),
            INVENTORY_FD.to_string(),
            ORIGINAL_FLATPAK_INVENTORY_PATH.into(),
        ];
        for index in 0..self.sources.len() {
            mounts.extend([
                "--ro-bind-fd".into(),
                (FIRST_SOURCE_FD + index as RawFd).to_string(),
                format!("{ORIGINAL_FLATPAK_SOURCE_PATH}/{index}"),
            ]);
        }
        argv.splice(0..0, mounts);
        ro_paths.extend([
            ORIGINAL_FLATPAK_INVENTORY_PATH.into(),
            ORIGINAL_FLATPAK_SOURCE_PATH.into(),
        ]);
    }
}
impl OriginalFlatpakBinding {
    fn validate(&self, uid: u32, digest: [u8; 32], sources: usize) -> io::Result<()> {
        if self.read_claim_digest != digest
            || digest == [0; 32]
            || self.inventory_sha256 == [0; 32]
            || self.original_uid != uid
            || uid == 0
            || uid == u32::MAX
            || self.inventory_len == 0
            || self.inventory_len > MAX_BYTES
            || self.projections.is_empty()
            || self.projections.len() > 64
            || self.projections.len() != sources
            || self.projections.iter().enumerate().any(|(index, p)| {
                usize::try_from(p.index).ok() != Some(index) || p.inode == 0 || p.owner == u32::MAX
            })
        {
            return Err(refused());
        }
        Ok(())
    }
}
impl CaptureAdmission {
    /// Attach only after the exact sealed inventory facts enter this admission's
    /// root-authenticated read claim. The caller retains source guards through
    /// child quiescence; original credentials are changed by the existing shim.
    pub fn with_original_flatpak_sources(
        mut self,
        binding: OriginalFlatpakBinding,
        inventory: File,
        sources: Vec<File>,
        custody: Arc<dyn ServiceAuthority>,
    ) -> io::Result<Self> {
        self.check()?;
        if self.flatpak.is_some() || self.original_bootstrap.is_none() {
            return Err(refused());
        }
        binding.validate(
            self.original_uid,
            self.original_read_claim_digest().ok_or_else(refused)?,
            sources.len(),
        )?;
        self.admit_original_ostree_helper()?;
        let facts = inventory.metadata()?;
        let flatpak = FlatpakSources {
            binding,
            inventory_identity: (facts.dev(), facts.ino(), facts.uid(), facts.len()),
            inventory,
            sources,
            custody,
        };
        flatpak.check()?;
        self.flatpak = Some(flatpak);
        self.check()?;
        Ok(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn binding() -> OriginalFlatpakBinding {
        OriginalFlatpakBinding {
            read_claim_digest: [7; 32],
            inventory_sha256: [9; 32],
            inventory_len: 16,
            original_uid: 1000,
            projections: vec![OriginalFlatpakProjection {
                index: 0,
                device: 1,
                inode: 2,
                owner: 0,
            }],
        }
    }
    #[test]
    fn authenticated_binding_codec_refuses_paths_substitution_and_extra_roots() {
        let mut declared = serde_json::to_value(binding()).unwrap();
        declared["source_path"] = serde_json::json!("/var/lib/flatpak");
        assert!(serde_json::from_value::<OriginalFlatpakBinding>(declared).is_err());
        let b = binding();
        b.validate(1000, [7; 32], 1).unwrap();
        assert!(b.validate(1001, [7; 32], 1).is_err());
        assert!(b.validate(1000, [8; 32], 1).is_err());
        assert!(b.validate(1000, [7; 32], 2).is_err());
        let mut changed = b.clone();
        changed.projections[0].index = 1;
        assert!(changed.validate(1000, [7; 32], 1).is_err());
        changed = b.clone();
        changed.inventory_len = MAX_BYTES + 1;
        assert!(changed.validate(1000, [7; 32], 1).is_err());
        changed = b;
        changed.projections = vec![changed.projections[0].clone(); 65];
        assert!(changed.validate(1000, [7; 32], 65).is_err());
    }
}
