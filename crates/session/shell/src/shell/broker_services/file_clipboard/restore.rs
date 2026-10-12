//! Restore through current Files authority, never through the app's private XDG namespace.
use super::*;
use serde::{Deserialize, Serialize};
use std::os::unix::fs::OpenOptionsExt as _;

const MAX_INFO_BYTES: u64 = 64 * 1024;

/// Standard metadata retained with the native transfer's durable receipt.
/// Undo can recreate it after returning the payload to its original Trash slot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct Info {
    parent: journal::Folder,
    name: Vec<u8>,
    body: Vec<u8>,
    seal: journal::Seal,
}

fn read_info(
    directory: &SecureNearbyDestination,
    name: &std::ffi::OsStr,
) -> Result<(Vec<u8>, fs::Metadata), String> {
    let mut file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
        .open(directory.anchored_root().join(name))
        .map_err(|error| error.to_string())?;
    let metadata = file.metadata().map_err(|error| error.to_string())?;
    if !metadata.is_file() || metadata.len() > MAX_INFO_BYTES {
        return Err("Trash metadata is not a bounded regular file".into());
    }
    let mut body = Vec::new();
    std::io::Read::by_ref(&mut file)
        .take(MAX_INFO_BYTES + 1)
        .read_to_end(&mut body)
        .map_err(|error| error.to_string())?;
    if body.len() as u64 > MAX_INFO_BYTES {
        return Err("Trash metadata exceeds its byte limit".into());
    }
    if !journal::Seal::of(&metadata).unchanged(&file.metadata().map_err(|error| error.to_string())?)
    {
        return Err("Trash metadata changed while reading".into());
    }
    Ok((body, metadata))
}

fn present(directory: &SecureNearbyDestination, name: &std::ffi::OsStr) -> Result<bool, String> {
    match fs::symlink_metadata(directory.anchored_root().join(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(error.to_string()),
    }
}

fn original_path(body: &[u8]) -> Result<PathBuf, String> {
    let text = std::str::from_utf8(body).map_err(|_| "Trash metadata is not UTF-8")?;
    let mut section = false;
    let mut path = None;
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            section = line == "[Trash Info]";
        } else if section && let Some(value) = line.strip_prefix("Path=") {
            if path.is_some() {
                return Err("Trash metadata has duplicate original paths".into());
            }
            let mut bytes = value.bytes();
            while let Some(byte) = bytes.next() {
                if byte == b'%'
                    && (!bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit())
                        || !bytes.next().is_some_and(|byte| byte.is_ascii_hexdigit()))
                {
                    return Err("Trash original path has malformed percent encoding".into());
                }
            }
            let decoded = percent_encoding::percent_decode_str(value).collect::<Vec<_>>();
            if decoded.len() > 4096 {
                return Err("Trash original path exceeds its byte limit".into());
            }
            if decoded.contains(&0) {
                return Err("Trash original path contains NUL".into());
            }
            path = Some(PathBuf::from(OsString::from_vec(decoded)));
        }
    }
    let path = path.ok_or("Trash original path is absent")?;
    if !path.is_absolute()
        || path.file_name().is_none()
        || path
            .components()
            .any(|part| part == std::path::Component::ParentDir)
    {
        return Err("Trash original path is not a confined absolute file path".into());
    }
    Ok(path)
}

impl Info {
    pub(super) fn read(
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
        source: &Path,
    ) -> Result<(Self, PathBuf), String> {
        context.authorize(Permission::Undoable)?;
        let data = tilo_paths::data_dir().ok_or("host Trash is unavailable")?;
        if source.parent() != Some(data.join("Trash/files").as_path()) {
            return Err("native Restore requires an entry in this host's home Trash".into());
        }
        let parent_path = data.join("Trash/info");
        let grant = Grant::open(
            tree,
            context.workspace(),
            &parent_path,
            true,
            context.owner(),
        )?;
        let directory = SecureNearbyDestination::open_beneath(grant.root(), &parent_path)?;
        let mut name = source
            .file_name()
            .ok_or("Trash entry has no name")?
            .to_os_string();
        name.push(".trashinfo");
        let (body, metadata) = read_info(&directory, &name)?;
        let seal = journal::Seal::of(&metadata);
        let original = original_path(&body)?;
        grant.check(tree, context.workspace(), context.owner())?;
        Ok((
            Self {
                parent: journal::Folder::new(&parent_path, &grant, &directory)?,
                name: name.into_vec(),
                body,
                seal,
            },
            original,
        ))
    }

