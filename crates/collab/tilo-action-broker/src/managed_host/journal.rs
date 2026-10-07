//! Private one-use launch barriers. Disk records confer no live process authority.
use advisory_lock::FileLock;
use std::{
    fs::{File, OpenOptions},
    io::Write,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
};

const MAX_EVENT_BYTES: usize = 16 * 1024;
const MAX_CLAIMS: usize = 1024;

pub(super) struct Journal {
    root: PathBuf,
    // Exactly one host controller owns this directory for its entire lifetime.
    _owner: FileLock,
}
impl Journal {
    pub(super) fn open(root: &Path) -> Result<Self, String> {
        if !root.is_absolute() {
            return Err("managed journal requires an absolute private host directory".into());
        }
        let metadata = std::fs::symlink_metadata(root).map_err(|error| error.to_string())?;
        if !metadata.is_dir()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
        {
            return Err("managed journal is not a private directory owned by this host".into());
        }
        let root = root.canonicalize().map_err(|error| error.to_string())?;
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(root.join("owner.lock"))
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file()
            || metadata.mode() & 0o077 != 0
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.nlink() != 1
        {
            return Err("managed journal owner lock is not private".into());
        }
        let owner = FileLock::try_exclusive(file).map_err(|error| error.to_string())?;
        Ok(Self {
            root,
            _owner: owner,
        })
    }

    pub(super) fn claim(&self, slot: &str, bytes: &[u8]) -> Result<(), String> {
        // A preexisting or partial barrier is an uncertain/previous attempt, never a retry.
        let mut claims: usize = 0;
        let mut entries: usize = 0;
        for entry in std::fs::read_dir(&self.root)
            .map_err(|error| error.to_string())?
            .take(MAX_CLAIMS * 3 + 2)
        {
            entries = entries
                .checked_add(1)
                .ok_or("journal entry count exhausted")?;
            if entries > MAX_CLAIMS * 3 + 1 {
                return Err("managed journal entry bound exceeded".into());
            }
            let entry = entry.map_err(|error| error.to_string())?;
            if !entry
                .file_type()
                .map_err(|error| error.to_string())?
                .is_file()
            {
                return Err("managed journal contains an unsafe entry".into());
            }
            if entry.file_name().to_string_lossy().ends_with(".claim.json") {
                claims = claims
                    .checked_add(1)
                    .ok_or("journal claim count exhausted")?;
            }
        }
        if claims >= MAX_CLAIMS {
            return Err("managed launch journal is full; reconcile retained claims".into());
        }
        self.write_once(slot, "claim", bytes)
    }

    pub(super) fn process(&self, slot: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_once(slot, "process", bytes)
    }

    pub(super) fn terminal(&self, slot: &str, bytes: &[u8]) -> Result<(), String> {
        self.write_once(slot, "terminal", bytes)
    }

    fn write_once(&self, slot: &str, kind: &str, bytes: &[u8]) -> Result<(), String> {
        if slot.len() != 64 || !slot.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid private launch slot".into());
        }
        if bytes.is_empty() || bytes.len() > MAX_EVENT_BYTES {
            return Err("managed launch evidence exceeds bound".into());
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW)
            .open(self.root.join(format!("{slot}.{kind}.json")))
            .map_err(|error| {
                format!("managed launch barrier unavailable; original attempt retained: {error}")
            })?;
        file.write_all(bytes)
            .and_then(|()| file.sync_all())
            .map_err(|error| format!("managed launch barrier uncertain: {error}"))?;
        File::open(&self.root)
            .and_then(|directory| directory.sync_all())
            .map_err(|error| format!("managed launch directory barrier uncertain: {error}"))
    }
}
