//! Files an app hands over through the document portal.
//!
//! A sandboxed app names a file of its own by the `file://` URI of its
//! document-portal export (`$XDG_RUNTIME_DIR/doc/<id>/<name>`), never by a
//! host path. Whatever the backend reads on an app's behalf must resolve,
//! after following links, below that mount: an app cannot make the session
//! read a host file it was never given.

use std::os::unix::ffi::OsStringExt as _;
use std::path::{Component, Path, PathBuf};

/// The document portal's mount.
fn root() -> Option<PathBuf> {
    tilo_paths::runtime_dir().map(|dir| dir.join("doc"))
}

/// The exported file `uri` names.
///
/// # Errors
/// Why it is no document-portal export: malformed, remote, outside the
/// mount, gone, or not a file.
pub fn exported_file(uri: &str) -> Result<PathBuf, String> {
    exported_file_below(uri, &root().ok_or("no runtime directory")?)
}

fn exported_file_below(uri: &str, root: &Path) -> Result<PathBuf, String> {
    if !crate::plain_text(uri, 8192) || uri.trim() != uri {
        return Err("malformed document URI".into());
    }
    let rest = uri.strip_prefix("file://").ok_or("not a file URI")?;
    let encoded = if rest.starts_with('/') {
        rest
    } else {
        rest.strip_prefix("localhost")
            .filter(|path| path.starts_with('/'))
            .ok_or("the file URI names another host")?
    };
    let bytes: Vec<u8> = percent_encoding::percent_decode_str(encoded).collect();
    if bytes.contains(&0) {
        return Err("the file URI holds a NUL".into());
    }
    let path = PathBuf::from(std::ffi::OsString::from_vec(bytes));
    if path.components().any(|part| part == Component::ParentDir) {
        return Err("the file URI climbs out of its directory".into());
    }
    let root = root
        .canonicalize()
        .map_err(|_| "the document portal is not mounted")?;
    let canonical = path
        .canonicalize()
        .map_err(|_| "the exported file is gone")?;
    if !canonical.starts_with(&root) || !canonical.is_file() {
        return Err("not a document-portal export".into());
    }
    Ok(canonical)
}

/// Read at most `max` bytes of the exported file `uri`.
///
/// # Errors
/// It is no export ([`exported_file`]), is larger, or could not be read.
pub fn read_exported(uri: &str, max: u64) -> Result<Vec<u8>, String> {
    use std::io::Read as _;
    let path = exported_file(uri)?;
    let mut bytes = Vec::new();
    std::fs::File::open(&path)
        .and_then(|file| file.take(max.saturating_add(1)).read_to_end(&mut bytes))
        .map_err(|error| error.to_string())?;
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > max {
        return Err("the file is too large".into());
    }
    Ok(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_files_below_the_mount_resolve() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("doc");
        let inside = root.join("ab12");
        std::fs::create_dir_all(&inside).unwrap();
        std::fs::write(inside.join("my picture.png"), b"x").unwrap();
        std::fs::write(dir.path().join("secret"), b"x").unwrap();
        std::os::unix::fs::symlink(dir.path().join("secret"), inside.join("link")).unwrap();
        let uri = |path: &Path| format!("file://{}", path.display()).replace(' ', "%20");

        assert!(exported_file_below(&uri(&inside.join("my picture.png")), &root).is_ok());
        assert!(
            exported_file_below(&uri(&dir.path().join("secret")), &root).is_err(),
            "a host file"
        );
        assert!(
            exported_file_below(&uri(&inside.join("link")), &root).is_err(),
            "a link out"
        );
        assert!(
            exported_file_below(&format!("{}/../../secret", uri(&inside)), &root).is_err(),
            "climbing out"
        );
        assert!(exported_file_below("file://example.org/doc/x", &root).is_err());
        assert!(exported_file_below("https://example.org/x", &root).is_err());
    }
}
