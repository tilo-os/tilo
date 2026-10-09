//! Cross-device compensation keeps the old object until its replacement and
//! write-ahead identity are durable. A killed worker resumes the same move.
use super::*;
use file_explorer::fileops::{SourceDirectory, SourceOpenOptions, TransferObserver};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct MoveIntent {
    source: Vec<u8>,
    target: Vec<u8>,
    source_seal: Seal,
    container: Vec<u8>,
    container_seal: Seal,
    staged: Option<(Vec<u8>, Seal)>,
    quarantine: Vec<u8>,
    published_copy: Option<Vec<u8>>,
}

pub(super) struct Mover<'a> {
    pub receipt: &'a PendingReceipt,
    pub index: usize,
    pub tree: &'a WorkspaceTreeHandle,
    pub context: &'a FileClipboardContext,
}
impl Mover<'_> {
    fn snapshot(&self) -> Result<Receipt, String> {
        self.receipt
            .receipt
            .lock()
            .map(|receipt| receipt.clone())
            .map_err(|_| "clipboard journal unavailable".into())
    }
    fn check(&self) -> Result<Receipt, String> {
        self.context.authorize(Permission::Write)?;
        let receipt = self.snapshot()?;
        receipt.destination.open(self.tree, self.context)?;
        if receipt.cut {
            receipt
                .items
                .get(self.index)
                .ok_or("clipboard item unavailable")?
                .parent
                .open(self.tree, self.context)?;
        }
        Ok(receipt)
    }
    fn directory(&self, path: &Path) -> Result<SecureNearbyDestination, String> {
        let receipt = self.check()?;
        let item = receipt
            .items
            .get(self.index)
            .ok_or("clipboard item unavailable")?;
        let receiving = decode_path(&receipt.destination.path)?;
        let original = decode_path(&item.parent.path)?;
        if path == original && receipt.cut {
            return item.parent.open(self.tree, self.context);
        }
        if path == receiving {
            return receipt.destination.open(self.tree, self.context);
        }
        if path.parent() == Some(receiving.as_path()) {
            let directory = SecureNearbyDestination::open_beneath(&receiving, path)?;
            let metadata = directory.metadata()?;
            // SAFETY: geteuid reads this process's effective user.
            if metadata.uid() != unsafe { libc::geteuid() } || metadata.mode() & 0o077 != 0 {
                return Err("clipboard recovery folder is not private".into());
            }
            return Ok(directory);
        }
        let trash = tilo_paths::data_dir()
            .ok_or("home Trash unavailable")?
            .join("Trash/files");
        if path == trash {
            return super::super::trash::files_directory();
        }
        Err("compensation path escaped its authorized folders".into())
    }
    pub fn move_entry(&self, source: &Path, target: &Path) -> Result<(), String> {
        let source_parent = self.directory(source.parent().ok_or("move source has no parent")?)?;
        let destination = self.directory(target.parent().ok_or("move target has no parent")?)?;
        let source_name = source.file_name().ok_or("move source has no name")?;
        let target_name = target.file_name().ok_or("move target has no name")?;
        let existing = self
            .snapshot()?
            .items
            .get(self.index)
            .ok_or("clipboard item unavailable")?
            .moving
            .clone();
        if let Some(intent) = &existing {
            if decode_path(&intent.source)? != source || decode_path(&intent.target)? != target {
                return Err("another durable compensation move is pending".into());
            }
            return self.finish(intent);
        }
        let source_entry = source_parent.entry(source_name)?;
        let source_seal = Seal::of(&source_entry.metadata().map_err(|error| error.to_string())?);
        // Same-device moves preserve resource identity and need no streamed copy.
        if source_seal.identity.0 == destination.metadata()?.dev() {
            self.check()?;
            rename_entry(&source_parent, source_name, &destination, target_name)?;
            sync_parent(&source_parent)?;
            return sync_parent(&destination);
        }
        let container_name =
            OsString::from(format!(".tilo-clipboard-relocate-{}", uuid::Uuid::new_v4()));
        let container = destination.private_child(&container_name)?;
        let container_path = target
            .parent()
            .ok_or("move target has no parent")?
            .join(&container_name);
        let intent = MoveIntent {
            source: source.as_os_str().as_bytes().into(),
            target: target.as_os_str().as_bytes().into(),
            source_seal,
            published_copy: None,
            container: container_path.as_os_str().as_bytes().into(),
            container_seal: Seal::of(&container.metadata()?),
            staged: None,
            quarantine: source
                .parent()
                .ok_or("move source has no parent")?
                .join(format!(
                    ".tilo-clipboard-relocate-source-{}",
                    uuid::Uuid::new_v4()
                ))
                .as_os_str()
                .as_bytes()
                .into(),
        };
        sync_parent(&destination)?;
        self.receipt.change(|receipt| {
            receipt
                .items
                .get_mut(self.index)
                .ok_or("clipboard item unavailable")?
                .moving = Some(intent.clone());
            Ok(())
        })?;
        self.finish(&intent)
    }
    pub fn resume(&self) -> Result<(), String> {
        let receipt = self.snapshot()?;
        if let Some(intent) = &receipt
            .items
            .get(self.index)
            .ok_or("clipboard item unavailable")?
            .moving
        {
            self.finish(intent)?;
        }
        Ok(())
    }
    fn finish(&self, saved: &MoveIntent) -> Result<(), String> {
        let source = decode_path(&saved.source)?;
        let target = decode_path(&saved.target)?;
        let container_path = decode_path(&saved.container)?;
        if container_path.parent() != target.parent() {
            return Err("saved compensation container escaped its target folder".into());
        }
        let source_parent = self.directory(source.parent().ok_or("move source has no parent")?)?;
        let destination = self.directory(target.parent().ok_or("move target has no parent")?)?;
        let container = SecureNearbyDestination::open_beneath(
            target.parent().ok_or("move target has no parent")?,
            &container_path,
        )?;
        if !saved.container_seal.matches(&container.metadata()?) {
            return Err("saved compensation container was replaced".into());
        }
        let mut intent = saved.clone();
        if intent.staged.is_none() {
            let current =
                source_parent.entry(source.file_name().ok_or("move source has no name")?)?;
            if !intent
                .source_seal
                .matches(&current.metadata().map_err(|error| error.to_string())?)
            {
                return Err("compensation source was replaced".into());
            }
            // A pre-stage interruption left the original intact. Clear only
            // this verified private container before copying it again.
            clear_private(&container, &mut || self.check().map(|_| ()))?;
            let mut observer = CopyObserver {
                mover: self,
                container: &container,
                container_path: &container_path,
            };
            file_explorer::fileops::transfer_into_with(
                &source_parent
                    .anchored_root()
                    .join(source.file_name().ok_or("move source has no name")?),
                &container.anchored_root(),
                false,
                &mut observer,
            )?;
            intent = self
                .snapshot()?
                .items
                .get(self.index)
                .ok_or("clipboard item unavailable")?
                .moving
                .clone()
                .ok_or("compensation intent unavailable")?;
        }
        self.publish(&intent, &destination)?;
        self.remove_original(&intent, &source_parent)?;
        let metadata = destination
            .entry(target.file_name().ok_or("move target has no name")?)?
            .metadata()
            .map_err(|error| error.to_string())?;
        self.receipt.change(|receipt| {
            let item = receipt
                .items
                .get_mut(self.index)
                .ok_or("clipboard item unavailable")?;
            if let Some((_, seal)) = &mut item.staged {
                *seal = Seal::of(&metadata);
            }
            item.moving = None;
            Ok(())
        })?;
        clear_private(&container, &mut || self.check().map(|_| ()))?;
        fs::remove_dir(
            destination
                .anchored_root()
                .join(container_path.file_name().ok_or("container has no name")?),
        )
        .map_err(|error| error.to_string())?;
        sync_parent(&destination)
    }
    fn publish(
        &self,
        intent: &MoveIntent,
        destination: &SecureNearbyDestination,
    ) -> Result<(), String> {
        let target = decode_path(&intent.target)?;
        let name = target.file_name().ok_or("move target has no name")?;
        let (staged, seal) = intent
            .staged
            .as_ref()
            .ok_or("compensation staged object unavailable")?;
        if let Some(entry) = optional_entry(destination, name)? {
            if !seal.matches(&entry.metadata().map_err(|error| error.to_string())?) {
                return Err("compensation target is occupied by another object".into());
            }
        } else {
            let mut staged = decode_path(staged)?;
            if let Some(published) = &intent.published_copy {
                let candidate = decode_path(published)?;
                let parent = candidate.parent().ok_or("private copy has no parent")?;
                let container = decode_path(&intent.container)?;
                if parent != container {
                    return Err("private copy escaped its compensation container".into());
                }
                let directory = SecureNearbyDestination::open_beneath(&container, parent)?;
                if optional_entry(
                    &directory,
                    candidate.file_name().ok_or("private copy has no name")?,
                )?
                .is_some()
                {
                    staged = candidate;
                }
            }
            let parent = staged.parent().ok_or("staged object has no parent")?;
            let container = decode_path(&intent.container)?;
            if parent != container && parent.parent() != Some(container.as_path()) {
                return Err("compensation stage escaped its private container".into());
            }
            let directory = SecureNearbyDestination::open_beneath(&container, parent)?;
            let leaf = staged.file_name().ok_or("staged object has no name")?;
            let entry = directory.entry(leaf)?;
            if !seal.matches(&entry.metadata().map_err(|error| error.to_string())?) {
                return Err("compensation stage was replaced".into());
            }
            self.check()?;
            rename_entry(&directory, leaf, destination, name)?;
            sync_parent(&directory)?;
        }
        sync_parent(destination)?;
        #[cfg(test)]
        super::super::test_support::pause(
            &decode_path(&self.snapshot()?.destination.path)?,
            "compensation-after-publish",
        )
        .map_err(|error| error.to_string())?;
        #[cfg(test)]
        super::super::interrupt_test_process("compensation-after-publish");
        Ok(())
    }
    fn remove_original(
        &self,
        intent: &MoveIntent,
        source: &SecureNearbyDestination,
    ) -> Result<(), String> {
        let original = decode_path(&intent.source)?;
        let quarantine = decode_path(&intent.quarantine)?;
        if quarantine.parent() != original.parent() {
            return Err("saved quarantine escaped source".into());
        }
        let name = original.file_name().ok_or("move source has no name")?;
        let retained = quarantine.file_name().ok_or("quarantine has no name")?;
        if let Some(entry) = optional_entry(source, name)? {
            if !intent
                .source_seal
                .matches(&entry.metadata().map_err(|error| error.to_string())?)
            {
                return Err("compensation source was replaced before removal".into());
            }
            self.check()?;
            rename_entry(source, name, source, retained)?;
            sync_parent(source)?;
        }
        if let Some(entry) = optional_entry(source, retained)? {
            if !intent
                .source_seal
                .matches(&entry.metadata().map_err(|error| error.to_string())?)
            {
                return Err("compensation source quarantine was replaced".into());
            }
            remove_private_entry(source, retained, &mut || self.check().map(|_| ()))?;
        }
        sync_parent(source)
    }
}
fn sync_parent(directory: &SecureNearbyDestination) -> Result<(), String> {
    File::open(directory.anchored_root())
        .and_then(|file| file.sync_all())
        .map_err(|error| error.to_string())
}
fn clear_private(
    directory: &SecureNearbyDestination,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut remaining = MAX_RETAINED_DESCRIPTORS;
    clear_bounded(directory, check, &mut remaining, 0)
}
fn clear_bounded(
    directory: &SecureNearbyDestination,
    check: &mut impl FnMut() -> Result<(), String>,
    remaining: &mut usize,
    depth: usize,
) -> Result<(), String> {
    for entry in fs::read_dir(directory.anchored_root()).map_err(|error| error.to_string())? {
        let name = entry.map_err(|error| error.to_string())?.file_name();
        remove_bounded(directory, &name, check, remaining, depth)?;
    }
    sync_parent(directory)
}
fn remove_private_entry(
    directory: &SecureNearbyDestination,
    name: &std::ffi::OsStr,
    check: &mut impl FnMut() -> Result<(), String>,
) -> Result<(), String> {
    let mut remaining = MAX_RETAINED_DESCRIPTORS;
    remove_bounded(directory, name, check, &mut remaining, 0)
}
fn remove_bounded(
    directory: &SecureNearbyDestination,
    name: &std::ffi::OsStr,
    check: &mut impl FnMut() -> Result<(), String>,
    remaining: &mut usize,
    depth: usize,
) -> Result<(), String> {
    if *remaining == 0 || depth > 256 {
        return Err("compensation removal exceeds its bounded tree".into());
    }
    *remaining = remaining.saturating_sub(1);
    check()?;
    let entry = directory.entry(name)?;
    let metadata = entry.metadata().map_err(|error| error.to_string())?;
    if metadata.is_dir() {
        let child = SecureNearbyDestination::open_beneath(
            &directory.anchored_root(),
            &directory.anchored_root().join(name),
        )?;
        if Identity::of(&child.metadata()?) != Identity::of(&metadata) {
            return Err("private removal object changed".into());
        }
        clear_bounded(&child, check, remaining, depth.saturating_add(1))?;
        check()?;
        let current = directory.entry(name)?;
        if Identity::of(&current.metadata().map_err(|error| error.to_string())?)
            != Identity::of(&metadata)
        {
            return Err("private removal object changed".into());
        }
        fs::remove_dir(directory.anchored_root().join(name)).map_err(|error| error.to_string())
    } else {
        check()?;
        let current = directory.entry(name)?;
        if Identity::of(&current.metadata().map_err(|error| error.to_string())?)
            != Identity::of(&metadata)
        {
            return Err("private removal object changed".into());
        }
        fs::remove_file(directory.anchored_root().join(name)).map_err(|error| error.to_string())
    }
}
struct CopyObserver<'a, 'b> {
    mover: &'a Mover<'b>,
    container: &'a SecureNearbyDestination,
    container_path: &'a Path,
}
impl TransferObserver for CopyObserver<'_, '_> {
    fn before_copy_source(&mut self, _: &Path) -> io::Result<()> {
        self.mover.check().map(|_| ()).map_err(io::Error::other)
    }
    fn open_source(
        &mut self,
        parent: &SourceDirectory,
        name: &std::ffi::OsStr,
        _: &SourceOpenOptions,
    ) -> io::Result<File> {
        let name = std::ffi::CString::new(name.as_bytes())?;
        // SAFETY: retained parent fd and one terminated leaf. Links and FIFOs
        // are never followed or allowed to block a compensation worker.
        let fd = unsafe {
            libc::openat(
                parent.as_raw_fd(),
                name.as_ptr(),
                libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        // SAFETY: successful openat transferred this newly owned descriptor.
        Ok(unsafe { File::from_raw_fd(fd) })
    }
    fn staged_source(&mut self, path: &Path) -> io::Result<()> {
        self.mover.check().map_err(io::Error::other)?;
        let relative = path
            .strip_prefix(self.container.anchored_root())
            .map_err(io::Error::other)?;
        let parent = path
            .parent()
            .ok_or_else(|| io::Error::other("stage has no parent"))?;
        let directory =
            SecureNearbyDestination::open_beneath(&self.container.anchored_root(), parent)
                .map_err(io::Error::other)?;
        let entry = directory
            .entry(
                path.file_name()
                    .ok_or_else(|| io::Error::other("stage has no name"))?,
            )
            .map_err(io::Error::other)?;
        sync_staged(&entry, &mut || {
            self.mover.check().map(|_| ()).map_err(io::Error::other)
        })?;
        sync_parent(&directory).map_err(io::Error::other)?;
        sync_parent(self.container).map_err(io::Error::other)?;
        let stable = self.container_path.join(relative);
        let seal = Seal::of(&entry.metadata()?);
        self.mover
            .receipt
            .change(|receipt| {
                receipt
                    .items
                    .get_mut(self.mover.index)
                    .ok_or("clipboard item unavailable")?
                    .moving
                    .as_mut()
                    .ok_or("compensation intent unavailable")?
                    .staged = Some((stable.as_os_str().as_bytes().into(), seal));
                Ok(())
            })
            .map_err(io::Error::other)
    }
    fn before_publish(&mut self, path: &Path) -> io::Result<()> {
        self.mover.check().map_err(io::Error::other)?;
        let relative = path
            .strip_prefix(self.container.anchored_root())
            .map_err(io::Error::other)?;
        let stable = self.container_path.join(relative);
        self.mover
            .receipt
            .change(|receipt| {
                receipt
                    .items
                    .get_mut(self.mover.index)
                    .ok_or("clipboard item unavailable")?
                    .moving
                    .as_mut()
                    .ok_or("compensation intent unavailable")?
                    .published_copy = Some(stable.as_os_str().as_bytes().into());
                Ok(())
            })
            .map_err(io::Error::other)
    }
    fn copy_bytes(&mut self, source: &mut File, destination: &mut File) -> io::Result<u64> {
        let mut buffer = vec![0; 256 * 1024];
        let mut total = 0u64;
        loop {
            self.mover.check().map_err(io::Error::other)?;
            let count = source.read(&mut buffer)?;
            if count == 0 {
                return Ok(total);
            }
            destination.write_all(
                buffer
                    .get(..count)
                    .ok_or_else(|| io::Error::other("invalid read length"))?,
            )?;
            total = total.saturating_add(u64::try_from(count).map_err(io::Error::other)?);
        }
    }
}
