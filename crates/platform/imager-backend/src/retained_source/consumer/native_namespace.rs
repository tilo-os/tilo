//! One private mount namespace owned by the fixed retained consumer helper.
//! No namespace descriptor or numeric credentials can construct this owner.
use std::{
    fs::File,
    io::{self, Read},
    os::unix::fs::MetadataExt,
};

const TABLE_LIMIT: usize = 4 * 1024 * 1024;

pub struct ManagedConsumerNamespace {
    origin: File,
    current: File,
}
impl ManagedConsumerNamespace {
    pub fn isolate() -> io::Result<Self> {
        launcher::retained_view::RootConsumerChannel::verify_current_helper()?;
        if rustix::process::getuid().as_raw() != 0 || rustix::process::geteuid().as_raw() != 0 {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let origin = File::open("/proc/thread-self/ns/mnt")?;
        // This changes only this already owned helper's namespace. Inherited
        // host mounts are made private before any new staging mount exists.
        if unsafe { libc::unshare(libc::CLONE_NEWNS) } != 0 {
            return Err(io::Error::last_os_error());
        }
        if unsafe {
            libc::mount(
                std::ptr::null(),
                c"/".as_ptr(),
                std::ptr::null(),
                libc::MS_REC | libc::MS_PRIVATE,
                std::ptr::null(),
            )
        } != 0
        {
            return Err(io::Error::last_os_error());
        }
        let current = File::open("/proc/thread-self/ns/mnt")?;
        let owner = Self { origin, current };
        owner.check()?;
        Ok(owner)
    }
    pub fn check(&self) -> io::Result<()> {
        launcher::retained_view::RootConsumerChannel::verify_current_helper()?;
        let origin = self.origin.metadata()?;
        let expected = self.current.metadata()?;
        let actual = File::open("/proc/thread-self/ns/mnt")?.metadata()?;
        if (origin.dev(), origin.ino()) == (expected.dev(), expected.ino())
            || (actual.dev(), actual.ino()) != (expected.dev(), expected.ino())
        {
            return Err(io::ErrorKind::PermissionDenied.into());
        }
        let mut bytes = Vec::new();
        File::open("/proc/thread-self/mountinfo")?
            .take((TABLE_LIMIT + 1) as u64)
            .read_to_end(&mut bytes)?;
        private_mount_table(&bytes)
    }
}

// Mount paths are arbitrary Unix bytes. Only the kernel numeric fields and
// propagation tags participate; unrelated path encoding creates no authority.
fn private_mount_table(bytes: &[u8]) -> io::Result<()> {
    let invalid = || io::Error::from(io::ErrorKind::InvalidData);
    if bytes.is_empty() || bytes.len() > TABLE_LIMIT || !bytes.ends_with(b"\n") {
        return Err(invalid());
    }
    let table = bytes.strip_suffix(b"\n").ok_or_else(invalid)?;
    for row in table.split(|byte| *byte == b'\n') {
        let fields: Vec<_> = row.split(|byte| *byte == b' ').collect();
        let separator = fields
            .iter()
            .position(|field| *field == b"-")
            .ok_or_else(invalid)?;
        let [mount_id, parent_id, ..] = fields.as_slice() else {
            return Err(invalid());
        };
        let optional = fields.get(6..separator).ok_or_else(invalid)?;
        if fields.len().saturating_sub(separator) < 4
            || !mount_id.iter().all(u8::is_ascii_digit)
            || mount_id.is_empty()
            || !parent_id.iter().all(u8::is_ascii_digit)
            || parent_id.is_empty()
            || fields.iter().any(|field| field.is_empty())
            || optional.iter().any(|field| {
                field.starts_with(b"shared:")
                    || field.starts_with(b"master:")
                    || field.starts_with(b"propagate_from:")
            })
        {
            return Err(invalid());
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn namespace_factory_refuses_unclassified_executable_before_isolation() {
        let before = File::open("/proc/thread-self/ns/mnt")
            .unwrap()
            .metadata()
            .unwrap();
        let expected = launcher::retained_view::RootConsumerChannel::verify_current_helper()
            .expect_err("the test executable is not the canonical consuming helper");
        let error = ManagedConsumerNamespace::isolate()
            .err()
            .expect("isolation must preserve canonical helper refusal");
        assert_eq!(error.kind(), expected.kind());
        assert_eq!(error.to_string(), expected.to_string());
        let after = File::open("/proc/thread-self/ns/mnt")
            .unwrap()
            .metadata()
            .unwrap();
        assert_eq!((before.dev(), before.ino()), (after.dev(), after.ino()));
    }

    #[test]
    fn actual_mount_table_parser_preserves_non_utf8_paths_and_refuses_propagation() {
        let private = b"10 1 0:1 / /raw-\xff rw - tmpfs tmpfs rw\n";
        private_mount_table(private).unwrap();
        assert!(private_mount_table(b"10 1 0:1 / /run rw shared:3 - tmpfs tmpfs rw\n").is_err());
        assert!(private_mount_table(b"10 1 0:1 / /run rw master:3 - tmpfs tmpfs rw\n").is_err());
        assert!(private_mount_table(b"x 1 0:1 / /run rw - tmpfs tmpfs rw\n").is_err());
        assert!(private_mount_table(&vec![b'x'; TABLE_LIMIT + 1]).is_err());
        assert!(private_mount_table(&private[..private.len() - 1]).is_err());
    }
}
