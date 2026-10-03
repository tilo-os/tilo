//! Publisher applications: a key asks to publish under namespaces, the
//! registry checks the ownership proofs (`registry_types::application`) and
//! queues the application for the owner, and the owner's approval — which
//! checks the proofs again — allowlists the key for exactly those namespaces.
//!
//! No application is ever approved remotely: approval is the owner's
//! `tilo-registry-server publishers approve`, like every other allowlist
//! change.

use std::io::Read as _;
use std::net::{SocketAddr, ToSocketAddrs as _};
use std::time::Duration;

use registry_types::application::{
    ApplicationError, NamespaceClaim, PublisherApplication, dns_txt_name, forge_proof_url,
    proof_matches, proof_token, well_known_url,
};
use registry_types::namespace::Forge;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::allowlist::PublisherRow;
use crate::storage::{Storage, StorageError};

/// How far an application's timestamp may be from the registry's clock.
pub const MAX_APPLICATION_SKEW_SECONDS: u64 = 24 * 60 * 60;

/// Most applications waiting at once; beyond it new ones are refused.
pub const MAX_PENDING_APPLICATIONS: usize = 1000;

/// Largest proof file read.
const MAX_PROOF_BYTES: u64 = 4096;

/// Where proofs are read from; the network in production.
pub trait ProofSource {
    /// The TXT records at `name`, each as one string.
    ///
    /// # Errors
    /// A description of why the lookup failed.
    fn txt_records(&self, name: &str) -> Result<Vec<String>, String>;

    /// The body of `url`, fetched over HTTPS from a public address.
    ///
    /// # Errors
    /// A description of why the fetch failed.
    fn https_text(&self, url: &str) -> Result<String, String>;
}

/// Reads proofs from public DNS and HTTPS.
#[derive(Clone, Copy, Debug)]
pub struct NetworkProofs {
    pub timeout: Duration,
}

impl Default for NetworkProofs {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(10),
        }
    }
}

impl ProofSource for NetworkProofs {
    fn txt_records(&self, name: &str) -> Result<Vec<String>, String> {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .map_err(|error| error.to_string())?;
        runtime.block_on(async {
            let resolver = hickory_resolver::TokioResolver::builder_tokio()
                .map_err(|error| error.to_string())?
                .build()
                .map_err(|error| error.to_string())?;
            let lookup = tokio::time::timeout(self.timeout, resolver.txt_lookup(name))
                .await
                .map_err(|_| "DNS lookup timed out".to_owned())?;
            match lookup {
                Ok(records) => Ok(records
                    .answers()
                    .iter()
                    .filter_map(|record| match &record.data {
                        hickory_resolver::proto::rr::RData::TXT(txt) => Some(
                            txt.txt_data
                                .iter()
                                .map(|part| String::from_utf8_lossy(part))
                                .collect::<String>(),
                        ),
                        _ => None,
                    })
                    .collect()),
                Err(error) if error.is_no_records_found() => Ok(Vec::new()),
                Err(error) => Err(error.to_string()),
            }
        })
    }

    fn https_text(&self, url: &str) -> Result<String, String> {
        let parsed = reqwest::Url::parse(url).map_err(|error| error.to_string())?;
        if parsed.scheme() != "https" {
            return Err("proofs are read over HTTPS".into());
        }
        let host = parsed.host_str().ok_or("the URL has no host")?.to_owned();
        let port = parsed.port_or_known_default().unwrap_or(443);
        let addresses: Vec<SocketAddr> = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|error| error.to_string())?
            .collect();
        let Some(address) = addresses.first().copied() else {
            return Err("the host has no address".into());
        };
        if addresses
            .iter()
            .any(|address| !crate::bundle_review::public_ip(address.ip()))
        {
            return Err("the host resolves to a non-public address".into());
        }
        let client = tilo_net::http::blocking_client()
            .timeout(self.timeout)
            .redirect(reqwest::redirect::Policy::none())
            .resolve(&host, address)
            .build()
            .map_err(|error| error.to_string())?;
        let response = client
            .get(parsed)
            .send()
            .map_err(|error| error.without_url().to_string())?;
        if response.status().is_redirection() {
            return Err("redirects are not followed".into());
        }
        if !response.status().is_success() {
            return Err(format!("HTTP {}", response.status()));
        }
        let mut body = String::new();
        response
            .take(MAX_PROOF_BYTES)
            .read_to_string(&mut body)
            .map_err(|error| error.to_string())?;
        Ok(body)
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

/// An application as the registry keeps it.
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
    /// The namespaces the application proved.
    #[must_use]
    pub fn namespaces(&self) -> Vec<String> {
        self.verified
            .iter()
            .map(|claim| claim.namespace.clone())
            .collect()
    }
}

