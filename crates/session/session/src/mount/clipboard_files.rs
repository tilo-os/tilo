//! Pasting copied files into a folder, with a journal that outlives the
//! session.
//!
//! A paste is not a wish: every item is written down before it is touched and
//! marked done after, through the device's settings document ([`PASTE`]), so
//! a session that dies in the middle of one finishes it when it starts again
//! instead of leaving a folder half filled with no record of what was meant.
//!
//! Copying is item by item, so a large paste does not hold up a frame: the
//! session carries out one item per turn of its loop. Copying over an
//! existing target is idempotent (a file is truncated, a folder merges), so
//! an item whose copy finished just before a crash is simply redone.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use settings_store::clipboard_history::{
    ItemState, MAX_PASTE_ITEMS, MAX_PATH_BYTES, PASTE, StoredItem, StoredPaste,
};
use ui::dnd::CopiedFiles;

/// Plan the paste of `files` into `destination`: what lands where. `None`
/// when there is nothing to paste, or the destination is not a folder.
///
/// Nothing is touched on the disk here; the plan is what [`begin`] writes
/// down before the first item is carried out.
#[must_use]
pub(crate) fn plan(files: &CopiedFiles, destination: &Path, now_ms: u64) -> Option<StoredPaste> {
    if !destination.is_dir() {
        return None;
    }
    let mut items = Vec::new();
    for source in &files.paths {
        if items.len() == MAX_PASTE_ITEMS {
            break;
        }
        let source_text = source.to_string_lossy();
        if source_text.len() > MAX_PATH_BYTES {
            continue;
        }
        // Pasted into the folder it already sits in, a file has nowhere to go.
        let Some(target) = free_target(destination, source) else {
            continue;
        };
        items.push(StoredItem {
            source: source_text.into_owned(),
            target: target.to_string_lossy().into_owned(),
            state: ItemState::Pending,
        });
    }
    if items.is_empty() {
        return None;
    }
    Some(StoredPaste {
        moving: files.cut,
        destination: destination.to_string_lossy().into_owned(),
        items,
        started_ms: now_ms,
    })
}

/// Write the paste of `files` into `destination` down, before any of it is
/// carried out. `None` when there is nothing to paste; the journal is then
/// left as it was.
pub(crate) fn begin(files: &CopiedFiles, destination: &Path, now_ms: u64) -> Option<StoredPaste> {
    let paste = plan(files, destination, now_ms)?;
    save(Some(&paste));
    Some(paste)
}

/// Carry out one item of `paste`, journalled before and after. Returns
/// whether the paste is complete.
///
/// An item that cannot be carried out is dropped from the journal with a
/// warning rather than retried forever: the rest of the paste still lands.
pub(crate) fn step(paste: &mut StoredPaste) -> bool {
    let Some(index) = paste.items.iter().position(|item| match item.state {
        ItemState::Pending => true,
        ItemState::Copied => paste.moving,
        ItemState::Moved => false,
    }) else {
        return true;
    };
    let Some(item) = paste.items.get(index) else {
        return true;
    };
    let (source, target, state) = (
        PathBuf::from(&item.source),
        PathBuf::from(&item.target),
        item.state,
    );
    let carried = match state {
        ItemState::Pending => copy_tree(&source, &target).map(|()| ItemState::Copied),
        _ => remove(&source).map(|()| ItemState::Moved),
    };
    match carried {
        Ok(done) => {
            if let Some(item) = paste.items.get_mut(index) {
                item.state = done;
            }
        }
        Err(error) => {
            tracing::warn!(
                %error,
                source = %source.display(),
                target = %target.display(),
                "a file was not pasted"
            );
            paste.items.remove(index);
        }
    }
    paste.items.is_empty()
}

/// Write the journal: `None` when there is no paste in flight.
pub(crate) fn save(paste: Option<&StoredPaste>) {
    if let Err(error) = PASTE.set(paste.cloned()) {
        tracing::warn!(%error, "the file paste journal was not written");
    }
}

