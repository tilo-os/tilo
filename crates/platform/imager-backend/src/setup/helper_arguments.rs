//! The argv surface a setup helper runs with: one value for each of the seven
//! keys, each key at most once, and nothing else.
use std::collections::BTreeMap;
use std::ffi::OsString;
use std::io;

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
pub(crate) fn parse(
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
