//! tilo-extension: downloaded, Tilo-signed system extensions.
//!
//! Some optional `/usr` content is not part of the Tilo image because Tilo does
//! not distribute it (patent-encumbered media codecs are the first case). When
//! the owner asks for it, this helper downloads the bytes on the device from the
//! publishers' own servers and assembles read-only systemd-sysext directories.
//!
//! The image decides which bytes are acceptable: its catalogue entry names the
//! parts and the hosts artifacts may come from, and where the extension's
//! manifest is published. The manifest pins every artifact by URL, length and
//! SHA-256 and names the exact base it was resolved for, and the subcommands
//! are `install:<id>`, `remove:<id>`, `refresh:<id>`, `refresh:all`,
//! `status:<id>`, `activate` and the `live:` commands ([`live`]).

mod host;
pub mod live;
mod machine;
mod model;
pub mod py;

pub use host::{Header, Host, HttpError, Response, System};
pub use machine::{Layout, activate, main};
pub use model::{
    Catalogue, MANIFEST_TYPE, Manifest, VERBS, base_packages_sha256, extension_release,
    parse_command, parse_os_release,
};

use std::fmt;
use std::io;
use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    Pack(String),
    Unpublished(String),
    Os(String),
    Value(String),
}

impl Error {
    pub fn pack(message: impl Into<String>) -> Self {
        Self::Pack(message.into())
    }

    pub fn os(error: &io::Error, path: Option<&Path>) -> Self {
        Self::Os(os_message(error, path))
    }

    pub fn is_pack_or_os(&self) -> bool {
        !matches!(self, Self::Value(_))
    }
}

pub fn os_message(error: &io::Error, path: Option<&Path>) -> String {
    let Some(code) = error.raw_os_error() else {
        return error.to_string();
    };
    let reason = io::Error::from_raw_os_error(code).to_string();
    let reason = reason
        .split(" (os error")
        .next()
        .unwrap_or(&reason)
        .to_owned();
    match path {
        Some(path) => format!(
            "[Errno {code}] {reason}: {}",
            py::repr_str(&path.to_string_lossy())
        ),
        None => format!("[Errno {code}] {reason}"),
    }
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Pack(message)
            | Self::Unpublished(message)
            | Self::Os(message)
            | Self::Value(message) => f.write_str(message),
        }
    }
}

impl std::error::Error for Error {}

pub type Result<T, E = Error> = std::result::Result<T, E>;

pub(crate) trait At<T> {
    fn at(self, path: &Path) -> Result<T>;
    fn os(self) -> Result<T>;
}

impl<T> At<T> for io::Result<T> {
    fn at(self, path: &Path) -> Result<T> {
        self.map_err(|error| Error::os(&error, Some(path)))
    }

    fn os(self) -> Result<T> {
        self.map_err(|error| Error::os(&error, None))
    }
}
