//! Descriptor-relative, bounded collection only after the exact service lease
//! has a namespace-exit witness. The collected bytes never enter an app loan.
use super::{
    AsRawFd, CString, File, FromRawFd, MetadataExt, OsStr, RamDirectory, SERVICE_PACKAGE,
    component, fd_path, io, open_directory, ram_backing,
};
use std::{collections::BTreeMap, io::Read};
use zeroize::Zeroizing;

pub(super) type Files = BTreeMap<String, Zeroizing<Vec<u8>>>;
const MAX_BYTES: usize = 64 * 1024 * 1024;
const MAX_ENTRIES: usize = 8192;
struct Collected {
    files: Files,
    bytes: usize,
    entries: usize,
}
impl RamDirectory {
    pub(super) fn collect_profile(&self) -> io::Result<Files> {
        ram_backing(&self.root)?;
        let mut directory = self.root.try_clone()?;
        for part in [
            launcher::appdir::ROOMS_DIR,
            launcher::appdir::PERSONAL_ROOM,
            SERVICE_PACKAGE,
        ] {
            directory = open_directory(&directory, &component(OsStr::new(part))?)?;
        }
        let mut collected = Collected {
            files: BTreeMap::new(),
            bytes: 0,
            entries: 0,
        };
        collected.directory(&directory, "", 0)?;
        Ok(collected.files)
    }
}
impl Collected {
    fn directory(&mut self, directory: &File, prefix: &str, depth: usize) -> io::Result<()> {
        if depth >= 32 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        for entry in std::fs::read_dir(fd_path(directory))? {
            self.entries = self
                .entries
                .checked_add(1)
                .ok_or(io::ErrorKind::InvalidData)?;
            if self.entries > MAX_ENTRIES {
                return Err(io::ErrorKind::InvalidData.into());
            }
            let entry = entry?;
            let name = component(&entry.file_name())?;
            let text = entry
                .file_name()
                .into_string()
                .map_err(|_| io::ErrorKind::InvalidData)?;
            let path = if prefix.is_empty() {
                text
            } else {
                format!("{prefix}/{text}")
            };
            let kind = entry.file_type()?;
            if kind.is_dir() {
                self.directory(
                    &open_directory(directory, &name)?,
                    &path,
                    depth.saturating_add(1),
                )?;
            } else if kind.is_file() {
                self.file(directory, &name, path)?;
            } else {
                // The exact IPC endpoint is ephemeral, never crypto state.
                use std::os::unix::fs::FileTypeExt as _;
                if !(prefix.is_empty() && path == "provider.sock" && kind.is_socket()) {
                    return Err(io::ErrorKind::PermissionDenied.into());
                }
            }
        }
        Ok(())
    }
    fn file(&mut self, directory: &File, name: &CString, path: String) -> io::Result<()> {
        if self.files.len() >= 4096 {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let fd = unsafe {
            libc::openat(
                directory.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_CLOEXEC | libc::O_NONBLOCK,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let file = unsafe { File::from_raw_fd(fd) };
        let metadata = file.metadata()?;
        if !metadata.is_file()
            || metadata.nlink() != 1
            || metadata.uid() != unsafe { libc::geteuid() }
            || !private_fs::is_owner_only(&metadata)
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let available = MAX_BYTES
            .checked_sub(self.bytes)
            .ok_or(io::ErrorKind::InvalidData)?;
        if metadata.len() > u64::try_from(available).map_err(io::Error::other)? {
            return Err(io::ErrorKind::InvalidData.into());
        }
        let mut bytes = Zeroizing::new(Vec::new());
        file.take(
            u64::try_from(available)
                .map_err(io::Error::other)?
                .saturating_add(1),
        )
        .read_to_end(&mut bytes)?;
        self.bytes = self
            .bytes
            .checked_add(bytes.len())
            .ok_or(io::ErrorKind::InvalidData)?;
        if self.bytes > MAX_BYTES || self.files.insert(path, bytes).is_some() {
            return Err(io::ErrorKind::InvalidData.into());
        }
        Ok(())
    }
}

#[cfg(test)]
#[cfg(feature = "signal-profile-native-test")]
mod tests {
    use super::*;
    use std::{
        os::unix::{fs::PermissionsExt as _, net::UnixListener},
        path::PathBuf,
    };
    fn fixture() -> (tempfile::TempDir, RamDirectory) {
        let root = tempfile::tempdir_in("/dev/shm").unwrap();
        std::fs::set_permissions(root.path(), std::fs::Permissions::from_mode(0o700)).unwrap();
        let ram = RamDirectory::allocate(root.path()).unwrap();
        (root, ram)
    }
    fn package(ram: &RamDirectory) -> PathBuf {
        ram.source()
            .join(launcher::appdir::ROOMS_DIR)
            .join(launcher::appdir::PERSONAL_ROOM)
            .join(SERVICE_PACKAGE)
    }
    #[test]
    fn collector_observes_latest_files_deletions_and_only_exact_rpc_socket() {
        let (_root, ram) = fixture();
        ram.write_profile("data/state", b"before").unwrap();
        ram.write_profile("deleted", b"old").unwrap();
        let path = package(&ram);
        std::fs::write(path.join("data/state"), b"after").unwrap();
        std::fs::remove_file(path.join("deleted")).unwrap();
        let _socket = UnixListener::bind(path.join("provider.sock")).unwrap();
        let files = ram.collect_profile().unwrap();
        assert_eq!(files.len(), 1);
        assert_eq!(files["data/state"].as_slice(), b"after");
        let _other = UnixListener::bind(path.join("other.sock")).unwrap();
        assert!(ram.collect_profile().is_err());
    }
    #[test]
    fn collector_refuses_links_and_permissive_provider_files() {
        let (_root, ram) = fixture();
        ram.write_profile("state", b"secret").unwrap();
        let path = package(&ram);
        std::os::unix::fs::symlink("state", path.join("linked")).unwrap();
        assert!(ram.collect_profile().is_err());
        std::fs::remove_file(path.join("linked")).unwrap();
        std::fs::hard_link(path.join("state"), path.join("linked")).unwrap();
        assert!(ram.collect_profile().is_err());
        std::fs::remove_file(path.join("linked")).unwrap();
        std::fs::set_permissions(path.join("state"), std::fs::Permissions::from_mode(0o644))
            .unwrap();
        assert!(ram.collect_profile().is_err());
    }
}
