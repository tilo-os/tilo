//! Runtime bundles that arrive with a thin app.
//!
//! `tilo-app package` bakes the runtime a thin app was linked against next to
//! the package. The daemon installs it for the slot user before the app, so
//! the app's first launch finds exactly that runtime.

use std::fs;
use std::io::Write as _;
use std::path::Path;

use crate::error::DaemonError;

/// The launcher's runtime trust list; when it is non-empty, only the
/// publishers it names are accepted.
pub const TRUSTED_PUBLISHERS: &str = "/etc/tilo/runtime-bundle-trusted-publishers.txt";

/// A top-level string field of the bundle's `bundle.toml`
/// (`version`, `publisher_hex`).
///
/// # Errors
/// A missing manifest or field.
pub fn bundle_field(bundle: &Path, key: &str) -> Result<String, DaemonError> {
    let manifest = bundle.join("bundle.toml");
    let text = fs::read_to_string(&manifest)
        .map_err(DaemonError::io(format!("read {}", manifest.display())))?;
    text.lines()
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| {
            let (name, value) = line.split_once('=')?;
            (name.trim() == key).then(|| value.trim().trim_matches('"').to_owned())
        })
        .filter(|value| !value.is_empty())
        .ok_or_else(|| DaemonError::Apply {
            step: "read the runtime bundle".into(),
            detail: format!("{} has no {key}", manifest.display()),
        })
}

/// Add `publisher` to the trust list at `list` when that list is in force
/// (exists with at least one entry) and does not name it yet. Returns whether
/// it was added. With no list in force every signed publisher is accepted
/// already, and the list is left alone.
///
/// # Errors
/// Reading or appending to the list.
pub fn trust_publisher(list: &Path, publisher: &str) -> Result<bool, DaemonError> {
    let Ok(text) = fs::read_to_string(list) else {
        return Ok(false);
    };
    let mut entries = text
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .peekable();
    if entries.peek().is_none() || entries.any(|e| e.eq_ignore_ascii_case(publisher)) {
        return Ok(false);
    }
    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(list)
        .map_err(DaemonError::io(format!("open {}", list.display())))?;
    let separator = if text.ends_with('\n') || text.is_empty() {
        ""
    } else {
        "\n"
    };
    write!(
        file,
        "{separator}# runtime bundle installed by tilo-vm with a thin app\n{publisher}\n"
    )
    .map_err(DaemonError::io(format!("append to {}", list.display())))?;
    Ok(true)
}

/// Unpack an uploaded tar of a bundle directory's contents into `dest`.
///
/// # Errors
/// Unreadable or malformed archives.
pub fn unpack(archive: &Path, dest: &Path) -> Result<(), DaemonError> {
    let file = fs::File::open(archive).map_err(DaemonError::io("open the runtime upload"))?;
    fs::create_dir_all(dest).map_err(DaemonError::io("create the runtime staging"))?;
    tar::Archive::new(file)
        .unpack(dest)
        .map_err(DaemonError::io("unpack the runtime upload"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bundle_fields_come_from_the_top_level_table() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join("bundle.toml"),
            "version = \"0.0.0-local+ab\"\npublisher_hex = \"aa11\"\n\n[[files]]\nversion = \"not this\"\n",
        )
        .unwrap();
        assert_eq!(
            bundle_field(dir.path(), "version").unwrap(),
            "0.0.0-local+ab"
        );
        assert_eq!(bundle_field(dir.path(), "publisher_hex").unwrap(), "aa11");
        assert!(bundle_field(dir.path(), "arch").is_err());
    }

    #[test]
    fn a_publisher_joins_only_a_trust_list_in_force() {
        let dir = tempfile::tempdir().unwrap();
        let list = dir.path().join("trusted.txt");
        assert!(!trust_publisher(&list, "aa11").unwrap());
        assert!(!list.exists());

        fs::write(&list, "# no entries yet\n").unwrap();
        assert!(!trust_publisher(&list, "aa11").unwrap());

        fs::write(&list, "bb22").unwrap();
        assert!(trust_publisher(&list, "aa11").unwrap());
        assert!(!trust_publisher(&list, "AA11").unwrap());
        let entries: Vec<String> = fs::read_to_string(&list)
            .unwrap()
            .lines()
            .filter(|l| !l.starts_with('#'))
            .map(str::to_owned)
            .collect();
        assert_eq!(entries, ["bb22", "aa11"]);
    }
}
