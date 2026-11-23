//! The argv surface a setup helper runs with: one value for each of the seven
//! keys, each key at most once, and nothing else.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;
use std::path::{Component, PathBuf};

/// Every key a setup helper accepts.
pub(crate) const KEYS: [&str; 7] = [
    "--action",
    "--home",
    "--package",
    "--receipt",
    "--setup-package",
    "--setup-quarantine",
    "--workspace",
];

/// Parse the whole argv into its key/value pairs, or refuse the run.
pub fn parse(
    args: impl IntoIterator<Item = OsString>,
    failure: impl Fn() -> io::Error,
) -> io::Result<BTreeMap<String, OsString>> {
    let mut args = args.into_iter();
    let mut values = BTreeMap::new();
    while let Some(key) = args.next() {
        let key = key.into_string().map_err(|_| failure())?;
        if !KEYS.contains(&key.as_str()) || values.contains_key(&key) {
            return Err(failure());
        }
        values.insert(key, args.next().ok_or_else(&failure)?);
    }
    if values.len() != KEYS.len() {
        return Err(failure());
    }
    Ok(values)
}

/// Check that a helper path argument is absolute and has no `..` or `.`
/// components.
pub fn strict_path(
    value: impl Into<PathBuf>,
    failure: impl Fn() -> io::Error,
) -> io::Result<PathBuf> {
    let path = value.into();
    if !path.is_absolute()
        || path
            .components()
            .any(|part| matches!(part, Component::ParentDir | Component::CurDir))
    {
        return Err(failure());
    }
    Ok(path)
}

/// Look up a required helper path argument and apply [`strict_path`].
pub fn path(
    values: &BTreeMap<String, OsString>,
    key: &str,
    failure: impl Fn() -> io::Error,
) -> io::Result<PathBuf> {
    strict_path(values.get(key).ok_or_else(&failure)?, failure)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn failure() -> io::Error {
        io::Error::other("refused")
    }

    #[test]
    fn strict_path_refuses_relative_and_parent_dir_paths() {
        assert_eq!(
            strict_path("/var/lib/tilo", failure).unwrap(),
            PathBuf::from("/var/lib/tilo")
        );
        // Path::components() drops interior `.` after the root; keep what the
        // strict helpers accept today.
        assert_eq!(
            strict_path("/var/./lib/tilo", failure).unwrap(),
            PathBuf::from("/var/./lib/tilo")
        );
        for bad in [
            "relative/path",
            "./relative",
            "../relative",
            "/var/../lib",
            "/var/lib/..",
            "/..",
        ] {
            let error = strict_path(bad, failure).expect_err(bad);
            assert_eq!(error.kind(), io::ErrorKind::Other);
            assert_eq!(error.to_string(), "refused");
        }
    }
}
