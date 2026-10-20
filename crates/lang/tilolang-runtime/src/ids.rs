//! Identifier types shared by the whole runtime.
//!
//! Declarations, fields, documents and runs are all named by plain strings: the ids the
//! compiler assigned, or ids this runtime generated. Principals and module hashes are
//! newtypes so a hash can never be passed where a person is expected.

use std::fmt;

use serde::{Deserialize, Serialize};

pub type AppId = String;
pub type TypeId = String;
pub type FieldId = String;
pub type ActionId = String;
pub type ViewId = String;
pub type FlowId = String;
pub type ItemId = String;
pub type DocId = String;
pub type RunId = String;

/// A person, an agent task or an app on whose behalf code runs.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Principal(pub String);

impl Principal {
    pub fn new(name: impl Into<String>) -> Self {
        Self(name.into())
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

impl fmt::Display for ModuleHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A loaded package is identified by the hash of its module.
pub type PackageId = ModuleHash;
