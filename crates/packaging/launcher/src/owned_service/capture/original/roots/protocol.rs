//! Closed source identity table: metadata alone never supplies mount authority.
use super::*;
use sha2::{Digest, Sha256};
const FD: RawFd = 51;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OriginalMountedIdentity {
    pub placement: Vec<u8>,
    pub identity: (u64, u64, u32, u32, u32),
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct OriginalMountedBinding {
    pub read_claim_digest: [u8; 32],
    pub table_sha256: [u8; 32],
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Receipt {
    schema: u32,
    read_claim_digest: [u8; 32],
    table_sha256: [u8; 32],
    roots: Vec<Root>,
}
#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Root {
    source: OriginalMountedIdentity,
    mount: u64,
}
/// Root-authenticated native projection, constructed only by reading the exact
/// sealed root-authored shim receipt and checking every actual confined entry.
pub struct VerifiedOriginalMountedRoots {
    roots: Vec<Root>,
    files: Vec<File>,
}
/// # Errors
/// Rejects unbounded, overlapping, noncanonical or unsupported root identities.
pub fn original_mounted_table_sha256(roots: &[OriginalMountedIdentity]) -> io::Result<[u8; 32]> {
    if roots.is_empty() || roots.len() > LIMIT {
        return Err(refused("Original mounted table count differs"));
    }
    let mut previous: Option<&Path> = None;
    let mut hash = Sha256::new();
    hash.update(b"tilo.original-mounted-identities.v1\0");
    hash.update((roots.len() as u64).to_le_bytes());
    for root in roots {
        placement_shape(&root.placement)?;
        let path = Path::new(std::ffi::OsStr::from_bytes(&root.placement));
        if previous.is_some_and(|old| old >= path || path.starts_with(old))
            || !matches!(
                root.identity.4 & libc::S_IFMT,
                libc::S_IFDIR | libc::S_IFREG
            )
        {
            return Err(refused("Original mounted table placement or kind differs"));
        }
        previous = Some(path);
        hash.update((root.placement.len() as u64).to_le_bytes());
        hash.update(&root.placement);
        hash.update(root.identity.0.to_le_bytes());
        hash.update(root.identity.1.to_le_bytes());
        for field in [root.identity.2, root.identity.3, root.identity.4] {
            hash.update(field.to_le_bytes());
        }
    }
    Ok(hash.finalize().into())
}
pub(super) fn identities(projections: &[Projection]) -> Vec<OriginalMountedIdentity> {
    projections
        .iter()
        .map(|p| OriginalMountedIdentity {
            placement: p.placement.clone(),
            identity: p.identity,
        })
        .collect()
}
pub(in crate::owned_service::capture::original) fn observe(
    facts: &Facts,
) -> io::Result<Option<Observed>> {
    let Some(expected) = facts.mounted_table_sha256 else {
        return Ok(None);
    };
    if original_mounted_table_sha256(&identities(&facts.roots))? != expected {
        return Err(refused("Original mounted bootstrap table differs"));
    }
    let mut roots = Vec::with_capacity(facts.roots.len());
    for projection in &facts.roots {
        let held = open_target(&projection.placement, true)?;
        readonly(&held)?;
        if identity(&held)? != projection.identity {
            return Err(refused("Original mounted final projection differs"));
        }
        roots.push(Root {
            source: OriginalMountedIdentity {
                placement: projection.placement.clone(),
                identity: projection.identity,
            },
            mount: super::super::super::inventory_scratch::identity(&held)?.2,
        });
    }
    let receipt = Receipt {
        schema: 1,
        read_claim_digest: facts.read_claim_digest,
        table_sha256: expected,
        roots,
    };
    Ok(Some(Observed { receipt }))
}

/// Native observations minted only under the original caller filesystem scope.
/// No request/JSON constructor can manufacture the root publication authority.
pub(in crate::owned_service::capture::original) struct Observed {
    receipt: Receipt,
}
pub(in crate::owned_service::capture::original) fn publish(
    observed: Option<Observed>,
) -> io::Result<()> {
    root()?;
    if unsafe { libc::setfsuid(u32::MAX) } != 0 {
        return Err(refused(
            "Original mounted receipt requires restored root filesystem identity",
        ));
    }
    let Some(observed) = observed else {
        return Ok(());
    };
    let bytes = serde_json::to_vec(&observed.receipt)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(refused("Original mounted receipt exceeds bound"));
    }
    let raw = unsafe {
        libc::memfd_create(
            c"tilo-original-mounted-receipt".as_ptr(),
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut token = unsafe { File::from_raw_fd(raw) };
    token.write_all(&bytes)?;
    token.rewind()?;
    if unsafe { libc::fcntl(raw, libc::F_ADD_SEALS, SEALS) } < 0 {
        return Err(io::Error::last_os_error());
    }
    if raw == FD {
        if unsafe { libc::fcntl(raw, libc::F_SETFD, 0) } < 0 {
            return Err(io::Error::last_os_error());
        }
        std::mem::forget(token);
    } else if unsafe { libc::dup3(raw, FD, 0) } < 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}
/// Signed original worker only; named/unsealed files and ordinary request JSON
/// cannot mint this type. The caller must already verify its canonical FD50 claim.
/// # Errors
/// Refuses missing/unsealed receipts, a changed claim, or any changed projection.
pub fn verify_original_mounted_roots(
    binding: Option<&OriginalMountedBinding>,
) -> io::Result<Option<VerifiedOriginalMountedRoots>> {
    let Some(binding) = binding else {
        if unsafe { libc::fcntl(FD, libc::F_GETFD) } >= 0 {
            return Err(refused("Unexpected original mounted receipt"));
        }
        return Ok(None);
    };
    super::super::verify_original_read_claim(binding.read_claim_digest)?;
    let raw = unsafe { libc::fcntl(FD, libc::F_DUPFD_CLOEXEC, 128) };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let token = unsafe { File::from_raw_fd(raw) };
    super::super::check_token(&token)?;
    use std::os::unix::fs::FileExt;
    let len = usize::try_from(token.metadata()?.len()).map_err(io::Error::other)?;
    let mut bytes = vec![0; len];
    token.read_exact_at(&mut bytes, 0)?;
    let receipt: Receipt = serde_json::from_slice(&bytes)?;
    if receipt.schema != 1
        || receipt.read_claim_digest != binding.read_claim_digest
        || receipt.table_sha256 != binding.table_sha256
        || original_mounted_table_sha256(
            &receipt
                .roots
                .iter()
                .map(|root| root.source.clone())
                .collect::<Vec<_>>(),
        )? != binding.table_sha256
    {
        return Err(refused(
            "Original mounted receipt differs from admitted claim",
        ));
    }
    let mut files = Vec::with_capacity(receipt.roots.len());
    for root in &receipt.roots {
        let held = open_target(&root.source.placement, true)?;
        readonly(&held)?;
        if identity(&held)? != root.source.identity
            || super::super::super::inventory_scratch::identity(&held)?.2 != root.mount
        {
            return Err(refused("Original worker mounted source changed"));
        }
        files.push(held);
    }
    Ok(Some(VerifiedOriginalMountedRoots {
        roots: receipt.roots,
        files,
    }))
}
impl VerifiedOriginalMountedRoots {
    /// # Errors
    /// Refuses changed path, inode, mount or readonly projection custody.
    pub fn check(&self) -> io::Result<()> {
        for (root, file) in self.roots.iter().zip(&self.files) {
            let current = open_target(&root.source.placement, true)?;
            readonly(&current)?;
            if identity(&current)? != root.source.identity
                || identity(file)? != root.source.identity
                || super::super::super::inventory_scratch::identity(&current)?.2 != root.mount
                || super::super::super::inventory_scratch::identity(file)?.2 != root.mount
            {
                return Err(refused("Verified original projection custody changed"));
            }
        }
        Ok(())
    }
    pub fn scope(&self, path: &Path) -> Option<(u64, u64)> {
        self.roots
            .iter()
            .find(|root| {
                path.starts_with(Path::new(std::ffi::OsStr::from_bytes(
                    &root.source.placement,
                )))
            })
            .map(|root| (root.source.identity.0, root.mount))
    }
}
pub(super) fn reserved(fd: RawFd) -> bool {
    fd == FD
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn original_mounted_table_digest_binds_exact_kind_placement_owner_and_inode() {
        let roots = vec![
            OriginalMountedIdentity {
                placement: b"Documents".to_vec(),
                identity: (10, 11, 1003, 1002, libc::S_IFDIR | 0o750),
            },
            OriginalMountedIdentity {
                placement: b"mounted-file".to_vec(),
                identity: (20, 12, 1004, 1002, libc::S_IFREG | 0o640),
            },
        ];
        let digest = original_mounted_table_sha256(&roots).unwrap();
        for field in 0..6 {
            let mut changed = roots.clone();
            match field {
                0 => changed[0].placement = b"Different".to_vec(),
                1 => changed[0].identity.0 += 1,
                2 => changed[0].identity.1 += 1,
                3 => changed[0].identity.2 += 1,
                4 => changed[0].identity.3 += 1,
                _ => changed[0].identity.4 ^= 0o020,
            }
            assert_ne!(original_mounted_table_sha256(&changed).unwrap(), digest);
        }
        for placement in [
            b"../outside".as_slice(),
            b"/outside",
            b"Documents/child",
            b"Documents",
            b"mounted-file//bad",
        ] {
            let mut changed = roots.clone();
            changed[1].placement = placement.to_vec();
            assert!(original_mounted_table_sha256(&changed).is_err());
        }
        assert!(original_mounted_table_sha256(&[]).is_err());
        let mut invalid = roots;
        invalid[1].identity.4 = libc::S_IFLNK | 0o777;
        assert!(original_mounted_table_sha256(&invalid).is_err());
    }
}
