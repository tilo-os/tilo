//! Endpoint-bound, copyable collaborative-file links.
//!
//! The earlier `tilo-file://v1/` representation carries a signed token but no
//! place to redeem it. V2 has a distinct prefix, carries an Iroh
//! [`EndpointAddr`], and has the invite issuer sign the endpoint-to-token
//! binding. The bearer secret is encoded only after `#`.

use std::fmt;

use collab_types::{
    BearerFileInvite, FILE_INVITE_LINK_PREFIX, FileInviteError, FileInviteSubject, FileInviteV1,
    Signature, VerifyError, bearer_secret_hash, canonical_bytes, sign_event, verify_event,
};
use iroh_base::EndpointAddr;
use serde::{Deserialize, Serialize};
use thiserror::Error;

use crate::clock::unix_now_ms;

pub const FILE_SHARE_LINK_V2_DOMAIN_TAG: &str = "tilo-file-share-link-v2";
pub const FILE_SHARE_LINK_V2_PREFIX: &str = "tilo-file://v2/";
pub const MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES: usize = 32 * 1024;
const VERSION: u8 = 2;

/// A reachable bearer invitation for one file record.
#[derive(Clone, PartialEq, Eq)]
pub struct FileShareLinkV2 {
    pub endpoint: EndpointAddr,
    pub invite: FileInviteV1,
    pub endpoint_binding_signature: Signature,
    secret: [u8; 32],
}

impl fmt::Debug for FileShareLinkV2 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FileShareLinkV2")
            .field("endpoint", &self.endpoint)
            .field("invite", &self.invite)
            .field(
                "endpoint_binding_signature",
                &self.endpoint_binding_signature,
            )
            .field("secret", &"<redacted>")
            .finish()
    }
}

#[derive(Serialize)]
struct BindingView<'a> {
    tag: &'a str,
    endpoint: &'a EndpointAddr,
    invite: &'a FileInviteV1,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct WireV2 {
    version: u8,
    endpoint: EndpointAddr,
    invite: FileInviteV1,
    endpoint_binding_signature: Signature,
}

impl FileShareLinkV2 {
    /// Validate and issuer-bind a signed bearer invite to a routed endpoint.
    ///
    /// # Errors
    ///
    /// Returns an error for an unroutable endpoint, invalid/expired invite or
    /// secret, wrong issuer, signing failure, or invalid endpoint binding.
    pub fn bind_at(
        endpoint: EndpointAddr,
        bearer: BearerFileInvite,
        issuer: &ed25519_dalek::SigningKey,
        now_ms: u64,
    ) -> Result<Self, FileShareLinkError> {
        validate_endpoint(&endpoint)?;
        validate_invite_secret(&bearer.token, &bearer.secret, now_ms)?;
        if bearer.token.issuer.as_bytes() != &issuer.verifying_key().to_bytes() {
            return Err(FileShareLinkError::WrongBindingKey);
        }
        let endpoint_binding_signature = sign_event(
            &BindingView {
                tag: FILE_SHARE_LINK_V2_DOMAIN_TAG,
                endpoint: &endpoint,
                invite: &bearer.token,
            },
            issuer,
        )?;
        let link = Self {
            endpoint,
            invite: bearer.token,
            endpoint_binding_signature,
            secret: bearer.secret,
        };
        link.validate_at(now_ms)?;
        Ok(link)
    }

    /// Validate endpoint reachability/binding, invite signature/expiry, and
    /// the fragment secret. This does not consume an authority redemption.
    ///
    /// # Errors
    ///
    /// Returns an error when the endpoint, invite, bearer secret, or binding
    /// signature fails validation.
    pub fn validate_at(&self, now_ms: u64) -> Result<(), FileShareLinkError> {
        validate_endpoint(&self.endpoint)?;
        validate_invite_secret(&self.invite, &self.secret, now_ms)?;
        verify_event(
            &BindingView {
                tag: FILE_SHARE_LINK_V2_DOMAIN_TAG,
                endpoint: &self.endpoint,
                invite: &self.invite,
            },
            &self.endpoint_binding_signature,
            &self.invite.issuer,
        )
        .map_err(FileShareLinkError::BadEndpointBinding)
    }

