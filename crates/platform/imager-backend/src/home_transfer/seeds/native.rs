//! Linux root-controlled firstboot proof reader.
use super::*;
use crate::migration_durability as durable;
use sha2::{Digest, Sha256};
use std::collections::BTreeSet;
use std::{
    io::Read,
    os::{
        fd::AsRawFd,
        unix::fs::{MetadataExt as _, OpenOptionsExt as _},
    },
    path::PathBuf,
};
const PROOFS: &str = "/var/lib/tilo/firstboot-home-seeds";
const SEED_LIMIT: u64 = 4 * 1024 * 1024;

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn real_proof_reader_refuses_symlinked_authority_namespace() {
        let home = File::open("/").unwrap();
        let error = match read_at(Path::new("/proc/self/status"), &home) {
            Err(error) => error,
            Ok(_) => panic!("a process-local symlink must not supply firstboot authority"),
        };
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "Firstboot seed proof namespace is not root controlled"
        );
    }
}

fn filesystem(home: &File) -> io::Result<u64> {
    let mut result = std::mem::MaybeUninit::<libc::statvfs>::uninit();
    // SAFETY: fstatvfs initializes this output on success; the FD is live.
    if unsafe { libc::fstatvfs(home.as_raw_fd(), result.as_mut_ptr()) } != 0 {
        return Err(io::Error::last_os_error());
    }
    // SAFETY: successful fstatvfs initialized the output.
    Ok(unsafe { result.assume_init() }.f_fsid)
}
pub(super) fn read(home: &File) -> io::Result<Option<Authority>> {
    // SAFETY: geteuid only reads this process's credentials.
    let uid = rustix::process::geteuid().as_raw();
    read_at(&Path::new(PROOFS).join(format!("{uid}.json")), home)
}
fn read_at(path: &Path, home: &File) -> io::Result<Option<Authority>> {
    let mut current = PathBuf::from("/");
    for component in path.components().skip(1) {
        current.push(component);
        let metadata = match std::fs::symlink_metadata(&current) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        if metadata.uid() != 0 || metadata.mode() & 0o022 != 0 || metadata.file_type().is_symlink()
        {
            return Err(changed(
                "Firstboot seed proof namespace is not root controlled",
            ));
        }
    }
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)?;
    let before = file.metadata()?;
    if !before.is_file() || before.len() > SEED_LIMIT || before.nlink() != 1 {
        return Err(changed("Firstboot seed proof exceeds bounds"));
    }
    let mut bytes = Vec::new();
    (&file).take(SEED_LIMIT + 1).read_to_end(&mut bytes)?;
    let after = file.metadata()?;
    if bytes.len() as u64 != before.len() || stamp(&before)? != stamp(&after)? {
        return Err(changed("Firstboot seed proof changed"));
    }
    let proof: Proof = serde_json::from_slice(&bytes)?;
    let metadata = durable::readable_directory(home)?.metadata()?;
    if proof.schema != 1 || proof.files.len() > 4096 {
        return Err(changed("Firstboot seed proof schema exceeds bounds"));
    }
    if proof.uid != metadata.uid()
        || proof.home_inode != metadata.ino()
        || proof.filesystem != filesystem(home)?
    {
        // A Desktop home or test quarantine is not the account home created
        // by this proof. Its existing files stay ordinary safe conflicts.
        return Ok(None);
    }
    let mut paths = BTreeSet::new();
    for item in &proof.files {
        relative(&item.path)?;
        if !paths.insert(&item.path)
            || item.stamp.uid != proof.uid
            || item.stamp.bytes > SEED_LIMIT
            || item.stamp.mode > 0o777
            || item.sha256.len() != 64
            || !item
                .sha256
                .bytes()
                .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
        {
            return Err(changed("Firstboot seed proof contains invalid items"));
        }
    }
    Ok(Some(Authority {
        proof,
        digest: hex::encode(Sha256::digest(bytes)),
    }))
}

/// Capture the exact native file stamp used by firstboot proof records.
/// # Errors
/// Refuses timestamps outside the protocol's nanosecond range.
pub fn stamp(metadata: &std::fs::Metadata) -> io::Result<Stamp> {
    fn ns(seconds: i64, fraction: i64) -> io::Result<i64> {
        seconds
            .checked_mul(1_000_000_000)
            .and_then(|value| value.checked_add(fraction))
            .ok_or_else(|| changed("Firstboot seed timestamp exceeds bounds"))
    }
    Ok(Stamp {
        inode: metadata.ino(),
        uid: metadata.uid(),
        mode: metadata.mode() & 0o777,
        bytes: metadata.len(),
        mtime_ns: ns(metadata.mtime(), metadata.mtime_nsec())?,
        ctime_ns: ns(metadata.ctime(), metadata.ctime_nsec())?,
    })
}
