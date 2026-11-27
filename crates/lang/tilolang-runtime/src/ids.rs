//! Identifier types shared by the whole runtime.
//!
//! Declarations, fields, documents and runs are all named by plain strings: the ids the
//! compiler assigned, or ids this runtime generated. Who acted is the shared [`Actor`], and a
//! module hash a newtype, so a hash can never be passed where a person is expected.

use std::fmt;

use serde::{Deserialize, Serialize};

pub use tilo_member_types::AppId;
pub type TypeId = String;
pub type FieldId = String;
pub type ActionId = String;
pub type ViewId = String;
pub type ItemId = String;
pub type DocId = String;
pub type RunId = String;

/// Who code runs as and who made a change: a member, an automation, or an app acting by itself.
pub use tilo_actions::{Actor, MemberId};

const WRITTEN_BY: &str = "by:";

/// `actor` as text, where only text is kept: a set's element, a map's key, a window's control.
#[must_use]
pub fn actor_text(actor: &Actor) -> String {
    serde_json::to_string(actor).unwrap_or_default()
}

/// The actor [`actor_text`] wrote.
#[must_use]
pub fn actor_of_text(text: &str) -> Option<Actor> {
    serde_json::from_str(text).ok()
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
    /// compiled package: the actor, tagged so it can never equal a module hash.
    /// An embedded block runs under it (`embed`), so the authority sees
    /// "this code is the agent's" and a grant for the actor covers every block it wrote.
    pub fn written_by(actor: &Actor) -> Self {
        Self(format!("{WRITTEN_BY}{}", actor_text(actor)))
    }

    /// The actor whose code this is, when [`Self::written_by`] made it.
    #[must_use]
    pub fn writer(&self) -> Option<Actor> {
        actor_of_text(self.0.strip_prefix(WRITTEN_BY)?)
    }
}

impl fmt::Display for ModuleHash {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// A loaded package is identified by the hash of its module.
pub type PackageId = ModuleHash;
