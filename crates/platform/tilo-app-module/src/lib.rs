//! Later builds of a running app whose payload is its module, as the directory
//! protocol puts them there.
//!
//! An app whose payload is code its runtime interprets (a tilolang package) can
//! take a later build while it runs. The writer publishes each build as a
//! numbered generation directory beside the last, then names it in a
//! [`CURRENT_FILE`] by one rename; the reader watches that file and reads the
//! generation it names. A running app is never pointed at a directory still
//! being written, and a build that fails to publish leaves the one before it
//! current.
//!
//! A session's launcher hands a verified reinstall over
//! (`crates/packaging/launcher/src/live_module.rs`), and `tilolang dev` runs its
//! own app on this desktop (`crates/lang/tilolang-cli/src/dev.rs`).

use std::io;
use std::path::{Path, PathBuf};

pub const UPDATES_ENV: &str = "TILO_APP_MODULE_UPDATES";
pub const CURRENT_FILE: &str = "current";
const PARTIAL: &str = ".partial";

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

#[must_use]
pub fn dir_from_env() -> Option<PathBuf> {
    std::env::var_os(UPDATES_ENV)
        .filter(|dir| !dir.is_empty())
        .map(PathBuf::from)
}

/// Make `dir` and its parents, private to its owner where the platform has
/// modes: an app reads what the tool wrote, as a session's launcher keeps its
/// copy.
pub fn make_dir(dir: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        std::fs::DirBuilder::new()
            .recursive(true)
            .mode(0o700)
            .create(dir)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(dir)
    }
}

pub fn current(dir: &Path) -> io::Result<Option<u64>> {
    let path = dir.join(CURRENT_FILE);
    let text = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    let name = text.trim();
    if name.is_empty() || !name.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err(invalid(format!(
            "{} names no build: {name:?}",
            path.display()
        )));
    }
    name.parse()
        .map(Some)
        .map_err(|_| invalid(format!("{} names no build: {name:?}", path.display())))
}

/// Publish the payload tree at `payload` as the next generation in `dir` and
/// name it current; the generation's number.
pub fn publish(dir: &Path, payload: &Path) -> io::Result<u64> {
    publish_with(dir, |generation| copy_tree(payload, generation))
}

/// Publish a generation that `write` fills, and name it current; the
/// generation's number.
///
/// A writer that holds the payload itself (a compiled bundle) writes it
/// straight into the generation this makes; nothing is staged or copied.
pub fn publish_with(dir: &Path, write: impl FnOnce(&Path) -> io::Result<()>) -> io::Result<u64> {
    make_dir(dir)?;
    let generation = match current(dir)? {
        Some(last) => last
            .checked_add(1)
            .ok_or_else(|| invalid("no generation number is left"))?,
        None => 1,
    };
    let name = generation.to_string();
    let staging = dir.join(format!("{name}{PARTIAL}"));
    // A publish that failed left its generation half written.
    let _ = std::fs::remove_dir_all(&staging);
    write(&staging)?;
    std::fs::rename(&staging, dir.join(&name))?;
    let named = dir.join(format!("{CURRENT_FILE}{PARTIAL}"));
    std::fs::write(&named, format!("{name}\n"))?;
    std::fs::rename(&named, dir.join(CURRENT_FILE))?;
    forget_before(dir, generation);
    Ok(generation)
}

/// Remove the generations before the one before `current`: the app reads the current one, and
/// may still be reading the one it replaced.
pub fn forget_before(dir: &Path, current: u64) {
    let Some(keep) = current.checked_sub(1) else {
        return;
    };
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let old = entry
            .file_name()
            .to_str()
            .and_then(|name| name.parse::<u64>().ok())
            .is_some_and(|number| number < keep);
        if old {
            let _ = std::fs::remove_dir_all(entry.path());
        }
    }
}

