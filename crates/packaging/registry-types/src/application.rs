//! A publisher's signed application to publish under verified namespaces.
//!
//! The applicant signs a [`PublisherApplication`] with the key it will
//! publish with, naming each namespace it claims and how ownership is proven
//! ([`NamespaceClaim`]). Every proof is the same line, [`proof_token`], placed
//! where only the namespace owner can put it:
//!
//! - a domain: a DNS TXT record at [`dns_txt_name`], or, as a fallback, the
//!   file at [`well_known_url`];
//! - a forge account: `tilo-publisher.txt` on the default branch of the
//!   account's public repository `tilo-publisher` ([`forge_proof_url`]).
//!
//! The registry checks the proofs when the application arrives and again when
//! its owner approves it, then allowlists the key for exactly those
//! namespaces.

use ed25519_dalek::{Signature, Signer as _, SigningKey, Verifier as _, VerifyingKey};
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::namespace::{Forge, domain_namespace};

/// Domain separator of an application signature.
pub const APPLICATION_DOMAIN: &[u8] = b"tilo-publisher-application-v1\0";

/// Most namespaces one application may claim.
pub const MAX_CLAIMS: usize = 8;

/// Name of the forge repository and file that carry a forge proof.
pub const FORGE_PROOF_REPOSITORY: &str = "tilo-publisher";
pub const FORGE_PROOF_FILE: &str = "tilo-publisher.txt";

/// How an applicant proves it owns a namespace.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "kebab-case")]
pub enum NamespaceClaim {
    /// A domain; proves its reversed labels (`example.com` → `com.example`).
    Domain { domain: String },
    /// An account on a forge (`github`, `codeberg`, or a Forgejo/Gitea host).
    Forge { forge: String, account: String },
}

impl NamespaceClaim {
    /// The namespace this claim proves, or `None` for a malformed claim.
    #[must_use]
    pub fn namespace(&self) -> Option<String> {
        match self {
            Self::Domain { domain } => domain_namespace(domain),
            Self::Forge { forge, account } => Forge::parse(forge)?.namespace(account),
        }
    }
}

/// How a namespace was proven.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ProofMethod {
    DnsTxt,
    WellKnown,
    Forge,
}

/// One namespace the registry saw proven, and where.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerifiedClaim {
    pub namespace: String,
    pub method: ProofMethod,
    pub location: String,
    pub checked_at_unix: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ApplicationStatus {
    Pending,
    Approved,
    Rejected,
}

/// The operator-local application review snapshot. Public status replies
/// omit its contact address and note.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct StoredApplication {
    pub application: PublisherApplication,
    pub verified: Vec<VerifiedClaim>,
    pub status: ApplicationStatus,
    pub received_at_unix: u64,
    #[serde(default)]
    pub decided_at_unix: Option<u64>,
    #[serde(default)]
    pub reviewer: Option<String>,
    #[serde(default)]
    pub reason: Option<String>,
}

impl StoredApplication {
    /// The exact stored snapshot an owner inspected, for guarded CLI or UI
    /// decisions. A replacement, proof refresh or decision changes this id.
    ///
    /// # Errors
    /// Returns an error if the snapshot cannot be serialized.
    pub fn review_id(&self) -> Result<String, serde_json::Error> {
        serde_json::to_vec(self).map(|bytes| blake3::hash(&bytes).to_hex().to_string())
    }

    /// The namespaces the application proved.
    #[must_use]
    pub fn namespaces(&self) -> Vec<String> {
        self.verified
            .iter()
            .map(|claim| claim.namespace.clone())
            .collect()
    }
}

/// A signed request to be allowlisted for some namespaces.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublisherApplication {
    /// The Ed25519 key that will sign packages and entries, hex.
    pub pubkey_hex: String,
    pub claims: Vec<NamespaceClaim>,
    /// How the registry owner can reach the applicant.
    pub contact: String,
    /// What the applicant intends to publish.
    #[serde(default)]
    pub note: String,
    pub submitted_at_unix: u64,
    /// Signature over [`PublisherApplication::signing_message`], hex.
    #[serde(default)]
    pub signature_hex: String,
}

