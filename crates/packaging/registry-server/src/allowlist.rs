//! Owner-controlled publisher allowlist — the `publishers.toml`
//! file the registry owner edits (via `tilo-registry-server
//! add-publisher` or by hand) to decide who can post entries.
//!
//! ```toml
//! [[publisher]]
//! pubkey_hex = "aabbcc..."   # 32-byte ed25519 pubkey, hex
//! can_add_new = true         # if false, publisher can only edit
//!                            # / delete entries whose stored
//!                            # publisher_pubkey_hex matches theirs
//! note = "acme software"     # free-form; not signed
//! namespaces = ["com.acme"]  # verified reverse-DNS prefixes its package
//!                            # ids must start with; owns nothing if absent
//! ```

use serde::{Deserialize, Serialize};
use thiserror::Error;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherRow {
    /// Ed25519 pubkey, hex encoded.
    pub pubkey_hex: String,
    /// Whether this publisher is allowed to POST new entries. If
    /// `false`, they can still update or delete entries whose
    /// stored `publisher_pubkey_hex` matches theirs.
    #[serde(default)]
    pub can_add_new: bool,
    /// Free-form comment for the operator's own bookkeeping. Not
    /// covered by any signature.
    #[serde(default)]
    pub note: String,
    /// Verified namespaces (`registry_types::namespace`) this publisher's
    /// package ids must lie in. An absent or empty list owns nothing:
    /// granting `so.tilo` is an explicit entry, never a default.
    #[serde(default)]
    pub namespaces: Vec<String>,
}

impl PublisherRow {
    /// Whether this publisher may name a package `package_id`.
    pub fn owns_package_id(&self, package_id: &str) -> bool {
        self.namespaces
            .iter()
            .any(|namespace| registry_types::namespace::namespace_owns(namespace, package_id))
    }

    /// Parse the hex pubkey into raw bytes.
    ///
    /// # Errors
    ///
    /// Returns [`AllowlistError::BadHex`] if `pubkey_hex` isn't valid
    /// hex, or [`AllowlistError::BadLength`] if it doesn't decode to
    /// exactly 32 bytes.
    pub fn pubkey_bytes(&self) -> Result<[u8; 32], AllowlistError> {
        parse_pubkey_hex(&self.pubkey_hex)
    }
}

/// The full `publishers.toml`.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherAllowlist {
    #[serde(default, rename = "publisher")]
    pub publishers: Vec<PublisherRow>,
}

impl PublisherAllowlist {
    /// Empty allowlist — nobody can post.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Parse from TOML source.
    ///
    /// # Errors
    ///
    /// Returns [`AllowlistError::Parse`] if `s` isn't valid TOML for this shape.
    pub fn from_toml(s: &str) -> Result<Self, AllowlistError> {
        toml::from_str(s).map_err(|e| AllowlistError::Parse(e.to_string()))
    }

    /// Encode to pretty TOML for `atomic_write`.
    ///
    /// # Errors
    ///
    /// Returns [`AllowlistError::Encode`] if `self` fails to encode as TOML.
    pub fn to_toml(&self) -> Result<String, AllowlistError> {
        toml::to_string_pretty(self).map_err(|e| AllowlistError::Encode(e.to_string()))
    }

    /// Look up a publisher by raw pubkey bytes.
    pub fn get(&self, pubkey: &[u8; 32]) -> Option<&PublisherRow> {
        self.publishers
            .iter()
            .find(|p| p.pubkey_bytes().is_ok_and(|b| &b == pubkey))
    }

    /// Look up a publisher by its hex pubkey, as `add-publisher --grant` names
    /// the row it extends.
    pub fn row(&self, pubkey_hex: &str) -> Option<&PublisherRow> {
        self.publishers
            .iter()
            .find(|p| p.pubkey_hex.eq_ignore_ascii_case(pubkey_hex))
    }

    /// Rows that own no namespace and so can submit nothing but deletes.
    /// Rows written before namespaces existed land here until the operator
    /// names one with `add-publisher --namespace`.
    pub fn rows_without_namespaces(&self) -> impl Iterator<Item = &PublisherRow> {
        self.publishers
            .iter()
            .filter(|row| row.namespaces.is_empty())
    }

    /// Log a warning naming every row in [`Self::rows_without_namespaces`].
    pub fn warn_rows_without_namespaces(&self) {
        for row in self.rows_without_namespaces() {
            tracing::warn!(
                "publisher {} owns no namespace; it can submit nothing until `add-publisher --namespace …` names one",
                row.pubkey_hex
            );
        }
    }

    /// The set of pubkeys allowed to authenticate any change
    /// request — used to filter [`registry_types::verify_change_request`].
    pub fn all_pubkey_bytes(&self) -> Vec<[u8; 32]> {
        self.publishers
            .iter()
            .filter_map(|p| p.pubkey_bytes().ok())
            .collect()
    }

    /// Insert a publisher, replacing any existing row with the
    /// same pubkey. Returns `true` if a row was replaced.
    pub fn upsert(&mut self, row: PublisherRow) -> bool {
        if let Some(existing) = self
            .publishers
            .iter_mut()
            .find(|p| p.pubkey_hex.eq_ignore_ascii_case(&row.pubkey_hex))
        {
            *existing = row;
            return true;
        }
        self.publishers.push(row);
        false
    }