    /// Encode this validated link as its secret-bearing V2 URI.
    ///
    /// # Errors
    ///
    /// Returns an error when validation/encoding fails or the payload exceeds
    /// its protocol bound.
    pub fn encode_uri_at(&self, now_ms: u64) -> Result<String, FileShareLinkError> {
        self.validate_at(now_ms)?;
        let payload = canonical_bytes(&WireV2 {
            version: VERSION,
            endpoint: self.endpoint.clone(),
            invite: self.invite.clone(),
            endpoint_binding_signature: self.endpoint_binding_signature,
        })?;
        if payload.len() > MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES {
            return Err(FileShareLinkError::PayloadTooLarge);
        }
        Ok(format!(
            "{FILE_SHARE_LINK_V2_PREFIX}{}#{}",
            hex::encode(payload),
            hex::encode(self.secret)
        ))
    }

    /// Encode using the process security clock.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::encode_uri_at`].
    pub fn encode_uri(&self) -> Result<String, FileShareLinkError> {
        self.encode_uri_at(unix_now_ms())
    }

    /// Decode V2 only. A V1 token gets a specific reachability error rather
    /// than being silently treated as this stronger format.
    ///
    /// # Errors
    ///
    /// Returns an error for malformed, legacy, oversized, unsupported, or
    /// cryptographically invalid links.
    pub fn decode_uri_at(uri: &str, now_ms: u64) -> Result<Self, FileShareLinkError> {
        let link = Self::decode_uri_shape(uri)?;
        link.validate_at(now_ms)?;
        Ok(link)
    }

    /// Decode and authenticate a previously admitted link even after expiry.
    /// This is only for rendering durable subscription state; callers must
    /// still use [`Self::validate_at`] before network access.
    pub(crate) fn decode_persisted_uri(uri: &str) -> Result<Self, FileShareLinkError> {
        let link = Self::decode_uri_shape(uri)?;
        let validation_time = link
            .invite
            .expires_at_ms
            .map_or_else(unix_now_ms, |deadline| deadline.saturating_sub(1));
        link.validate_at(validation_time)?;
        Ok(link)
    }

    fn decode_uri_shape(uri: &str) -> Result<Self, FileShareLinkError> {
        if uri.starts_with(FILE_INVITE_LINK_PREFIX) {
            return Err(FileShareLinkError::LegacyTokenOnlyV1);
        }
        let maximum = FILE_SHARE_LINK_V2_PREFIX
            .len()
            .saturating_add(MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES.saturating_mul(2))
            .saturating_add(1)
            .saturating_add(64);
        if uri.len() > maximum {
            return Err(FileShareLinkError::PayloadTooLarge);
        }
        let remainder = uri
            .strip_prefix(FILE_SHARE_LINK_V2_PREFIX)
            .ok_or(FileShareLinkError::InvalidUri)?;
        let (payload_hex, secret_hex) = remainder
            .split_once('#')
            .ok_or(FileShareLinkError::InvalidUri)?;
        if payload_hex.len() > MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES * 2
            || payload_hex.len() % 2 != 0
            || secret_hex.len() != 64
        {
            return Err(FileShareLinkError::InvalidUri);
        }
        let payload = hex::decode(payload_hex).map_err(|_| FileShareLinkError::InvalidUri)?;
        let secret: [u8; 32] = hex::decode(secret_hex)
            .map_err(|_| FileShareLinkError::InvalidUri)?
            .try_into()
            .map_err(|_| FileShareLinkError::InvalidUri)?;
        let wire: WireV2 = collab_types::canonical::from_canonical_bytes(&payload)?;
        if wire.version != VERSION {
            return Err(FileShareLinkError::UnsupportedVersion(wire.version));
        }
        Ok(Self {
            endpoint: wire.endpoint,
            invite: wire.invite,
            endpoint_binding_signature: wire.endpoint_binding_signature,
            secret,
        })
    }

