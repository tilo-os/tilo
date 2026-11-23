use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::os::unix::ffi::{OsStrExt as _, OsStringExt as _};

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InvalidEntryName;

impl std::fmt::Display for InvalidEntryName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("invalid native entry name")
    }
}
impl std::error::Error for InvalidEntryName {}

/// Exactly one native component. Lossy display labels must not be used as identity.
///
/// On the wire a name is the daemon's bytes for it. Unix names are any bytes; a Windows device
/// names a file in Unicode, so a name there is one whose bytes are UTF-8, and one that is not
/// cannot be named on it.
#[derive(Clone, Debug, Eq, PartialEq, Hash)]
pub struct EntryName(OsString);

impl serde::Serialize for EntryName {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_bytes(bytes(&self.0))
    }
}

impl<'de> serde::Deserialize<'de> for EntryName {
    fn deserialize<D: serde::Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let bytes = <Vec<u8> as serde::Deserialize>::deserialize(deserializer)?;
        #[cfg(unix)]
        let name = OsString::from_vec(bytes);
        #[cfg(not(unix))]
        let name = OsString::from(String::from_utf8(bytes).map_err(serde::de::Error::custom)?);
        Self::new(name).map_err(serde::de::Error::custom)
    }
}

impl EntryName {
    pub fn new(value: OsString) -> Result<Self, InvalidEntryName> {
        let bytes = bytes(&value);
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

/// The bytes `name` is on the wire: its own on Unix, its UTF-8 elsewhere. A name that is not
/// Unicode has no bytes there, which [`EntryName::new`] refuses as empty.
fn bytes(name: &OsStr) -> &[u8] {
    #[cfg(unix)]
    {
        name.as_bytes()
    }
    #[cfg(not(unix))]
    {
        name.to_str().map_or(&[], str::as_bytes)
    }
}