#[derive(Debug, Error)]
pub enum ApplicationRejection {
    #[error(transparent)]
    Invalid(#[from] ApplicationError),
    #[error("the application is dated {submitted}, too far from the registry's clock {now}")]
    Stale { submitted: u64, now: u64 },
    #[error("no proof of {namespace} for this key: looked for `{token}` at {}", looked_at.join(" and "))]
    ProofMissing {
        namespace: String,
        token: String,
        looked_at: Vec<String>,
    },
}

/// Check `application`'s signature, age and every proof.
///
/// # Errors
/// The first reason the application cannot be accepted.
pub fn check_application(
    application: &PublisherApplication,
    now: u64,
    proofs: &dyn ProofSource,
) -> Result<Vec<VerifiedClaim>, ApplicationRejection> {
    application.verify()?;
    if application.submitted_at_unix.abs_diff(now) > MAX_APPLICATION_SKEW_SECONDS {
        return Err(ApplicationRejection::Stale {
            submitted: application.submitted_at_unix,
            now,
        });
    }
    check_proofs(application, now, proofs)
}

/// Check every claim's proof, regardless of the application's age.
///
/// # Errors
/// The first claim without a proof.
pub fn check_proofs(
    application: &PublisherApplication,
    now: u64,
    proofs: &dyn ProofSource,
) -> Result<Vec<VerifiedClaim>, ApplicationRejection> {
    let namespaces = application.verify()?;
    let token = proof_token(&application.pubkey_hex);
    application
        .claims
        .iter()
        .zip(namespaces)
        .map(|(claim, namespace)| check_claim(claim, namespace, &token, now, proofs))
        .collect()
}

fn check_claim(
    claim: &NamespaceClaim,
    namespace: String,
    token: &str,
    now: u64,
    proofs: &dyn ProofSource,
) -> Result<VerifiedClaim, ApplicationRejection> {
    let candidates: Vec<(ProofMethod, String)> = match claim {
        NamespaceClaim::Domain { domain } => vec![
            (ProofMethod::DnsTxt, dns_txt_name(domain)),
            (ProofMethod::WellKnown, well_known_url(domain)),
        ],
        NamespaceClaim::Forge { forge, account } => {
            let forge = Forge::parse(forge).ok_or_else(|| {
                ApplicationRejection::Invalid(ApplicationError::BadClaim(claim.clone()))
            })?;
            vec![(ProofMethod::Forge, forge_proof_url(&forge, account))]
        }
    };
    let mut looked_at = Vec::new();
    for (method, location) in candidates {
        let outcome = match method {
            ProofMethod::DnsTxt => proofs
                .txt_records(&location)
                .map(|records| records.iter().any(|record| proof_matches(record, token))),
            ProofMethod::WellKnown | ProofMethod::Forge => proofs
                .https_text(&location)
                .map(|text| proof_matches(&text, token)),
        };
        let place = match method {
            ProofMethod::DnsTxt => format!("DNS TXT {location}"),
            ProofMethod::WellKnown | ProofMethod::Forge => location.clone(),
        };
        match outcome {
            Ok(true) => {
                return Ok(VerifiedClaim {
                    namespace,
                    method,
                    location,
                    checked_at_unix: now,
                });
            }
            Ok(false) => looked_at.push(format!("{place} (not there)")),
            Err(error) => looked_at.push(format!("{place} ({error})")),
        }
    }
    Err(ApplicationRejection::ProofMissing {
        namespace,
        token: token.to_owned(),
        looked_at,
    })
}

#[derive(Debug, Error)]
pub enum ApplicationFailure {
    #[error(transparent)]
    Rejected(#[from] ApplicationRejection),
    #[error("the registry already holds {MAX_PENDING_APPLICATIONS} applications for review")]
    QueueFull,
    #[error("no application from {0}")]
    Unknown(String),
    #[error("the application from {0} is not pending")]
    NotPending(String),
    #[error(
        "a first-party promotion transaction requires recovery; run publish-staged-first-party"
    )]
    PromotionActive,
    #[error(transparent)]
    Storage(#[from] StorageError),
}

/// Check `application` and queue it for the owner, replacing any earlier
/// application from the same key.
///
/// # Errors
/// Why the application was not queued.
pub fn submit(
    storage: &Storage,
    application: PublisherApplication,
    now: u64,
    proofs: &dyn ProofSource,
) -> Result<StoredApplication, ApplicationFailure> {
    let verified = check_application(&application, now, proofs)?;
    let _lock = storage.lock_snapshot()?;
    let waiting = storage
        .load_publisher_applications()?
        .iter()
        .filter(|stored| {
            stored.status == ApplicationStatus::Pending
                && stored.application.pubkey_hex != application.pubkey_hex
        })
        .count();
    if waiting >= MAX_PENDING_APPLICATIONS {
        return Err(ApplicationFailure::QueueFull);
    }
    let stored = StoredApplication {
        application,
        verified,
        status: ApplicationStatus::Pending,
        received_at_unix: now,
        decided_at_unix: None,
        reviewer: None,
        reason: None,
    };
    storage.save_publisher_application(&stored)?;
    Ok(stored)
}

/// Check a pending application's proofs again and allowlist its key for the
/// namespaces they prove, keeping any namespaces the key already had.
///
/// # Errors
/// Why the application was not approved; nothing changes then.
pub fn approve(
    storage: &Storage,
    pubkey_hex: &str,
    reviewer: &str,
    now: u64,
    proofs: &dyn ProofSource,
) -> Result<(StoredApplication, PublisherRow), ApplicationFailure> {
    let mut stored = pending(storage, pubkey_hex)?;
    stored.verified = check_proofs(&stored.application, now, proofs)?;
    let _lock = storage.lock_snapshot()?;
    if storage.load_promotion_transaction()?.is_some() {
        return Err(ApplicationFailure::PromotionActive);
    }
    let mut allowlist = storage.load_allowlist()?;
    let mut namespaces = allowlist
        .publishers
        .iter()
        .find(|row| row.pubkey_hex.eq_ignore_ascii_case(pubkey_hex))
        .map(|row| row.namespaces.clone())
        .unwrap_or_default();
    for namespace in stored.namespaces() {
        if !namespaces.contains(&namespace) {
            namespaces.push(namespace);
        }
    }
    let row = PublisherRow {
        pubkey_hex: stored.application.pubkey_hex.clone(),
        can_add_new: true,
        note: stored.application.contact.clone(),
        namespaces,
    };
    allowlist.upsert(row.clone());
    storage.save_allowlist(&allowlist)?;
    stored.status = ApplicationStatus::Approved;
    stored.decided_at_unix = Some(now);
    stored.reviewer = Some(reviewer.to_owned());
    storage.save_publisher_application(&stored)?;
    Ok((stored, row))
}

/// Turn down a pending application; the key's allowlist row is untouched.
///
/// # Errors
/// Why the application could not be rejected.
pub fn reject(
    storage: &Storage,
    pubkey_hex: &str,
    reviewer: &str,
    reason: &str,
    now: u64,
) -> Result<StoredApplication, ApplicationFailure> {
    let mut stored = pending(storage, pubkey_hex)?;
    let _lock = storage.lock_snapshot()?;
    stored.status = ApplicationStatus::Rejected;
    stored.decided_at_unix = Some(now);
    stored.reviewer = Some(reviewer.to_owned());
    stored.reason = Some(reason.to_owned());
    storage.save_publisher_application(&stored)?;
    Ok(stored)
}

fn pending(storage: &Storage, pubkey_hex: &str) -> Result<StoredApplication, ApplicationFailure> {
    let stored = storage
        .load_publisher_application(pubkey_hex)?
        .ok_or_else(|| ApplicationFailure::Unknown(pubkey_hex.to_owned()))?;
    if stored.status != ApplicationStatus::Pending {
        return Err(ApplicationFailure::NotPending(pubkey_hex.to_owned()));
    }
    Ok(stored)
}

#[cfg(test)]
pub(crate) mod tests {
    use std::collections::BTreeMap;

