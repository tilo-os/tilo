//! Protected mounted-entry facts; ownership is distinct from reader credentials.
use super::*;
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EntryBinding {
    pub(super) home: Home,
    pub(super) kind: EntryKind,
    pub(super) group: u32,
    pub(super) mode: u32,
}
impl EntryBinding {
    pub fn validate(&self) -> io::Result<()> {
        self.home.validate_identity()?;
        let path = self.home.relative.decode().map_err(refusal)?;
        // A separately approved directory can be the top-level filesystem.
        // A regular file always has an actual nonempty relative locator.
        if path.as_os_str().is_empty() {
            if self.kind != EntryKind::Directory {
                return Err(refusal("Mounted file locator is empty"));
            }
        } else {
            relative(&path)?;
            use std::os::unix::ffi::OsStrExt;
            if path
                .as_os_str()
                .as_bytes()
                .split(|byte| *byte == b'/')
                .any(|part| part.is_empty() || part == b"." || part == b".." || part.contains(&0))
            {
                return Err(refusal("Mounted entry locator is not normalized"));
            }
        }
        let expected = match self.kind {
            EntryKind::Directory => libc::S_IFDIR,
            EntryKind::File => libc::S_IFREG,
        };
        if self.mode & libc::S_IFMT != expected
            || self.group == u32::MAX
            || self.home.original_uid == u32::MAX
        {
            return Err(refusal("Mounted entry kind or ownership is invalid"));
        }
        Ok(())
    }
    pub fn volume(&self) -> &SourceVolume {
        &self.home.volume
    }
    pub fn owner(&self) -> u32 {
        self.home.original_uid
    }
    pub fn kind(&self) -> EntryKind {
        self.kind
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn mounted_entry_shape_allows_only_directory_filesystem_root_without_changing_home_contract() {
        let mut plan = super::super::tests::sample();
        plan.home.relative = NativePath::encode(Path::new(""));
        assert!(plan.validate().is_err());
        let mut binding = EntryBinding {
            home: plan.home,
            kind: EntryKind::Directory,
            group: 1002,
            mode: libc::S_IFDIR | 0o750,
        };
        binding.validate().unwrap();
        binding.kind = EntryKind::File;
        binding.mode = libc::S_IFREG | 0o640;
        assert!(binding.validate().is_err());
        binding.home.relative = NativePath::encode(Path::new("data"));
        binding.validate().unwrap();
        for bad in [
            "/data",
            "../data",
            "data/../file",
            "data//file",
            "data/./file",
        ] {
            binding.home.relative = NativePath::encode(Path::new(bad));
            assert!(binding.validate().is_err(), "{bad}");
        }
        binding.home.relative = NativePath::encode(Path::new("data"));
        binding.home.volume.partition_guid = "caller-selected-device".into();
        assert!(binding.validate().is_err());
    }
}