#[derive(Debug, Error, PartialEq, Eq)]
pub enum ApplicationError {
    #[error("the publisher key is not a valid Ed25519 public key")]
    BadKey,
    #[error("the application signature does not verify")]
    BadSignature,
    #[error("an application claims between 1 and {MAX_CLAIMS} namespaces")]
    ClaimCount,
    #[error("claim {0:?} does not name a valid namespace")]
    BadClaim(NamespaceClaim),
    #[error("contact must be 1-256 characters and the note at most 2048")]
    BadText,
}

impl PublisherApplication {
    /// The bytes the signature covers: the domain and the application's JSON
    /// with an empty signature.
    #[must_use]
    pub fn signing_message(&self) -> Vec<u8> {
        let unsigned = Self {
            signature_hex: String::new(),
            ..self.clone()
        };
        let mut message = APPLICATION_DOMAIN.to_vec();
        message.extend(serde_json::to_vec(&unsigned).unwrap_or_default());
        message
    }

    /// Set `pubkey_hex` to `key`'s and sign.
    pub fn sign(&mut self, key: &SigningKey) {
        self.pubkey_hex = hex::encode(key.verifying_key().to_bytes());
        self.signature_hex = hex::encode(key.sign(&self.signing_message()).to_bytes());
    }

    /// The applicant's key.
    ///
    /// # Errors
    /// [`ApplicationError::BadKey`] for a malformed key.
    pub fn pubkey(&self) -> Result<[u8; 32], ApplicationError> {
        hex::decode(&self.pubkey_hex)
            .ok()
            .and_then(|bytes| <[u8; 32]>::try_from(bytes).ok())
            .filter(|key| VerifyingKey::from_bytes(key).is_ok())
            .ok_or(ApplicationError::BadKey)
    }

    /// Check the signature and shape, returning the claimed namespaces in
    /// claim order. Proofs are checked by the registry, not here.
    ///
    /// # Errors
    /// The first [`ApplicationError`] found.
    pub fn verify(&self) -> Result<Vec<String>, ApplicationError> {
        let key =
            VerifyingKey::from_bytes(&self.pubkey()?).map_err(|_| ApplicationError::BadKey)?;
        let signature = hex::decode(&self.signature_hex)
            .ok()
            .and_then(|bytes| <[u8; 64]>::try_from(bytes).ok())
            .ok_or(ApplicationError::BadSignature)?;
        key.verify(&self.signing_message(), &Signature::from_bytes(&signature))
            .map_err(|_| ApplicationError::BadSignature)?;
        if self.claims.is_empty() || self.claims.len() > MAX_CLAIMS {
            return Err(ApplicationError::ClaimCount);
        }
        if !(1..=256).contains(&self.contact.chars().count()) || self.note.chars().count() > 2048 {
            return Err(ApplicationError::BadText);
        }
        self.claims
            .iter()
            .map(|claim| {
                claim
                    .namespace()
                    .ok_or_else(|| ApplicationError::BadClaim(claim.clone()))
            })
            .collect()
    }
}

/// The line a proof must contain for the key `pubkey_hex`.
#[must_use]
pub fn proof_token(pubkey_hex: &str) -> String {
    format!("tilo-publisher={}", pubkey_hex.to_ascii_lowercase())
}

/// The DNS name whose TXT record proves `domain`.
#[must_use]
pub fn dns_txt_name(domain: &str) -> String {
    format!(
        "_tilo-publisher.{}",
        domain.trim_end_matches('.').to_ascii_lowercase()
    )
}

/// The HTTPS fallback proof for `domain`.
#[must_use]
pub fn well_known_url(domain: &str) -> String {
    format!(
        "https://{}/.well-known/tilo-publisher.txt",
        domain.trim_end_matches('.').to_ascii_lowercase()
    )
}