/// The target `source` takes in `folder`: its own name, or the first free
/// numbered one so a paste never overwrites what is already there, and
/// `None` when it is already in that folder.
fn free_target(folder: &Path, source: &Path) -> Option<PathBuf> {
    let name = source.file_name()?;
    let stem = source.file_stem()?;
    let extension = source.extension();
    let mut candidate = folder.join(name);
    let mut number: u32 = 2;
    while fs::symlink_metadata(&candidate).is_ok() {
        if candidate == source {
            return None;
        }
        let mut numbered = stem.to_os_string();
        numbered.push(format!(" ({number})"));
        if let Some(extension) = extension {
            numbered.push(".");
            numbered.push(extension);
        }
        candidate = folder.join(numbered);
        number = number.saturating_add(1);
        if number > 9999 {
            return None;
        }
    }
    Some(candidate)
}

/// Copy `source` to `target`: a file's bytes, a folder's tree, or a symlink
/// as the link it is. A folder that already exists is merged into, so a copy
/// interrupted by a crash can be taken again.
fn copy_tree(source: &Path, target: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(source)?;
    if metadata.is_symlink() {
        let link = fs::read_link(source)?;
        match fs::remove_file(target) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => return Err(error),
        }
        std::os::unix::fs::symlink(link, target)
    } else if metadata.is_dir() {
        fs::create_dir_all(target)?;
        for entry in fs::read_dir(source)? {
            let entry = entry?;
            copy_tree(&entry.path(), &target.join(entry.file_name()))?;
        }
        Ok(())
    } else if metadata.is_file() {
        if let Some(parent) = target.parent() {
            fs::create_dir_all(parent)?;
        }
        fs::copy(source, target).map(drop)
    } else {
        Err(io::Error::other(format!(
            "{} is not a file, a folder or a link",
            source.display()
        )))
    }
}

/// Take `path` away: a folder with everything in it, a file, or a link. A
/// path that is already gone is the outcome a move wanted.
fn remove(path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_dir() && !metadata.is_symlink() => fs::remove_dir_all(path),
        Ok(_) => fs::remove_file(path),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> tempfile::TempDir {
        tempfile::Builder::new()
            .prefix(&format!("clipboard-files-{name}-"))
            .tempdir()
            .expect("a scratch folder")
    }

    fn files(paths: &[&Path], cut: bool) -> CopiedFiles {
        CopiedFiles {
            paths: paths.iter().map(Path::new).map(Path::to_path_buf).collect(),
            cut,
        }
    }

    #[test]
    fn a_paste_gives_each_file_its_own_free_name() {
        let dir = scratch("names");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(&source).expect("the source folder");
        fs::create_dir_all(&destination).expect("the destination folder");
        let note = source.join("note.txt");
        fs::write(&note, b"mine").expect("a file to copy");
        fs::write(destination.join("note.txt"), b"theirs").expect("a file in the way");

        let paste = plan(&files(&[&note], false), &destination, 7).expect("a paste");
        assert_eq!(paste.items.len(), 1);
        assert_eq!(
            Path::new(&paste.items[0].target),
            destination.join("note (2).txt"),
            "the file already there is left alone"
        );
        assert_eq!(paste.items[0].state, ItemState::Pending);
        assert!(!paste.moving);
        assert_eq!(paste.started_ms, 7);

        // Pasting into the folder the files already sit in does nothing:
        // there is no name to give them.
        assert_eq!(plan(&files(&[&note], false), &source, 7), None);
        // And a destination that is not a folder is not a paste either.
        assert_eq!(plan(&files(&[&note], false), &note, 7), None);
    }

    #[test]
    fn a_paste_carries_its_items_out_and_a_cut_takes_the_sources() {
        let dir = scratch("carry");
        let source = dir.path().join("source");
        let destination = dir.path().join("destination");
        fs::create_dir_all(source.join("folder")).expect("a source folder");
        fs::create_dir_all(&destination).expect("a destination folder");
        let loose = source.join("note.txt");
        fs::write(&loose, b"hello").expect("a file");
        fs::write(source.join("folder").join("inner.txt"), b"inner").expect("a nested file");

        let mut paste = plan(
            &files(&[&loose, &source.join("folder")], true),
            &destination,
            0,
        )
        .expect("a paste");
        let mut turns = 0;
        while !step(&mut paste) {
            turns += 1;
            assert!(turns < 10, "the paste finishes");
        }
        assert_eq!(
            fs::read(destination.join("note.txt")).expect("pasted"),
            b"hello"
        );
        assert_eq!(
            fs::read(destination.join("folder").join("inner.txt")).expect("pasted tree"),
            b"inner"
        );
        assert!(!loose.exists(), "a cut takes its source once it is copied");
        assert!(!source.join("folder").exists());
    }
}
