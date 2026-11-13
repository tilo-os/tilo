//! Identifier types shared by the whole runtime.
//!
//! Declarations, fields, documents and runs are all named by plain strings: the ids the
//! compiler assigned, or ids this runtime generated. Principals and module hashes are
//! newtypes so a hash can never be passed where a person is expected.

use std::fmt;

use serde::{Deserialize, Serialize};

pub use tilo_app_id::AppId;
pub type TypeId = String;
pub type FieldId = String;
pub type ActionId = String;
pub type ViewId = String;
pub type ItemId = String;
pub type DocId = String;
pub type RunId = String;

/// A person, an agent task or an app on whose behalf code runs.
///
/// Its id names it in every record and grant. Its name is what a person reads where it stands
/// (an avatar's initials, a byline, a quote's "In reply to"), which a record does not keep and
/// the host knows; a principal a record holds carries the id alone, and the host names it there
/// ([`crate::core::NameHost`]).
#[derive(Clone, Debug, Default, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Principal(pub String, #[serde(default)] pub String);

impl Principal {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into(), String::new())
    }

    /// It, called `name` where a person reads it.
    #[must_use]
    pub fn named(self, name: impl Into<String>) -> Self {
        Self(self.0, name.into())
    }

    /// What to show for it: the name it carries, else its id.
    #[must_use]
    pub fn shown(&self) -> String {
        if self.1.trim().is_empty() {
            self.0.clone()
        } else {
            self.1.clone()
        }
    }
}

impl fmt::Display for Principal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// The content hash of a compiled module: the identity of a code author.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModuleHash(pub String);

impl ModuleHash {
    pub fn of_bytes(bytes: &[u8]) -> Self {
        Self(blake3::hash(bytes).to_hex().to_string())
    }
}

impl ModuleHash {
    /// The authorship identity of code written by a person or an agent task rather than by a
    /// compiled package: the principal's id, tagged so it can never equal a module hash.
    /// An embedded block runs under it (`embed`), so the authority sees
    /// "this code is the agent's" and a grant for the principal covers every block it wrote.
    pub fn principal(id: &str) -> Self {
        Self(format!("principal:{id}"))
    }
}

impl fmt::Display for ModuleHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A loaded package is identified by the hash of its module.
pub type PackageId = ModuleHash;