    /// Decode and validate a V2 URI using the process security clock.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`Self::decode_uri_at`].
    pub fn decode_uri(uri: &str) -> Result<Self, FileShareLinkError> {
        Self::decode_uri_at(uri, unix_now_ms())
    }

    /// Secret for online authority redemption. Never log or persist it.
    pub fn bearer_secret(&self) -> &[u8; 32] {
        &self.secret
    }
}

fn validate_endpoint(endpoint: &EndpointAddr) -> Result<(), FileShareLinkError> {
    if endpoint.is_empty() {
        return Err(FileShareLinkError::EndpointHasNoRoute);
    }
    Ok(())
}

fn validate_invite_secret(
    invite: &FileInviteV1,
    secret: &[u8; 32],
    now_ms: u64,
) -> Result<(), FileShareLinkError> {
    invite.verify_at(now_ms)?;
    let FileInviteSubject::Bearer { secret_hash } = invite.subject else {
        return Err(FileShareLinkError::NotBearer);
    };
    if bearer_secret_hash(secret) != secret_hash {
        return Err(FileShareLinkError::WrongSecret);
    }
    Ok(())
}

#[derive(Debug, Error)]
pub enum FileShareLinkError {
    #[error("tilo-file v1 is token-only and contains no redemption endpoint")]
    LegacyTokenOnlyV1,
    #[error("file share URI is malformed")]
    InvalidUri,
    #[error("unsupported file share link version {0}")]
    UnsupportedVersion(u8),
    #[error("file share endpoint contains no network route")]
    EndpointHasNoRoute,
    #[error("file share link payload exceeds its wire bound")]
    PayloadTooLarge,
    #[error("embedded file invite is invalid: {0}")]
    Invite(#[from] FileInviteError),
    #[error("file share link requires a bearer invite")]
    NotBearer,
    #[error("file share link bearer secret does not match")]
    WrongSecret,
    #[error("endpoint binding key is not the file invite issuer")]
    WrongBindingKey,
    #[error("file share endpoint binding did not verify: {0}")]
    BadEndpointBinding(#[source] VerifyError),
    #[error("file share link canonical encoding failed: {0}")]
    Canonical(#[from] collab_types::canonical::CanonicalError),
}

#[cfg(test)]
mod tests {
    use super::*;
    use collab_types::{
        FileInviteSpec, FileInviteSubject, FilePermission, FileRecordId, mint_bearer_file_invite,
    };
    use rand::{SeedableRng, rngs::StdRng};
    use uuid::Uuid;

    const NOW: u64 = 9_000;

    fn fixture() -> FileShareLinkV2 {
        let issuer = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
        let mut rng = StdRng::seed_from_u64(7);
        let bearer = mint_bearer_file_invite(
            FileInviteSpec {
                workspace_id: Uuid::from_u128(1),
                record_id: FileRecordId(Uuid::from_u128(2)),
                subject: FileInviteSubject::Bearer {
                    secret_hash: [0; 32],
                },
                permission: FilePermission::View,
                expires_at_ms: Some(10_000),
                max_redemptions: Some(1),
            },
            &issuer,
            &mut rng,
        )
        .unwrap();
        let endpoint = EndpointAddr::new(iroh::SecretKey::from_bytes(&[41; 32]).public())
            .with_ip_addr("127.0.0.1:4455".parse().unwrap());
        FileShareLinkV2::bind_at(endpoint, bearer, &issuer, NOW).unwrap()
    }

    #[test]
    fn share_links_round_trip_canonically_and_reject_tampering_expiry_and_oversize() {
        // Phase `roundtrip_is_canonical_and_secret_exists_only_in_fragment`.
        {
            let link = fixture();
            let uri = link.encode_uri_at(NOW).unwrap();
            let (route, fragment) = uri.split_once('#').unwrap();
            assert!(!route.contains(&hex::encode(link.secret)));
            assert_eq!(fragment, hex::encode(link.secret));
            assert_eq!(FileShareLinkV2::decode_uri_at(&uri, NOW).unwrap(), link);
            assert_eq!(link.encode_uri_at(NOW).unwrap(), uri);
        }
        // Phase `endpoint_token_and_secret_tampering_fail`.
        {
            let link = fixture();
            let mut wire = WireV2 {
                version: VERSION,
                endpoint: link.endpoint.clone(),
                invite: link.invite.clone(),
                endpoint_binding_signature: link.endpoint_binding_signature,
            };
            wire.endpoint = wire
                .endpoint
                .with_ip_addr("127.0.0.1:9999".parse().unwrap());
            assert!(matches!(
                FileShareLinkV2::decode_uri_at(&uri_for(&wire, link.secret), NOW),
                Err(FileShareLinkError::BadEndpointBinding(_))
            ));
            wire.endpoint = link.endpoint.clone();
            wire.invite.record_id = FileRecordId::new();
            assert!(matches!(
                FileShareLinkV2::decode_uri_at(&uri_for(&wire, link.secret), NOW),
                Err(FileShareLinkError::Invite(FileInviteError::BadSignature(_)))
            ));
            wire.invite = link.invite;
            assert!(matches!(
                FileShareLinkV2::decode_uri_at(&uri_for(&wire, [99; 32]), NOW),
                Err(FileShareLinkError::WrongSecret)
            ));
        }
        // Phase `expiry_legacy_routeless_and_oversize_are_explicit`.
        {
            let link = fixture();
            let uri = link.encode_uri_at(NOW).unwrap();
            assert!(matches!(
                FileShareLinkV2::decode_uri_at(&uri, 10_000),
                Err(FileShareLinkError::Invite(FileInviteError::Expired))
            ));
            assert!(matches!(
                FileShareLinkV2::decode_uri_at("tilo-file://v1/00#00", NOW),
                Err(FileShareLinkError::LegacyTokenOnlyV1)
            ));
            let oversized = format!(
                "{FILE_SHARE_LINK_V2_PREFIX}{}#{}",
                "00".repeat(MAX_FILE_SHARE_LINK_V2_PAYLOAD_BYTES + 1),
                "00".repeat(32)
            );
            assert!(matches!(
                FileShareLinkV2::decode_uri_at(&oversized, NOW),
                Err(FileShareLinkError::PayloadTooLarge)
            ));

            let issuer = ed25519_dalek::SigningKey::from_bytes(&[31; 32]);
            let mut rng = StdRng::seed_from_u64(8);
            let bearer = mint_bearer_file_invite(
                FileInviteSpec {
                    workspace_id: Uuid::new_v4(),
                    record_id: FileRecordId::new(),
                    subject: FileInviteSubject::Bearer {
                        secret_hash: [0; 32],
                    },
                    permission: FilePermission::View,
                    expires_at_ms: Some(10_000),
                    max_redemptions: None,
                },
                &issuer,
                &mut rng,
            )
            .unwrap();
            assert!(matches!(
                FileShareLinkV2::bind_at(
                    EndpointAddr::new(iroh::SecretKey::from_bytes(&[44; 32]).public()),
                    bearer,
                    &issuer,
                    NOW
                ),
                Err(FileShareLinkError::EndpointHasNoRoute)
            ));
        }
    }

    fn uri_for(wire: &WireV2, secret: [u8; 32]) -> String {
        let payload = canonical_bytes(wire).unwrap();
        format!(
            "{FILE_SHARE_LINK_V2_PREFIX}{}#{}",
            hex::encode(payload),
            hex::encode(secret)
        )
    }
}
