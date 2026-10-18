//! tilo-extension: downloaded, Tilo-signed system extensions.
//!
//! ```text
//! tilo-extension install:<id>   opt in, then download, verify and merge
//! tilo-extension remove:<id>    opt out: unmerge and delete everything added
//! tilo-extension refresh:<id>   re-resolve for the booted base (no-op unless
//!                               the owner opted in)
//! tilo-extension refresh:all    refresh every extension the owner opted in to
//! tilo-extension status:<id>    print the status JSON
//! tilo-extension activate       rebuild every part from verified bytes for the
//!                               booted base and merge exactly those (at boot,
//!                               offline, and after every change)
//! tilo-extension live:system    apply the staged system image's components
//!                               that need no desktop restart ([`live`])
//! tilo-extension live:session   apply the rest and restart the desktops in
//!                               place
//! tilo-extension live:status    print the live update's state
//! ```
//!
//! Some optional /usr content is not part of the Tilo image because Tilo does
//! not distribute it (patent-encumbered media codecs are the first case). When
//! the owner asks for it, this helper downloads the bytes on the device, from
//! the publishers' own servers, and assembles read-only systemd-sysext
//! directories.
//!
//! The image decides which bytes are acceptable. Its catalogue entry
//! (/usr/share/tilo/system-extensions/sysext.d/<id>.json, part of the sealed
//! image) names the parts and the hosts artifacts may come from, and where Tilo
//! publishes the extension's manifest. The manifest pins every artifact by URL,
//! length and SHA-256 and names the exact base it was resolved for; it is
//! signed by Tilo's extension-manifest key. An image carrying Tilo's offline
//! release root (/usr/share/tilo/release/root.crt) accepts a manifest only with
//! a Rekor bundle (`manifest.json.bundle`): the signature, an operational
//! certificate the root issued for extension manifests in the current epoch,
//! and the transparency-log entry dating it. An image without the root checks
//! the bare signature (`manifest.json.sig`) against the key it carries
//! (/usr/share/tilo/extensions.pub). No other key, not even the OS update key,
//! can sign a manifest this helper accepts. The signature is checked over the
//! raw bytes before anything is parsed, and any mismatch refuses the whole
//! transaction. Nothing from the manifest is ever run.
//!
//! Exit status: 0 done, 1 failed, 2 usage (or not root). See
//! /usr/share/tilo/system-extensions/README.md.
//!
//! Everything the helper asks of the machine (the rpm database, the network,
//! the signature check, rpm2archive, systemd-sysext) goes through [`Host`], so
//! the tests drive the whole state machine against a temporary root.

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

/// A failure, by the kind of exception the original raised, because which
/// callers catch it depends on that kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error {
    /// A failure the status file reports to the owner.
    Pack(String),
    /// No manifest is published for this base (yet). A `Pack` failure too.
    Unpublished(String),
    /// The operating system refused something.
    Os(String),
    /// A value had the wrong form (`ValueError`); activation does not catch it.
    Value(String),
}

impl Error {
    /// A `Pack` failure with `message`.
    pub fn pack(message: impl Into<String>) -> Self {
        Self::Pack(message.into())
    }

    /// An operating-system error, worded as the journal and the status file
    /// have always shown it: `[Errno N] reason: 'path'`.
    pub fn os(error: &io::Error, path: Option<&Path>) -> Self {
        Self::Os(os_message(error, path))
    }

    /// Whether `except (PackError, OSError)` catches it.
    pub fn is_pack_or_os(&self) -> bool {
        !matches!(self, Self::Value(_))
    }
}

/// `str(OSError)`.
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

/// The helper's result type.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Attach a path to an I/O error, as the original's messages did.
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
