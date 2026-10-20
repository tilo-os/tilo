//! Native ciphertext authority. Neither the held file nor its constructor is
//! exposed outside this module; receipt metadata cannot manufacture custody.
use super::{
    AtomicBool, CiphertextInfo, DeliveryProgress, MAX_PLAINTEXT, Observer, PreparedMediaHandle,
    check_cancel, io,
};
// The prepared medium carries the portable migration framing; its purpose is
// authenticated, not inferred from the filename.
use crate::setup::portable_transfer::{Purpose, codec};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Component, Path},
};

pub(super) struct Custody {
    file: File,
}

fn changed(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

const SEALED_BOUND: u64 = MAX_PLAINTEXT + 2048 + MAX_PLAINTEXT.div_ceil(codec::CHUNK as u64) * 256;

/// Walk from the filesystem root without following any symlink. Nonblocking
/// opens ensure a substituted FIFO cannot stall admission before the type check.
fn open(package: &Path) -> io::Result<File> {
    use cap_fs_ext::OpenOptionsFollowExt;
    use cap_primitives::fs::{FollowSymlinks, OpenOptions};
    if !package.is_absolute() || package.file_name().is_none() {
        return Err(changed("Prepared package needs an absolute file path"));
    }
    let mut parent = File::options()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open("/")?;
    let components: Vec<_> = package.components().collect();
    for (index, part) in components.iter().enumerate() {
        let name = match part {
            Component::RootDir => continue,
            Component::Normal(name) => Path::new(name),
            _ => return Err(changed("Prepared package path is not canonical")),
        };
        if index.checked_add(1) == Some(components.len()) {
            use cap_fs_ext::OpenOptionsSyncExt;
            let mut options = OpenOptions::new();
            options.read(true).follow(FollowSymlinks::No).nonblock(true);
            return cap_primitives::fs::open(&parent, name, &options);
        }
        parent = cap_primitives::fs::open_dir_nofollow(&parent, name)?;
    }
    Err(changed("Prepared package name missing"))
}

pub(super) fn review(
    package: &Path,
    password: &[u8],
    cancel: &AtomicBool,
) -> io::Result<PreparedMediaHandle> {
    check_cancel(cancel)?;
    let mut file = open(package)?;
    let facts = file.metadata()?;
    if !facts.is_file() || facts.len() == 0 || facts.len() > SEALED_BOUND {
        return Err(changed(
            "Prepared ciphertext exceeds review bounds or is not a regular file",
        ));
    }
    let before = hash(&mut file, cancel)?;
    {
        let mut opener = codec::Opener::new(&mut file, password, MAX_PLAINTEXT, cancel)?;
        if opener.purpose != Purpose::PreparedSetup {
            return Err(changed(
                "Migration package purpose does not match prepared setup",
            ));
        }
        // Consume every chunk, the authenticated footer and the final EOF. No
        // plaintext staging, archive extraction or imported identity is involved.
        io::copy(&mut opener, &mut io::sink())?;
        let footer = opener
            .footer
            .ok_or_else(|| changed("Prepared ciphertext footer missing"))?;
        if footer
            .logical_bytes
            .is_some_and(|bytes| bytes > i64::MAX as u64)
            || footer
                .setup_binding
                .as_ref()
                .is_some_and(|binding| binding.is_empty() || binding.len() > 256)
        {
            return Err(changed("Prepared setup footer exceeds native bounds"));
        }
        if footer.runtime_sockets > 2048 {
            return Err(changed(
                "Prepared setup socket disclosure exceeds review bounds",
            ));
        }
    }
    let after = hash(&mut file, cancel)?;
    if before != after || file.metadata()?.len() != facts.len() {
        return Err(changed("Prepared ciphertext changed during authentication"));
    }
    Ok(PreparedMediaHandle {
        info: CiphertextInfo {
            sealed_bytes: facts.len(),
            ciphertext_sha256: after,
        },
        custody: Custody { file },
    })
}

impl Custody {
    pub(super) fn verify(&mut self, info: &CiphertextInfo, cancel: &AtomicBool) -> io::Result<()> {
        if self.file.metadata()?.len() != info.sealed_bytes
            || hash(&mut self.file, cancel)? != info.ciphertext_sha256
        {
            return Err(changed("Reviewed prepared ciphertext changed"));
        }
        Ok(())
    }

    pub(super) fn copy(
        &mut self,
        info: &CiphertextInfo,
        output: &mut File,
        cancel: &AtomicBool,
        observer: &mut Observer<'_>,
    ) -> io::Result<[u8; 32]> {
        let source = self.file.metadata()?;
        let destination = output.metadata()?;
        if (source.dev(), source.ino()) == (destination.dev(), destination.ino()) {
            return Err(changed("Ciphertext delivery must not overwrite its source"));
        }
        self.file.seek(SeekFrom::Start(0))?;
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 65_536];
        let mut completed = 0u64;
        loop {
            check_cancel(cancel)?;
            let count = self.file.read(&mut buffer)?;
            if count == 0 {
                break;
            }
            completed = completed
                .checked_add(count as u64)
                .filter(|bytes| *bytes <= info.sealed_bytes)
                .ok_or_else(|| changed("Prepared ciphertext grew during delivery"))?;
            let chunk = buffer
                .get(..count)
                .ok_or_else(|| changed("Ciphertext read exceeds buffer"))?;
            output.write_all(chunk)?;
            digest.update(chunk);
            observer(DeliveryProgress {
                completed,
                total: Some(info.sealed_bytes),
            })?;
        }
        check_cancel(cancel)?;
        if completed != info.sealed_bytes {
            return Err(changed("Prepared ciphertext was truncated during delivery"));
        }
        output.sync_all()?;
        Ok(digest.finalize().into())
    }
}

fn hash(file: &mut File, cancel: &AtomicBool) -> io::Result<[u8; 32]> {
    let size = file.metadata()?.len();
    if size > SEALED_BOUND {
        return Err(changed("Prepared ciphertext exceeds review bounds"));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut digest = Sha256::new();
    let mut buffer = vec![0; 65_536];
    let mut completed = 0u64;
    loop {
        check_cancel(cancel)?;
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        completed = completed
            .checked_add(u64::try_from(count).map_err(io::Error::other)?)
            .ok_or_else(|| changed("Ciphertext length overflows"))?;
        if completed > size {
            return Err(changed("Prepared ciphertext grew while reading"));
        }
        digest.update(
            buffer
                .get(..count)
                .ok_or_else(|| changed("Ciphertext read exceeds buffer"))?,
        );
    }
    check_cancel(cancel)?;
    if completed != size {
        return Err(changed("Prepared ciphertext was truncated while reading"));
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(digest.finalize().into())
}
