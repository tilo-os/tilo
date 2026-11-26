//! SSH host-key trust: what a saved connection expects of the machine, and what it observed.
//!
//! A connection is stopped before authentication unless the key the machine presents is the
//! exact one this device trusted for that host and port. The first contact with a machine has
//! no such key and fails closed; a key that changed fails closed too, and neither is ever
//! overwritten silently. The trust itself is the app's, kept on the device; this is only the
//! decision.

use serde::{Deserialize, Serialize};

/// The identity of an SSH host key: its algorithm, and its SHA-256 fingerprint as `ssh-key`
/// writes it (`SHA256:…`).
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostKeyIdentity {
    pub algorithm: String,
    pub fingerprint: String,
}

/// Why a host key was refused. The connection is stopped before authentication whenever one of
/// these is present, and the app asks the person what to do.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "issue", rename_all = "snake_case")]
pub enum HostKeyIssue {
    /// This device has trusted no key for the machine yet: the observed one is shown for the
    /// person to check out of band before it is trusted.
    FirstContact { observed: HostKeyIdentity },
    /// The machine presented a key other than the trusted one. A changed key is never
    /// overwritten: the person forgets it first.
    Changed {
        expected: HostKeyIdentity,
        observed: HostKeyIdentity,
    },
}

impl HostKeyIssue {
    /// The sentence a person reads of it, naming what was observed where the app cannot.
    #[must_use]
    pub fn message(&self, host: &str, port: u16) -> String {
        match self {
            Self::FirstContact { observed } => format!(
                "SSH host key for {host}:{port} needs explicit trust: {} {}",
                observed.algorithm, observed.fingerprint
            ),
            Self::Changed { expected, observed } => format!(
                "SSH host key changed for {host}:{port}. Expected {} {}; received {} {}. Connection blocked.",
                expected.algorithm, expected.fingerprint, observed.algorithm, observed.fingerprint
            ),
        }
    }
}

/// Verify `observed` against the key `expected`, when this device trusted one: a first contact
/// is refused until the observed key is trusted, and a changed key is refused until the old one
/// is forgotten.
///
/// # Errors
///
/// [`HostKeyIssue`] saying which decision the person has to make.
pub fn verify(
    expected: Option<&HostKeyIdentity>,
    observed: HostKeyIdentity,
) -> Result<(), HostKeyIssue> {
    match expected {
        Some(expected) if *expected == observed => Ok(()),
        Some(expected) => Err(HostKeyIssue::Changed {
            expected: expected.clone(),
            observed,
        }),
        None => Err(HostKeyIssue::FirstContact { observed }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(fingerprint: &str) -> HostKeyIdentity {
        HostKeyIdentity {
            algorithm: "ssh-ed25519".to_owned(),
            fingerprint: fingerprint.to_owned(),
        }
    }

    tilo_test_env::scenario! {
        fn a_host_key_is_trusted_only_when_it_is_exactly_the_one_expected;

        phase first_contact_fails_closed_with_the_key_it_saw;
        {
            let observed = key("SHA256:first");
            assert_eq!(
                verify(None, observed.clone()),
                Err(HostKeyIssue::FirstContact { observed })
            );
        }

        phase the_trusted_key_reconnects_and_a_changed_one_is_refused;
        {
            let trusted = key("SHA256:first");
            assert_eq!(verify(Some(&trusted), trusted.clone()), Ok(()));
            let changed = key("SHA256:changed");
            assert_eq!(
                verify(Some(&trusted), changed.clone()),
                Err(HostKeyIssue::Changed {
                    expected: trusted.clone(),
                    observed: changed,
                })
            );
        }

        phase the_refusal_names_the_machine_the_algorithm_and_the_fingerprint;
        {
            let issue = HostKeyIssue::FirstContact {
                observed: key("SHA256:abc"),
            };
            let said = issue.message("build.lab", 2222);
            assert!(said.contains("build.lab:2222"), "{said}");
            assert!(said.contains("ssh-ed25519"), "{said}");
            assert!(said.contains("SHA256:abc"), "{said}");
        }
    }
}