    pub(super) fn check(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let directory = self.parent.open(tree, context)?;
        let name = std::ffi::OsStr::from_bytes(&self.name);
        // Check ctime and object identity without rereading metadata on every
        // copied chunk. The captured body is checked again before consuming it.
        let file = fs::OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.anchored_root().join(name))
            .map_err(|error| error.to_string())?;
        let metadata = file.metadata().map_err(|error| error.to_string())?;
        if !metadata.is_file() || !self.seal.unchanged(&metadata) {
            return Err("Trash metadata changed before Restore".into());
        }
        Ok(())
    }

    pub(super) fn remove(
        &self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let directory = self.parent.open(tree, context)?;
        let name = std::ffi::OsStr::from_bytes(&self.name);
        let mut held = name.to_os_string();
        held.push(".tilo-restore-held");
        let pinned = if present(&directory, name)? {
            self.check(tree, context)?;
            let file = fs::OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
                .open(directory.anchored_root().join(name))
                .map_err(|error| error.to_string())?;
            if !self
                .seal
                .unchanged(&file.metadata().map_err(|error| error.to_string())?)
            {
                return Err("Trash metadata changed before retention".into());
            }
            rename_entry(&directory, name, &directory, &held)?;
            #[cfg(test)]
            interrupt_test_process("restore-info-retained");
            Some(file)
        } else {
            None
        };
        if !present(&directory, &held)? {
            // The native payload has already been published. An observed
            // absent metadata slot is the completed standard Trash state.
            return Ok(());
        }
        let (body, metadata) = read_info(&directory, &held)?;
        let same = match &pinned {
            Some(file) => journal::Seal::of(&file.metadata().map_err(|error| error.to_string())?)
                .unchanged(&metadata),
            None => self.seal.matches(&metadata),
        };
        if body != self.body || !same {
            if pinned.is_some() {
                let returned = rename_entry(&directory, &held, &directory, name);
                return Err(format!(
                    "Trash metadata changed during Restore; retained metadata return: {returned:?}"
                ));
            }
            return Err("retained Trash metadata no longer matches this receipt".into());
        }
        self.parent.open(tree, context)?;
        fs::remove_file(directory.anchored_root().join(&held))
            .map_err(|error| error.to_string())?;
        File::open(directory.anchored_root())
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())
    }

    pub(super) fn restore(
        &mut self,
        tree: &WorkspaceTreeHandle,
        context: &FileClipboardContext,
    ) -> Result<(), String> {
        let directory = self.parent.open(tree, context)?;
        let name = std::ffi::OsStr::from_bytes(&self.name);
        let mut held = name.to_os_string();
        held.push(".tilo-restore-held");
        if present(&directory, &held)? {
            if present(&directory, name)? {
                return Err("Trash metadata has both an original and retained slot".into());
            }
            let (body, metadata) = read_info(&directory, &held)?;
            if body != self.body || !self.seal.matches(&metadata) {
                return Err("retained Trash metadata no longer matches this receipt".into());
            }
            // A crash after retention must return the same metadata object,
            // not recreate a second slot that blocks the next Restore.
            self.parent.open(tree, context)?;
            rename_entry(&directory, &held, &directory, name)?;
            let (body, metadata) = read_info(&directory, name)?;
            if body != self.body || !self.seal.matches(&metadata) {
                return Err("returned Trash metadata no longer matches this receipt".into());
            }
            self.seal = journal::Seal::of(&metadata);
        }
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(directory.anchored_root().join(name))
        {
            Ok(mut file) => {
                file.write_all(&self.body)
                    .and_then(|()| file.sync_all())
                    .map_err(|error| error.to_string())?;
                self.seal = journal::Seal::of(&file.metadata().map_err(|error| error.to_string())?);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                let (body, metadata) = read_info(&directory, name)?;
                if body != self.body {
                    return Err("Trash metadata slot is occupied by another item".into());
                }
                self.seal = journal::Seal::of(&metadata);
            }
            Err(error) => return Err(error.to_string()),
        }
        self.parent.open(tree, context)?;
        File::open(directory.anchored_root())
            .and_then(|file| file.sync_all())
            .map_err(|error| error.to_string())
    }
}

