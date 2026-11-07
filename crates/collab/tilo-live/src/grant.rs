//! What a native device presents to be admitted: the workspace, the scope it
//! joins, and its device certificate.
//!
//! On the native carrier the transport has already proved who is calling
//! ([`crate::native`] hands the relay the endpoint id its QUIC/TLS handshake
//! authenticated), so the grant only has to say what the device asks for and
//! carry the certificate that binds that key to a person's device. The relay
//! checks the certificate's key against the handshake's endpoint id, its
//! chain against the workspace's trust, and the device against the roster
//! ([`crate::relay::RelayAuthority::admit`]).
//!
//! Wire form (version 2):
//!
//! ```text
//! 2 | workspace [16] | scope (u8 length + id) | epoch u64 BE
//!   | certificate (u16 BE length + bytes) | delegation (u16 BE length + bytes)
//! ```
//!
//! The delegation is empty for a device that dials as itself. A page keeps its
//! device key unextractable, so its transport speaks under a key of its own
//! and presents its device's [`tilo_user_identity::EndpointDelegation`] here;
//! the relay admits the endpoint only through that signature.
//!
//! The workspace is the uuid of the workspace record, so the two ends spell
//! the namespace's workspace segment from the same bytes. The epoch is the
//! content-key epoch the device sealed under, carried so a relay can refuse a
//! grant that predates a rotation it has already applied.

use crate::preface::MAX_GRANT_BYTES;

/// The version byte of a native grant.
pub const NATIVE: u8 = 2;
/// Longest scope id a grant may name.
pub const MAX_SCOPE_BYTES: usize = 128;
/// Longest device certificate a grant may carry.
pub const MAX_CERTIFICATE_BYTES: usize = 4096;
/// Longest endpoint delegation a grant may carry.
pub const MAX_DELEGATION_BYTES: usize = 4096;

const HEADER: usize = 1 + 16 + 1;
const TAIL: usize = 8 + 4;

/// One device's request to join one scope of one workspace.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Native {
    /// The uuid bytes of the workspace record.
    pub workspace: [u8; 16],
    /// The scope's id, as [`crate::scope::Scope::id`] spells it.
    pub scope: String,
    /// The content-key epoch the device sealed under.
    pub epoch: u64,
    /// The device certificate that binds the caller's key to the roster.
    pub certificate: Vec<u8>,
    /// The device's statement that the calling endpoint speaks for it, when
    /// the connection is not the device's own key; empty for a device that
    /// dials as itself.
    pub delegation: Vec<u8>,
}

/// No length prefix is trusted before the bytes behind it are present.
fn encoded_len(length: usize) -> Option<u8> {
    u8::try_from(length).ok().filter(|length| *length > 0)
}

impl Native {
    /// The grant for one scope.
    ///
    /// # Errors
    ///
    /// Returns a message when the scope is empty or too long or the
    /// certificate does not fit an admission preface.
    pub fn new(
        workspace: [u8; 16],
        scope: &str,
        epoch: u64,
        certificate: Vec<u8>,
        delegation: Vec<u8>,
    ) -> Result<Self, String> {
        let scope_len = encoded_len(scope.len())
            .filter(|length| usize::from(*length) <= MAX_SCOPE_BYTES)
            .ok_or_else(|| format!("scope id must be 1..={MAX_SCOPE_BYTES} bytes"))?;
        let certificate_len = u16::try_from(certificate.len())
            .ok()
            .filter(|length| usize::from(*length) <= MAX_CERTIFICATE_BYTES)
            .ok_or_else(|| format!("certificate must be at most {MAX_CERTIFICATE_BYTES} bytes"))?;
        let delegation_len = u16::try_from(delegation.len())
            .ok()
            .filter(|length| usize::from(*length) <= MAX_DELEGATION_BYTES)
            .ok_or_else(|| format!("delegation must be at most {MAX_DELEGATION_BYTES} bytes"))?;
        let total = HEADER
            .saturating_add(usize::from(scope_len))
            .saturating_add(TAIL)
            .saturating_add(usize::from(certificate_len))
            .saturating_add(usize::from(delegation_len));
        if total > MAX_GRANT_BYTES {
            return Err(format!("grant must be at most {MAX_GRANT_BYTES} bytes"));
        }
        if certificate.is_empty() {
            return Err("certificate must not be empty".to_owned());
        }
        Ok(Self {
            workspace,
            scope: scope.to_owned(),
            epoch,
            certificate,
            delegation,
        })
    }

