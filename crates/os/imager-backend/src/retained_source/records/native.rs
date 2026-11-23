//! Immutable transport records, never source admission or person proof.
//! Callers independently retain and revalidate their live source authority.
use std::{
    fs::File,
    io::{self, Seek, SeekFrom, Write},
    os::{
        fd::{AsRawFd, FromRawFd},
        unix::fs::MetadataExt,
    },
};

use super::HandoffRole;

fn refused() -> io::Error {
    io::ErrorKind::PermissionDenied.into()
}

/// Descriptive identity of the exact held file, not a grant of source access.
pub(super) fn file_identity(file: &File) -> io::Result<[u64; 2]> {
    let metadata = file.metadata()?;
    Ok([metadata.dev(), metadata.ino()])
}

/// Seal an already admitted record without granting any authority to its bytes.
pub(super) fn seal(bytes: &[u8], role: HandoffRole) -> io::Result<File> {
    if rustix::process::getuid().as_raw() != 0
        || rustix::process::geteuid().as_raw() != 0
        || bytes.is_empty()
        || bytes.len() > 4096
    {
        return Err(refused());
    }
    let raw = unsafe {
        libc::memfd_create(
            match role {
                HandoffRole::Pending => c"tilo-native-pending-handoff".as_ptr(),
                HandoffRole::Read => c"tilo-native-read-handoff".as_ptr(),
            },
            libc::MFD_CLOEXEC | libc::MFD_ALLOW_SEALING,
        )
    };
    if raw < 0 {
        return Err(io::Error::last_os_error());
    }
    let mut file = unsafe { File::from_raw_fd(raw) };
    file.write_all(bytes)?;
    let seals = libc::F_SEAL_WRITE | libc::F_SEAL_GROW | libc::F_SEAL_SHRINK | libc::F_SEAL_SEAL;
    if unsafe { libc::fchmod(file.as_raw_fd(), 0o400) } != 0
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_ADD_SEALS, seals) } != 0
    {
        return Err(io::Error::last_os_error());
    }
    let m = file.metadata()?;
    if !m.is_file()
        || m.uid() != 0
        || m.nlink() != 0
        || m.mode() & 0o7777 != 0o400
        || unsafe { libc::fcntl(file.as_raw_fd(), libc::F_GET_SEALS) } != seals
    {
        return Err(refused());
    }
    file.seek(SeekFrom::Start(0))?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Read;

    #[test]
    fn sealed_records_are_bounded_immutable_and_rewound_or_refuse_unprivileged_callers() {
        assert!(seal(&[], HandoffRole::Read).is_err());
        assert!(seal(&vec![0; 4097], HandoffRole::Pending).is_err());
        for role in [HandoffRole::Pending, HandoffRole::Read] {
            let record = seal(b"held record", role);
            if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
                assert_eq!(record.unwrap_err().kind(), io::ErrorKind::PermissionDenied);
                continue;
            }
            let mut record = record.unwrap();
            let mut bytes = Vec::new();
            record.read_to_end(&mut bytes).unwrap();
            assert_eq!(bytes, b"held record");
            assert!(record.write_all(b"changed").is_err());
            assert!(record.set_len(0).is_err());
            assert!(record.set_len(100).is_err());
            assert_eq!(
                file_identity(&record).unwrap(),
                file_identity(&record.try_clone().unwrap()).unwrap()
            );
            assert_ne!(
                unsafe { libc::fcntl(record.as_raw_fd(), libc::F_GETFD) } & libc::FD_CLOEXEC,
                0
            );
        }
    }
}