    use ed25519_dalek::SigningKey;

    use super::*;

    /// Proofs from fixed maps, so tests never touch the network.
    #[derive(Default)]
    pub(crate) struct FixedProofs {
        pub txt: BTreeMap<String, Vec<String>>,
        pub https: BTreeMap<String, String>,
    }

    impl ProofSource for FixedProofs {
        fn txt_records(&self, name: &str) -> Result<Vec<String>, String> {
            Ok(self.txt.get(name).cloned().unwrap_or_default())
        }

        fn https_text(&self, url: &str) -> Result<String, String> {
            self.https.get(url).cloned().ok_or_else(|| "404".into())
        }
    }

    pub(crate) fn signed(
        key: &SigningKey,
        claims: Vec<NamespaceClaim>,
        now: u64,
    ) -> PublisherApplication {
        let mut application = PublisherApplication {
            pubkey_hex: String::new(),
            claims,
            contact: "ada@example.com".into(),
            note: String::new(),
            submitted_at_unix: now,
            signature_hex: String::new(),
        };
        application.sign(key);
        application
    }

    #[test]
    fn each_claim_needs_its_proof_where_only_the_owner_can_write() {
        let key = SigningKey::from_bytes(&[5; 32]);
        let token = proof_token(&hex::encode(key.verifying_key().to_bytes()));
        let now = 1_790_000_000;
        let application = signed(
            &key,
            vec![
                NamespaceClaim::Domain {
                    domain: "example.com".into(),
                },
                NamespaceClaim::Domain {
                    domain: "example.org".into(),
                },
                NamespaceClaim::Forge {
                    forge: "codeberg".into(),
                    account: "ada".into(),
                },
            ],
            now,
        );
        let mut proofs = FixedProofs::default();
        let missing = check_application(&application, now, &proofs).unwrap_err();
        assert!(
            missing.to_string().contains("_tilo-publisher.example.com"),
            "{missing}"
        );

        proofs.txt.insert(
            "_tilo-publisher.example.com".into(),
            vec!["v=spf1".into(), token.clone()],
        );
        proofs.https.insert(
            "https://example.org/.well-known/tilo-publisher.txt".into(),
            format!("# publisher keys\n{token}\n"),
        );
        proofs.https.insert(
            "https://codeberg.org/api/v1/repos/ada/tilo-publisher/raw/tilo-publisher.txt".into(),
            proof_token(&"ee".repeat(32)),
        );
        let wrong_key = check_application(&application, now, &proofs).unwrap_err();
        assert!(matches!(
            wrong_key,
            ApplicationRejection::ProofMissing { ref namespace, .. } if namespace == "org.codeberg.ada"
        ));

        proofs.https.insert(
            "https://codeberg.org/api/v1/repos/ada/tilo-publisher/raw/tilo-publisher.txt".into(),
            token,
        );
        let verified = check_application(&application, now, &proofs).unwrap();
        assert_eq!(
            verified
                .iter()
                .map(|claim| (claim.namespace.as_str(), claim.method))
                .collect::<Vec<_>>(),
            [
                ("com.example", ProofMethod::DnsTxt),
                ("org.example", ProofMethod::WellKnown),
                ("org.codeberg.ada", ProofMethod::Forge),
            ]
        );
        assert!(matches!(
            check_application(
                &application,
                now + MAX_APPLICATION_SKEW_SECONDS + 1,
                &proofs
            ),
            Err(ApplicationRejection::Stale { .. })
        ));
        assert!(
            check_proofs(
                &application,
                now + MAX_APPLICATION_SKEW_SECONDS + 1,
                &proofs
            )
            .is_ok()
        );
    }
}