    /// Remove a publisher by pubkey. Returns `true` if a row was
    /// removed.
    pub fn remove(&mut self, pubkey_hex: &str) -> bool {
        let before = self.publishers.len();
        self.publishers
            .retain(|p| !p.pubkey_hex.eq_ignore_ascii_case(pubkey_hex));
        self.publishers.len() != before
    }
}

fn parse_pubkey_hex(s: &str) -> Result<[u8; 32], AllowlistError> {
    let bytes = hex::decode(s).map_err(|e| AllowlistError::BadHex(e.to_string()))?;
    if bytes.len() != 32 {
        return Err(AllowlistError::BadLength { got: bytes.len() });
    }
    let mut out = [0u8; 32];
    out.copy_from_slice(&bytes);
    Ok(out)
}

#[derive(Debug, Error)]
pub enum AllowlistError {
    #[error("TOML parse: {0}")]
    Parse(String),
    #[error("TOML encode: {0}")]
    Encode(String),
    #[error("hex decode: {0}")]
    BadHex(String),
    #[error("pubkey length {got}, expected 32")]
    BadLength { got: usize },
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allowlist_round_trips_upserts_and_looks_up_publishers() {
        // phase: empty_toml_roundtrip
        {
            let empty = PublisherAllowlist::empty();
            let s = empty.to_toml().unwrap();
            let back = PublisherAllowlist::from_toml(&s).unwrap();
            assert_eq!(back, empty);
        }
        // phase: single_publisher_roundtrip
        {
            let mut al = PublisherAllowlist::empty();
            al.upsert(PublisherRow {
                pubkey_hex: "aa".repeat(32),
                can_add_new: true,
                note: "acme".into(),
                namespaces: vec!["com.acme".into()],
            });
            let s = al.to_toml().unwrap();
            let back = PublisherAllowlist::from_toml(&s).unwrap();
            assert_eq!(back, al);
        }
        // phase: upsert_replaces_existing
        {
            let mut al = PublisherAllowlist::empty();
            assert!(!al.upsert(PublisherRow {
                pubkey_hex: "aa".repeat(32),
                can_add_new: false,
                note: String::new(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }));
            assert!(al.upsert(PublisherRow {
                pubkey_hex: "aa".repeat(32),
                can_add_new: true,
                note: "later".into(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            }));
            assert_eq!(al.publishers.len(), 1);
            assert!(al.publishers[0].can_add_new);
            assert_eq!(al.publishers[0].note, "later");
        }
        // phase: get_finds_by_raw_bytes
        {
            let mut al = PublisherAllowlist::empty();
            al.upsert(PublisherRow {
                pubkey_hex: "de".repeat(32),
                can_add_new: true,
                note: String::new(),
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
            });
            assert!(al.get(&[0xde; 32]).is_some());
            assert!(al.get(&[0x00; 32]).is_none());
        }
        // phase: rows_without_namespaces_own_nothing
        {
            let al = PublisherAllowlist::from_toml(&format!(
                "[[publisher]]\npubkey_hex = \"{}\"\ncan_add_new = true\n",
                "ab".repeat(32)
            ))
            .unwrap();
            let row = &al.publishers[0];
            assert!(row.namespaces.is_empty());
            assert!(!row.owns_package_id("so.tilo.notes"));
            assert!(!row.owns_package_id("so.tilo.store"));
            assert!(!row.owns_package_id("com.acme.notes"));
            assert_eq!(al.rows_without_namespaces().collect::<Vec<_>>(), [row]);
            let tilo = PublisherRow {
                namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                ..row.clone()
            };
            assert!(tilo.owns_package_id("so.tilo.notes"));
            assert!(!tilo.owns_package_id("com.acme.notes"));
            let acme = PublisherRow {
                namespaces: vec!["com.acme".into(), "io.github.acme".into()],
                ..row.clone()
            };
            assert!(acme.owns_package_id("io.github.acme.notes"));
            assert!(!acme.owns_package_id("so.tilo.notes"));
            assert!(!acme.owns_package_id("com.acmes.notes"));
            let explicit = PublisherAllowlist {
                publishers: vec![tilo, acme],
            };
            assert_eq!(explicit.rows_without_namespaces().count(), 0);
        }
        // phase: all_pubkey_bytes_skips_bad_hex
        {
            // If a hand-edited toml has junk in one row, the good rows
            // still surface — matches the "prefer availability" bias
            // for a public registry.
            let al = PublisherAllowlist {
                publishers: vec![
                    PublisherRow {
                        pubkey_hex: "zzzz".into(),
                        can_add_new: false,
                        note: String::new(),
                        namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                    },
                    PublisherRow {
                        pubkey_hex: "11".repeat(32),
                        can_add_new: true,
                        note: String::new(),
                        namespaces: vec![registry_types::namespace::TILO_NAMESPACE.to_owned()],
                    },
                ],
            };
            let keys = al.all_pubkey_bytes();
            assert_eq!(keys, vec![[0x11; 32]]);
        }
    }
}
