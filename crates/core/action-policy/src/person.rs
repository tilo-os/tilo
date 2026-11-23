//! A person, by their root public key: the one id every resource, roster and
//! grant spells the same way.
//!
//! A person's id is the lowercase hex of their root public key and nothing
//! else (the empty id is the nobody the action model carries for software
//! holding no person's authority). Kinds of member that are not people carry a
//! prefix that says so (`agent:<id>`, `bot:<id>`, `automation:<id>`), so a
//! person needs none: a bare id is a person's, and an id carrying a prefix
//! names software. Storing `person:<root>` beside the bare root made one
//! person two strings, and every comparison, roster row and presence entry had
//! to guess which spelling it was looking at. [`PersonId`] refuses that
//! spelling when it is parsed.

use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A person's id: the lowercase hex of their root public key, bare.
///
/// Built from a root key ([`PersonId::from_root`]) or parsed from a stored id
/// ([`PersonId::parse`], which is also what its serde reader uses). A string
/// carrying a member prefix names software, never a person.
#[derive(Clone, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct PersonId(String);

/// Why an id is not a person's.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PersonIdError {
    /// An id that carries a prefix names software: `person:<root>` is the
    /// prefixed spelling of a person, and is refused like any other.
    Prefixed,
}

impl std::fmt::Display for PersonIdError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("a person's id is their bare root key, with no prefix")
    }
}

impl std::error::Error for PersonIdError {}

impl PersonId {
    /// `id` as a person's, when it is their bare root key. Empty is nobody:
    /// the action model carries it for software holding no person's authority.
    ///
    /// # Errors
    /// An id carrying a `:` (the prefix a member's kind is written with)
    /// names software, never a person.
    pub fn parse(id: &str) -> Result<Self, PersonIdError> {
        if id.contains(':') {
            return Err(PersonIdError::Prefixed);
        }
        Ok(Self(id.to_owned()))
    }

    /// The root public key `root`, as the id every resource spells.
    #[must_use]
    pub fn from_root(root: [u8; 32]) -> Self {
        Self(hex::encode(root))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// The root public key, when the id is one: 32 bytes of hex.
    #[must_use]
    pub fn root(&self) -> Option<[u8; 32]> {
        let mut root = [0; 32];
        hex::decode_to_slice(&self.0, &mut root).ok()?;
        Some(root)
    }

    /// `id` as a person's, for an id this device minted or a fixture names:
    /// a prefixed id is a mistake in this program, not in data.
    ///
    /// # Panics
    /// When `id` is not a person's id ([`PersonId::parse`] refuses it).
    #[must_use]
    #[track_caller]
    #[allow(
        clippy::expect_used,
        reason = "an id this program mints or a fixture names: a prefixed one is a mistake here, not in data"
    )]
    pub fn expect(id: impl AsRef<str>) -> Self {
        Self::parse(id.as_ref()).expect("a person's id is their bare root key")
    }
}

impl From<String> for PersonId {
    #[track_caller]
    fn from(id: String) -> Self {
        Self::expect(id)
    }
}

impl From<&str> for PersonId {
    #[track_caller]
    fn from(id: &str) -> Self {
        Self::expect(id)
    }
}

impl From<&String> for PersonId {
    #[track_caller]
    fn from(id: &String) -> Self {
        Self::expect(id)
    }
}

impl From<&PersonId> for PersonId {
    fn from(id: &PersonId) -> Self {
        id.clone()
    }
}

impl std::ops::Deref for PersonId {
    type Target = str;

    fn deref(&self) -> &str {
        &self.0
    }
}

impl AsRef<str> for PersonId {
    fn as_ref(&self) -> &str {
        &self.0
    }
}

impl std::borrow::Borrow<str> for PersonId {
    fn borrow(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Display for PersonId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A person's id compares as the string it is, so a caller holding a stored id
/// reads a roster or a grant without unwrapping one.
impl PartialEq<str> for PersonId {
    fn eq(&self, other: &str) -> bool {
        self.0 == other
    }
}

impl PartialEq<&str> for PersonId {
    fn eq(&self, other: &&str) -> bool {
        self.0 == *other
    }
}

impl PartialEq<String> for PersonId {
    fn eq(&self, other: &String) -> bool {
        &self.0 == other
    }
}

/// Written and read as the bare root key: an id that carries a prefix fails to
/// read as a person.
impl Serialize for PersonId {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for PersonId {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let id = String::deserialize(deserializer)?;
        Self::parse(&id).map_err(D::Error::custom)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    tilo_test_env::scenario! {
        /// A person's id is their bare root key, and a prefixed one is no person.
        fn a_person_id_is_its_bare_root_key;

        phase a_bare_root_reads_back_as_the_same_person;
        {
            let id = PersonId::from_root([7; 32]);
            assert_eq!(id.as_str().len(), 64);
            assert_eq!(id.root(), Some([7; 32]));
            assert_eq!(PersonId::parse(id.as_str()).unwrap(), id);
            let json = serde_json::to_string(&id).unwrap();
            assert_eq!(json, format!("\"{id}\""), "written bare");
            assert_eq!(serde_json::from_str::<PersonId>(&json).unwrap(), id);
        }

        phase a_prefixed_id_names_no_person;
        {
            assert_eq!(
                PersonId::parse("person:7a7a"),
                Err(PersonIdError::Prefixed),
                "the prefixed spelling is not a person"
            );
            assert_eq!(PersonId::parse("agent:helper"), Err(PersonIdError::Prefixed));
            assert!(
                serde_json::from_str::<PersonId>("\"person:7a\"").is_err(),
                "a resource carrying the prefixed spelling does not read as a person"
            );
        }
    }
}
