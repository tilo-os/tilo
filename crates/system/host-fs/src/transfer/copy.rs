//! Copying a source tree through retained directory descriptors: entries are
//! read by name below an opened directory and never through a path that could
//! be replaced under the copy, so a name swapped for a link mid-copy cannot
//! lead it out of the staging folder.

use std::path::{Component, Path, PathBuf};

use super::TransferObserver;

/// Copy the tree at `source` to exactly `destination`, which must not exist.
/// A file keeps its bytes, permission bits and when it last changed; a folder
/// keeps its tree and its permission bits; a link is copied as the link it is.
pub(crate) fn copy_tree(
    source: &Path,
    destination: &Path,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    let parent = destination
        .parent()
        .ok_or_else(|| std::io::Error::other("the staged copy has no parent directory"))?;
    let name = destination
        .file_name()
        .ok_or_else(|| std::io::Error::other("the staged copy has no name"))?;
    let directory = cap_std::fs::Dir::open_ambient_dir(parent, cap_std::ambient_authority())?;
    copy_tree_to(&directory, Path::new(name), source, observer)
}

/// Copy the tree at `source` to `name` below `destination`.
pub(crate) fn copy_tree_to(
    destination: &cap_std::fs::Dir,
    name: &Path,
    source: &Path,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    let parent_path = source
        .parent()
        .ok_or_else(|| std::io::Error::other("source has no parent directory"))?;
    let source_name = source
        .file_name()
        .ok_or_else(|| std::io::Error::other("source has no file name"))?;
    let parent = cap_std::fs::Dir::open_ambient_dir(parent_path, cap_std::ambient_authority())?;
    let destination = CopyDestination {
        root: destination,
        path: name.to_path_buf(),
    };
    copy_entry(&parent, source_name, source, &destination, observer)
}

struct CopyDestination<'a> {
    root: &'a cap_std::fs::Dir,
    path: PathBuf,
}

impl CopyDestination<'_> {
    fn parent(&self) -> std::io::Result<(cap_std::fs::Dir, &std::ffi::OsStr)> {
        use cap_fs_ext::DirExt as _;

        let name = self
            .path
            .file_name()
            .ok_or_else(|| std::io::Error::other("destination has no leaf"))?;
        let mut directory = self.root.try_clone()?;
        if let Some(parent) = self.path.parent() {
            for component in parent.components() {
                let Component::Normal(name) = component else {
                    return Err(std::io::Error::other("destination escaped staging"));
                };
                directory = directory.open_dir_nofollow(name)?;
            }
        }
        Ok((directory, name))
    }

    fn child(&self, name: &std::ffi::OsStr) -> CopyDestination<'_> {
        CopyDestination {
            root: self.root,
            path: self.path.join(name),
        }
    }
}

fn copy_entry(
    parent: &cap_std::fs::Dir,
    name: &std::ffi::OsStr,
    display_path: &Path,
    destination: &CopyDestination<'_>,
    observer: &mut impl TransferObserver,
) -> std::io::Result<()> {
    use cap_fs_ext::{DirExt, FollowSymlinks, OpenOptionsFollowExt, OpenOptionsMaybeDirExt};

    let initial = parent.symlink_metadata(name)?;
    observer.before_copy_source(display_path)?;
    let (receiving, destination_name) = destination.parent()?;
    if initial.file_type().is_symlink() {
        let target = match observer.symlink_target(display_path)? {
            Some(target) => target,
            None => parent.read_link_contents(name)?,
        };
        return crate::symlink_at(&target, &receiving.into_std_file(), destination_name);
    }

    let mut options = cap_std::fs::OpenOptions::new();
    options
        .read(true)
        .follow(FollowSymlinks::No)
        .maybe_dir(true);
    let opened = observer.open_source(parent, name, &options)?;
    observer.opened_source(display_path, &opened)?;
    let metadata = opened.metadata()?;

    if metadata.is_dir() {
        receiving.create_dir(destination_name)?;
        let created = receiving.open_dir_nofollow(destination_name)?;
        let source = cap_std::fs::Dir::from_std_file(opened);
        for entry in source.entries()? {
            let entry = entry?;
            let child_name = entry.file_name();
            copy_entry(
                &source,
                &child_name,
                &display_path.join(&child_name),
                &destination.child(&child_name),
                observer,
            )?;
        }
        created.set_permissions(
            ".",
            cap_std::fs::Permissions::from_std(metadata.permissions()),
        )?;
        return Ok(());
    }
    if !metadata.is_file() {
        return Err(std::io::Error::other("source type changed during copy"));
    }

    let mut source = opened;
    let mut options = cap_std::fs::OpenOptions::new();
    options
        .write(true)
        .create_new(true)
        .follow(FollowSymlinks::No);
    let mut destination = receiving.open_with(destination_name, &options)?.into_std();
    observer.copy_bytes(&mut source, &mut destination)?;
    destination.set_permissions(observer.destination_permissions(metadata.permissions()))?;
    // A copy is the file it was made of in when it last changed too, so a
    // photo album keeps its order.
    if let Ok(modified) = metadata.modified()
        && let Err(error) = destination.set_times(std::fs::FileTimes::new().set_modified(modified))
    {
        tracing::warn!(%error, path = %display_path.display(), "a copy's time was not kept");
    }
    Ok(())
}
