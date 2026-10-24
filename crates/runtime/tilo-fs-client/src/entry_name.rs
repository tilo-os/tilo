use std::ffi::{OsStr, OsString};
use std::os::unix::ffi::OsStrExt;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidEntryName;

impl std::fmt::Display for InvalidEntryName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid native entry name")
    }
}
impl std::error::Error for InvalidEntryName {}

/// Exactly one native component. Lossy display labels must not be used as identity.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EntryName(OsString);

impl EntryName {
    pub fn new(value: OsString) -> Result<Self, InvalidEntryName> {
        let bytes = value.as_bytes();
        if bytes.is_empty()
            || bytes == b"."
            || bytes == b".."
            || bytes.contains(&b'/')
            || bytes.contains(&0)
        {
            return Err(InvalidEntryName);
        }
        Ok(Self(value))
    }

    pub fn as_os_str(&self) -> &OsStr {
        &self.0
    }
}