impl NativeFileClipboard {
    /// Derive the destination from sealed standard Trash metadata, under the
    /// invoking resource action's current authority. This private Cut operation
    /// never replaces the workspace's clipboard offer.
    pub(super) fn restore_trash(
        &self,
        context: &FileClipboardContext,
        selected: &SelectedDescriptor,
        id: String,
    ) -> Result<Reply, String> {
        context.authorize(Permission::Undoable)?;
        let path = PathBuf::from(OsString::from_vec(selected.path.clone()));
        if let Ok(prior) = self.operation(context, &id) {
            let source = prior
                .offer
                .entries
                .first()
                .ok_or("Restore source unavailable")?;
            if prior.restore_info.is_none()
                || source.path != path
                || prior.context.peer() != context.peer()
                || prior.context.window() != context.window()
            {
                return Err("operation id already names another transfer".into());
            }
            verify_selected(context, selected, source.identity)?;
            return Ok(prior.answer());
        }
        let source = Arc::new(Source::open(
            &self.tree,
            context.workspace(),
            path,
            true,
            context.owner(),
        )?);
        verify_selected(context, selected, source.identity)?;
        source.check(&self.tree, context.workspace(), context.owner())?;
        let (info, original) = Info::read(&self.tree, context, &source.path)?;
        let name = original
            .file_name()
            .ok_or("Trash original name is absent")?
            .to_os_string();
        let destination = Arc::new(Destination::open(
            &self.tree,
            context.workspace(),
            original
                .parent()
                .ok_or("Trash original folder is absent")?
                .to_path_buf(),
            context.owner(),
        )?);
        destination.check(&self.tree, context.workspace(), context.owner())?;
        if present(&destination.directory, &name)? {
            return Err("original Restore location is occupied; Trash data is retained".into());
        }
        let now = Instant::now();
        let expires = now.checked_add(OFFER_LIFETIME).unwrap_or(now);
        let offer = Arc::new(Offer {
            public: Offered {
                id: id.clone(),
                count: 1,
                cut: true,
            },
            source: context.clone(),
            expires,
            entries: vec![source],
            consumed: Mutex::new(vec![false]),
            cut_busy: AtomicBool::new(false),
        });
        let custody = Arc::new(Ticket {
            context: context.clone(),
            expires,
            object: SelectedObject::Destination(destination.clone()),
        });
        self.enqueue(&Arc::new(Operation {
            context: context.clone(),
            offer,
            destination,
            destination_ticket: id.clone(),
            destination_custody: custody,
            id,
            cancelled: AtomicBool::new(false),
            records: Mutex::default(),
            reply: Mutex::default(),
            journal: self.journal.clone(),
            receipt: std::sync::OnceLock::new(),
            destination_name: Some(name),
            restore_info: Some(info),
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn trash_original_paths_are_bounded_absolute_and_preserve_encoded_bytes() {
        for path in [
            "relative.txt",
            "/",
            "/tmp/../escape",
            "/tmp/%",
            "/tmp/%0g",
            "/tmp/%00",
        ] {
            let body = format!("[Trash Info]\nPath={path}\n");
            assert!(original_path(body.as_bytes()).is_err(), "{path}");
        }
        assert!(original_path(b"[Trash Info]\nPath=/tmp/a\nPath=/tmp/b\n").is_err());
        assert!(original_path(b"[Other]\nPath=/tmp/a\n").is_err());
        let oversized = format!("[Trash Info]\nPath=/tmp/{}\n", "a".repeat(4096));
        assert!(original_path(oversized.as_bytes()).is_err());
        let path = original_path(b"[Trash Info]\nPath=/tmp/space%20and%FF.txt\n").unwrap();
        assert_eq!(
            path,
            PathBuf::from(OsString::from_vec(b"/tmp/space and\xff.txt".to_vec()))
        );
    }
}