    /// The bytes the admission preface carries.
    #[must_use]
    pub fn encode(&self) -> Vec<u8> {
        let scope = self.scope.as_bytes();
        let scope_len = u8::try_from(scope.len()).unwrap_or(0);
        let certificate_len = u16::try_from(self.certificate.len()).unwrap_or(0);
        let [high, low] = certificate_len.to_be_bytes();
        let delegation_len = u16::try_from(self.delegation.len()).unwrap_or(0);
        let [delegation_high, delegation_low] = delegation_len.to_be_bytes();
        let capacity = HEADER
            .saturating_add(scope.len())
            .saturating_add(TAIL)
            .saturating_add(self.certificate.len())
            .saturating_add(self.delegation.len());
        let mut grant = Vec::with_capacity(capacity);
        grant.push(NATIVE);
        grant.extend_from_slice(&self.workspace);
        grant.push(scope_len);
        grant.extend_from_slice(scope);
        grant.extend_from_slice(&self.epoch.to_be_bytes());
        grant.extend_from_slice(&[high, low]);
        grant.extend_from_slice(&self.certificate);
        grant.extend_from_slice(&[delegation_high, delegation_low]);
        grant.extend_from_slice(&self.delegation);
        grant
    }

    /// The grant in `grant`, or `None` for any other version or shape.
    #[must_use]
    pub fn parse(grant: &[u8]) -> Option<Self> {
        let rest = grant.strip_prefix(&[NATIVE])?;
        let (workspace, rest) = rest.split_first_chunk::<16>()?;
        let (&scope_len, rest) = rest.split_first()?;
        let scope_len = usize::from(scope_len);
        if scope_len == 0 || scope_len > MAX_SCOPE_BYTES {
            return None;
        }
        let (scope, rest) = rest.split_at_checked(scope_len)?;
        let scope = String::from_utf8(scope.to_vec()).ok()?;
        let (epoch, rest) = rest.split_first_chunk::<8>()?;
        let (&[high, low], rest) = rest.split_first_chunk::<2>()?;
        let certificate_len = usize::from(u16::from_be_bytes([high, low]));
        if certificate_len == 0 || certificate_len > MAX_CERTIFICATE_BYTES {
            return None;
        }
        let (certificate, rest) = rest.split_at_checked(certificate_len)?;
        let (&[delegation_high, delegation_low], rest) = rest.split_first_chunk::<2>()?;
        let delegation_len = usize::from(u16::from_be_bytes([delegation_high, delegation_low]));
        if delegation_len > MAX_DELEGATION_BYTES {
            return None;
        }
        let (delegation, rest) = rest.split_at_checked(delegation_len)?;
        if !rest.is_empty() {
            return None;
        }
        Some(Self {
            workspace: *workspace,
            scope,
            epoch: u64::from_be_bytes(*epoch),
            certificate: certificate.to_vec(),
            delegation: delegation.to_vec(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant() -> Native {
        Native::new(
            [7; 16],
            "channel/0f8fad5b-d9cb-469f-a165-70867728950e",
            3,
            vec![9; 64],
            Vec::new(),
        )
        .unwrap()
    }

    #[test]
    fn a_grant_round_trips() {
        let grant = grant();
        assert_eq!(Native::parse(&grant.encode()), Some(grant));
    }

    #[test]
    fn a_delegated_transport_rides_beside_the_certificate() {
        let mut delegated = grant();
        delegated.delegation = vec![5; 200];
        assert_eq!(Native::parse(&delegated.encode()), Some(delegated));
    }

    #[test]
    fn the_version_is_the_first_byte_and_no_other_version_parses() {
        let mut bytes = grant().encode();
        assert_eq!(bytes[0], NATIVE);
        bytes[0] = 3;
        assert_eq!(Native::parse(&bytes), None);
        bytes[0] = 1;
        assert_eq!(
            Native::parse(&bytes),
            None,
            "version 1 carried no delegation"
        );
    }

    #[test]
    fn a_truncated_or_trailing_grant_parses_as_nothing() {
        let bytes = grant().encode();
        for cut in 1..bytes.len() {
            assert_eq!(
                Native::parse(&bytes[..cut]),
                None,
                "a grant cut to {cut} bytes"
            );
        }
        let mut longer = bytes;
        longer.push(0);
        assert_eq!(Native::parse(&longer), None);
    }

    #[test]
    fn a_scope_that_does_not_fit_is_refused() {
        let long = "s".repeat(MAX_SCOPE_BYTES + 1);
        assert!(Native::new([0; 16], &long, 0, vec![1], Vec::new()).is_err());
        assert!(Native::new([0; 16], "", 0, vec![1], Vec::new()).is_err());
        assert!(Native::new([0; 16], "workspace", 0, Vec::new(), Vec::new()).is_err());
        assert!(
            Native::new(
                [0; 16],
                "workspace",
                0,
                vec![1; MAX_CERTIFICATE_BYTES + 1],
                Vec::new()
            )
            .is_err()
        );
        assert!(
            Native::new(
                [0; 16],
                "workspace",
                0,
                vec![1],
                vec![1; MAX_DELEGATION_BYTES + 1]
            )
            .is_err()
        );
    }
}