/// Copy the payload tree `from` into `to`: regular files and directories, and
/// nothing else (a payload holds no links).
fn copy_tree(from: &Path, to: &Path) -> io::Result<()> {
    std::fs::create_dir_all(to)?;
    for entry in std::fs::read_dir(from)? {
        let entry = entry?;
        let kind = entry.file_type()?;
        let target = to.join(entry.file_name());
        if kind.is_dir() {
            copy_tree(&entry.path(), &target)?;
        } else if kind.is_file() {
            std::fs::copy(entry.path(), &target)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn payload(root: &Path, text: &str) -> PathBuf {
        let dir = root.join(text);
        std::fs::create_dir_all(dir.join("imports")).unwrap();
        std::fs::write(dir.join("app.wasm"), text.as_bytes()).unwrap();
        std::fs::write(dir.join("imports/other.wasm"), b"other").unwrap();
        dir
    }

    fn generations(dir: &Path) -> Vec<String> {
        let mut names: Vec<String> = std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .filter(|name| name.chars().all(|c| c.is_ascii_digit()))
            .collect();
        names.sort();
        names
    }

    #[test]
    fn the_current_file_names_the_generation_a_publish_copied() {
        let out = tempfile::tempdir().unwrap();
        let updates = out.path().join("updates");
        assert_eq!(current(&updates).unwrap(), None, "nothing published yet");
        let first = publish(&updates, &payload(out.path(), "one")).unwrap();
        assert_eq!(first, 1);
        assert_eq!(current(&updates).unwrap(), Some(1));
        let read = updates.join("1");
        assert_eq!(
            std::fs::read_to_string(read.join("app.wasm")).unwrap(),
            "one"
        );
        assert_eq!(
            std::fs::read_to_string(read.join("imports/other.wasm")).unwrap(),
            "other",
            "the packages it uses come with it"
        );
        let second = publish(&updates, &payload(out.path(), "two")).unwrap();
        assert_eq!(second, 2);
        assert_eq!(current(&updates).unwrap(), Some(2));
        assert_eq!(
            std::fs::read_to_string(updates.join("2/app.wasm")).unwrap(),
            "two"
        );
        assert_eq!(
            generations(&updates),
            ["1", "2"],
            "the one it replaced stays"
        );
    }

    #[test]
    fn only_the_generation_before_the_current_one_is_kept() {
        let out = tempfile::tempdir().unwrap();
        let updates = out.path().join("updates");
        for text in ["one", "two", "three"] {
            publish(&updates, &payload(out.path(), text)).unwrap();
        }
        assert_eq!(generations(&updates), ["2", "3"]);
    }

    #[test]
    fn a_writer_publishes_the_generation_it_fills_itself() {
        let out = tempfile::tempdir().unwrap();
        let updates = out.path().join("updates");
        let first = publish_with(&updates, |generation| {
            std::fs::create_dir_all(generation)?;
            std::fs::write(generation.join("app.wasm"), "written")
        })
        .unwrap();
        assert_eq!(first, 1);
        assert_eq!(current(&updates).unwrap(), Some(1));
        assert_eq!(
            std::fs::read_to_string(updates.join("1/app.wasm")).unwrap(),
            "written"
        );
        assert_eq!(
            generations(&updates),
            ["1"],
            "no half-written staging is left"
        );
        let second = publish_with(&updates, |generation| {
            std::fs::create_dir_all(generation)?;
            std::fs::write(generation.join("app.wasm"), "again")
        })
        .unwrap();
        assert_eq!(second, 2);
        assert_eq!(current(&updates).unwrap(), Some(2));
        assert_eq!(generations(&updates), ["1", "2"]);
    }

    #[test]
    fn a_current_that_names_no_build_is_refused() {
        let out = tempfile::tempdir().unwrap();
        std::fs::write(out.path().join(CURRENT_FILE), "the third one\n").unwrap();
        let error = current(out.path()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("names no build"), "{error}");
        assert!(publish(out.path(), &payload(out.path(), "one")).is_err());
    }

    #[test]
    fn a_generation_is_never_read_half_written() {
        let out = tempfile::tempdir().unwrap();
        let updates = out.path().join("updates");
        std::fs::create_dir_all(updates.join("1.partial")).unwrap();
        assert_eq!(current(&updates).unwrap(), None);
        assert_eq!(publish(&updates, &payload(out.path(), "one")).unwrap(), 1);
        assert_eq!(generations(&updates), ["1"], "the half one is gone");
        assert!(!updates.join("1.partial").exists());
    }
}
