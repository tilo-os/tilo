//! Bytes an earlier start accepted but did not commit: replayed into their own file when it still
//! has the content they were derived from, kept as a sibling copy when it does not, and left
//! staged when neither is possible yet.

use std::io;

use crate::acl::Access;

use super::{QuickFS, VersionSource, commit_errno, is_permanent_commit_failure, unsaved_copy_name};

impl QuickFS {
    /// Replay bytes staged by an earlier start (an `fsync` before a crash, or a commit that
    /// never succeeded). Bytes go back into their own file only while it still holds the
    /// content they were derived from; otherwise, or when that commit is refused
    /// permanently, they become a sibling copy. Entries that cannot be placed stay staged.
    /// Returns how many were placed.
    pub fn recover_unsaved_writes(&self) -> usize {
        let Some(store) = self.unsaved.clone() else {
            return 0;
        };
        let (loaded, damaged) = store.load_all();
        for (path, error) in damaged {
            eprintln!(
                "staged unsaved bytes at {} are unreadable and were left in place: {error}",
                path.display()
            );
        }
        let mut placed = 0_usize;
        for item in loaded {
            let entry = &item.entry;
            let label = entry.path.as_deref().unwrap_or(&entry.key);
            let workspace = entry
                .path
                .as_deref()
                .and_then(|path| self.hydrator.scoped_workspace(path));
            let target = self.recovery_target(entry.document_id, workspace);
            if let Some(ino) = target
                && self.ns().hash_for(ino) == entry.base()
                && self.check_access(ino, Access::Write).is_ok()
            {
                match self.commit_content(ino, &item.bytes) {
                    Ok(()) => {
                        let _ = store.remove(&entry.key);
                        placed = placed.saturating_add(1);
                        continue;
                    }
                    Err(error) if !is_permanent_commit_failure(commit_errno(&error)) => {
                        eprintln!(
                            "staged bytes for {label} could not be committed yet ({error}); kept in {}",
                            store.dir().display()
                        );
                        continue;
                    }
                    Err(_) => {}
                }
            }
            // A commit that reported failure can have landed anyway: the file's version then holds
            // exactly these bytes, and there is nothing to restore. Asked by the content id the
            // bytes produce, so a file the version did reach is not copied beside itself.
            if let Some(ino) = target
                && let Ok((Some((staged, _)), _)) = self.store_version_blocking(
                    VersionSource::Bytes(&item.bytes),
                    entry.base(),
                    workspace,
                )
            {
                let current = self.ns().hash_for(ino);
                if current == Some(staged) {
                    let _ = store.remove(&entry.key);
                    continue;
                }
            }
            let location = self.recovery_location(target, entry.path.as_deref());
            if workspace.is_some() && location.is_none() {
                continue;
            }
            let (parent, name) = location
                .unwrap_or_else(|| (crate::namespace::ROOT_INO, entry.key.clone().into_bytes()));
            let copy_name = unsaved_copy_name(&name, &item.bytes);
            match self.create_preserved_copy(
                parent,
                &copy_name,
                VersionSource::Bytes(&item.bytes),
                workspace,
            ) {
                Ok(path) => {
                    // Each of these names the place, what the bytes were derived from, and
                    // what the file and the bytes each are now: a copy made for bytes the file
                    // already holds is a bug, and these are the four facts that say which.
                    let current = target.and_then(|ino| self.ns().hash_for(ino));
                    let staged = self
                        .store_version_blocking(
                            VersionSource::Bytes(&item.bytes),
                            entry.base(),
                            workspace,
                        )
                        .ok()
                        .and_then(|(stored, _)| stored)
                        .map(|(hash, _)| hash);
                    eprintln!(
                        "staged bytes for {label} were restored as {path} (inode {target:?}, from \
                         {:?}, staged {staged:?}, file {current:?})",
                        entry.base()
                    );
                    if let Err(error) = store.remove(&entry.key) {
                        eprintln!("the staged entry for {label} was not dropped: {error}");
                    }
                    placed = placed.saturating_add(1);
                }
                Err(errno) => eprintln!(
                    "staged bytes for {label} could not be restored ({}); kept in {}",
                    io::Error::from_raw_os_error(errno),
                    store.dir().display()
                ),
            }
        }
        placed
    }

    fn recovery_target(
        &self,
        document: Option<fs_protocol::FileDocumentId>,
        workspace: Option<collab_types::WorkspaceId>,
    ) -> Option<u64> {
        let (ino, path) = {
            let ns = self.ns();
            let ino = ns.ino_for_document_id(document?)?;
            ns.getattr(ino)?;
            (ino, ns.path_for(ino))
        };
        let scope = path.and_then(|path| self.hydrator.scoped_workspace(&path));
        (scope == workspace).then_some(ino)
    }

    fn recovery_location(&self, target: Option<u64>, path: Option<&str>) -> Option<(u64, Vec<u8>)> {
        if let Some(ino) = target {
            let mut ns = self.ns();
            let name = ns.file_name_for(ino)?.as_bytes().to_vec();
            return ns.parent_ino(ino).map(|parent| (parent, name));
        }
        let (directory, name) = path?.rsplit_once('/')?;
        let directory = if directory.is_empty() { "/" } else { directory };
        let parent = self.ns().resolve_path_ino(directory)?;
        Some((parent, name.as_bytes().to_vec()))
    }
}