/// Where the proof for `account` on `forge` is read: the raw
/// `tilo-publisher.txt` of the account's `tilo-publisher` repository, on its
/// default branch.
#[must_use]
pub fn forge_proof_url(forge: &Forge, account: &str) -> String {
    let account = account.to_ascii_lowercase();
    match forge {
        Forge::GitHub => format!(
            "https://raw.githubusercontent.com/{account}/{FORGE_PROOF_REPOSITORY}/HEAD/{FORGE_PROOF_FILE}"
        ),
        Forge::Codeberg | Forge::Forgejo(_) => format!(
            "https://{}/api/v1/repos/{account}/{FORGE_PROOF_REPOSITORY}/raw/{FORGE_PROOF_FILE}",
            forge.host()
        ),
    }
}

/// Whether `text` (a TXT record or a proof file) carries `token` on a line of
/// its own.
#[must_use]
pub fn proof_matches(text: &str, token: &str) -> bool {
    text.lines().any(|line| line.trim() == token)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn application() -> PublisherApplication {
        PublisherApplication {
            pubkey_hex: String::new(),
            claims: vec![
                NamespaceClaim::Domain {
                    domain: "example.com".into(),
                },
                NamespaceClaim::Forge {
                    forge: "github".into(),
                    account: "Ada".into(),
                },
            ],
            contact: "ada@example.com".into(),
            note: "notes apps".into(),
            submitted_at_unix: 1_790_000_000,
            signature_hex: String::new(),
        }
    }

    #[test]
    fn a_signed_application_names_its_namespaces() {
        let key = SigningKey::from_bytes(&[3; 32]);
        let mut signed = application();
        signed.sign(&key);
        assert_eq!(signed.verify().unwrap(), ["com.example", "io.github.ada"]);
        let round_trip: PublisherApplication =
            serde_json::from_str(&serde_json::to_string(&signed).unwrap()).unwrap();
        assert_eq!(round_trip.verify().unwrap().len(), 2);

        let mut tampered = signed.clone();
        tampered.claims.push(NamespaceClaim::Domain {
            domain: "tilo.so".into(),
        });
        assert_eq!(tampered.verify(), Err(ApplicationError::BadSignature));

        let mut unsigned = application();
        unsigned.pubkey_hex = signed.pubkey_hex.clone();
        assert_eq!(unsigned.verify(), Err(ApplicationError::BadSignature));

        let mut bad = application();
        bad.claims = vec![NamespaceClaim::Domain {
            domain: "localhost".into(),
        }];
        bad.sign(&key);
        assert!(matches!(bad.verify(), Err(ApplicationError::BadClaim(_))));
        let mut empty = application();
        empty.claims.clear();
        empty.sign(&key);
        assert_eq!(empty.verify(), Err(ApplicationError::ClaimCount));
    }

    #[test]
    fn proofs_live_where_only_the_owner_can_write() {
        assert_eq!(dns_txt_name("Example.com."), "_tilo-publisher.example.com");
        assert_eq!(
            well_known_url("example.com"),
            "https://example.com/.well-known/tilo-publisher.txt"
        );
        assert_eq!(
            forge_proof_url(&Forge::GitHub, "Ada"),
            "https://raw.githubusercontent.com/ada/tilo-publisher/HEAD/tilo-publisher.txt"
        );
        assert_eq!(
            forge_proof_url(&Forge::Codeberg, "ada"),
            "https://codeberg.org/api/v1/repos/ada/tilo-publisher/raw/tilo-publisher.txt"
        );
        let token = proof_token(&"AB".repeat(32));
        assert_eq!(token, format!("tilo-publisher={}", "ab".repeat(32)));
        assert!(proof_matches(&format!("hello\n  {token}  \n"), &token));
        assert!(!proof_matches(&format!("x{token}"), &token));
    }
}
