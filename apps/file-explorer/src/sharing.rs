//! Client for the owner-only collaborative share service.
//!
//! Every call is async: the UI spawns it on the app runtime
//! (`ui::rt::app()`) and receives the outcome through its wake mechanism.

use std::path::Path;

use collab_types::Ed25519PublicKey;
use fs_protocol::{
    FilePermission, FileRecordId, NamedGrantMetadataV1, SHARE_CONTROL_FRAMING,
    SHARE_CONTROL_VERSION, ShareControlOperationV1, ShareControlRequestV1, ShareControlResponseV1,
    ShareControlResultV1,
};
use runtime_portal::local_socket;
use tilo_clock::unix_now_ms;
use tokio::io::AsyncWriteExt;
use uuid::Uuid;
use wire_frame::FrameError;

use crate::i18n::{arg, t, t_args};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShareActionResult {
    LinkCreated(String),
    NamedCreated(NamedGrantMetadataV1),
    Revoked { grant_id: Uuid, revoked: bool },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareSubjectChoice {
    Named(Ed25519PublicKey),
    Link,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ShareExpiryChoice {
    Day,
    Week,
    Month,
    Never,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LinkUseChoice {
    OneUse,
    /// Reusable, but still authority-consumed and bounded to this exact count.
    Bounded(u32),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShareComposerRequest {
    pub subject: ShareSubjectChoice,
    /// Governed, root-certified endpoint keys for a named recipient. Empty for
    /// bearer shares and for callers migrating from the V1 human-only map.
    pub recipient_devices: Vec<Ed25519PublicKey>,
    pub permission: FilePermission,
    pub expiry: ShareExpiryChoice,
    pub link_uses: Option<LinkUseChoice>,
}

impl ShareComposerRequest {
    fn operation_at(
        self,
        record_id: FileRecordId,
        now_ms: u64,
    ) -> Result<ShareControlOperationV1, String> {
        let expires_at_ms = match self.expiry {
            ShareExpiryChoice::Day => Some(now_ms.saturating_add(24 * 60 * 60 * 1_000)),
            ShareExpiryChoice::Week => Some(now_ms.saturating_add(7 * 24 * 60 * 60 * 1_000)),
            ShareExpiryChoice::Month => Some(now_ms.saturating_add(30 * 24 * 60 * 60 * 1_000)),
            ShareExpiryChoice::Never => None,
        };
        match self.subject {
            ShareSubjectChoice::Named(person) => {
                let mut recipient_devices = self.recipient_devices;
                recipient_devices.sort_by(|left, right| left.as_bytes().cmp(right.as_bytes()));
                recipient_devices.dedup();
                if self.link_uses.is_some()
                    || recipient_devices.is_empty()
                    || recipient_devices.len() > 32
                {
                    return Err(t("files-share-invalid-options"));
                }
                Ok(ShareControlOperationV1::CreateNamed {
                    record_id,
                    person,
                    recipient_devices,
                    permission: self.permission,
                    expires_at_ms,
                })
            }
            ShareSubjectChoice::Link => {
                if !self.recipient_devices.is_empty() {
                    return Err(t("files-share-invalid-options"));
                }
                let Some(expires_at_ms) = expires_at_ms else {
                    return Err(t("files-share-link-needs-expiry"));
                };
                let max_uses = match self.link_uses {
                    Some(LinkUseChoice::OneUse) => 1,
                    Some(LinkUseChoice::Bounded(uses)) if (2..=1_000).contains(&uses) => uses,
                    _ => return Err(t("files-share-invalid-uses")),
                };
                Ok(ShareControlOperationV1::CreateBearerV2 {
                    record_id,
                    permission: self.permission,
                    expires_at_ms,
                    max_uses: Some(max_uses),
                })
            }
        }
    }
}

/// Create a share from a validated composer request.
///
/// # Errors
///
/// Returns a localized error for invalid options or a failed control-service
/// transaction.
pub async fn create_share(
    socket: &Path,
    record_id: FileRecordId,
    request: ShareComposerRequest,
) -> Result<ShareActionResult, String> {
    transact(socket, request.operation_at(record_id, unix_now_ms())?).await
}

/// Create a one-use, view-only bearer link for one stable record.
///
/// # Errors
///
/// Returns a localized error when creation fails or the service returns the
/// wrong response type.
pub async fn create_one_use_view_link(
    socket: &Path,
    record_id: FileRecordId,
) -> Result<String, String> {
    match create_share(
        socket,
        record_id,
        ShareComposerRequest {
            subject: ShareSubjectChoice::Link,
            recipient_devices: Vec::new(),
            permission: FilePermission::View,
            expiry: ShareExpiryChoice::Day,
            link_uses: Some(LinkUseChoice::OneUse),
        },
    )
    .await?
    {
        ShareActionResult::LinkCreated(uri) => Ok(uri),
        ShareActionResult::NamedCreated(_) | ShareActionResult::Revoked { .. } => {
            Err(t("files-share-wrong-response"))
        }
    }
}

/// Grant a certified human root access to one stable record. `display_name`
/// is deliberately absent: the identity directory is presentation-only and
/// the authority request carries only the root key.
///
/// # Errors
///
/// Returns a localized error when the control transaction fails or returns the
/// wrong response type.
pub async fn create_named_share(
    socket: &Path,
    record_id: FileRecordId,
    person: Ed25519PublicKey,
    recipient_devices: Vec<Ed25519PublicKey>,
    permission: FilePermission,
    expires_at_ms: Option<u64>,
) -> Result<NamedGrantMetadataV1, String> {
    let operation = ShareControlOperationV1::CreateNamed {
        record_id,
        person,
        recipient_devices,
        permission,
        expires_at_ms,
    };
    // Preserve this lower-level compatibility entry point's exact absolute
    // deadline; new UI callers use `create_share` and its bounded choices.
    match transact(socket, operation).await? {
        ShareActionResult::NamedCreated(metadata) => Ok(metadata),
        ShareActionResult::LinkCreated(_) | ShareActionResult::Revoked { .. } => {
            Err(t("files-share-wrong-response"))
        }
    }
}

/// Revoke one exact share grant.
///
/// # Errors
///
/// Returns a localized error when the control-service transaction fails.
pub async fn revoke_grant(socket: &Path, grant_id: Uuid) -> Result<ShareActionResult, String> {
    transact(socket, ShareControlOperationV1::Revoke { grant_id }).await
}

/// One request/response exchange with the share service. Must be polled
/// in a tokio runtime context (the app runtime), which drives the socket.
async fn transact(
    socket: &Path,
    operation: ShareControlOperationV1,
) -> Result<ShareActionResult, String> {
    if !local_socket::SUPPORTED {
        return Err(t("files-share-not-available"));
    }
    let mut stream = local_socket::connect(socket).await.map_err(|error| {
        t_args(
            "files-share-unavailable-error",
            &[arg("error", error.to_string())],
        )
    })?;
    let request = ShareControlRequestV1 {
        protocol_version: SHARE_CONTROL_VERSION,
        operation,
    };
    let request = collab_types::canonical_bytes(&request).map_err(|error| {
        t_args(
            "files-share-encode-failed",
            &[arg("error", error.to_string())],
        )
    })?;
    let request = SHARE_CONTROL_FRAMING
        .encode(&request)
        .map_err(|_| t("files-share-request-too-large"))?;
    stream.write_all(&request).await.map_err(|error| {
        t_args(
            "files-share-request-failed",
            &[arg("error", error.to_string())],
        )
    })?;
    stream.flush().await.map_err(|error| {
        t_args(
            "files-share-flush-failed",
            &[arg("error", error.to_string())],
        )
    })?;
    let response = match SHARE_CONTROL_FRAMING.read_async(&mut stream).await {
        Ok(Some(response)) => response,
        Err(FrameError::TooLarge(_) | FrameError::EmptyFrame) => {
            return Err(t("files-share-response-too-large"));
        }
        Err(error @ FrameError::Truncated) => {
            return Err(t_args(
                "files-share-response-truncated",
                &[arg("error", error.to_string())],
            ));
        }
        Ok(None) => {
            return Err(t_args(
                "files-share-no-response",
                &[arg("error", FrameError::Closed.to_string())],
            ));
        }
        Err(error) => {
            return Err(t_args(
                "files-share-no-response",
                &[arg("error", error.to_string())],
            ));
        }
    };
    let response: ShareControlResponseV1 = collab_types::canonical::from_canonical_bytes(&response)
        .map_err(|error| {
            t_args(
                "files-share-response-invalid",
                &[arg("error", error.to_string())],
            )
        })?;
    match response.result {
        ShareControlResultV1::NamedCreated(metadata) => {
            Ok(ShareActionResult::NamedCreated(metadata))
        }
        ShareControlResultV1::BearerCreatedV2 { uri } => Ok(ShareActionResult::LinkCreated(uri)),
        ShareControlResultV1::Revoked { grant_id, revoked } => {
            Ok(ShareActionResult::Revoked { grant_id, revoked })
        }
        ShareControlResultV1::Error { code } => Err(t_args(
            "files-share-refused",
            &[arg("code", format!("{code:?}"))],
        )),
    }
}

#[cfg(test)]
mod tests {
    #[cfg(unix)]
    use std::io::{Read, Write};
    #[cfg(unix)]
    use std::os::unix::net::UnixListener;

    use super::*;

    /// The composer preserves permission, expiry and bounded link use, and rejects unbounded links or link controls on named grants.
    #[test]
    fn share_composer_bounds_links_and_grants_on_the_wire() {
        // --- phase: composer_preserves_permission_expiry_and_bounded_link_use_on_wire ---
        {
            let record_id = FileRecordId(Uuid::from_u128(39));
            let operation = ShareComposerRequest {
                subject: ShareSubjectChoice::Link,
                permission: FilePermission::Edit,
                expiry: ShareExpiryChoice::Week,
                link_uses: Some(LinkUseChoice::Bounded(10)),
                recipient_devices: Vec::new(),
            }
            .operation_at(record_id, 1_000)
            .unwrap();
            assert_eq!(
                operation,
                ShareControlOperationV1::CreateBearerV2 {
                    record_id,
                    permission: FilePermission::Edit,
                    expires_at_ms: 1_000 + 7 * 24 * 60 * 60 * 1_000,
                    max_uses: Some(10),
                }
            );
        }

        // --- phase: composer_rejects_unbounded_links_and_link_controls_on_named_grants ---
        {
            let record_id = FileRecordId(Uuid::from_u128(40));
            assert!(
                ShareComposerRequest {
                    subject: ShareSubjectChoice::Link,
                    permission: FilePermission::View,
                    expiry: ShareExpiryChoice::Never,
                    link_uses: Some(LinkUseChoice::OneUse),
                    recipient_devices: Vec::new(),
                }
                .operation_at(record_id, 1_000)
                .is_err()
            );
            assert!(
                ShareComposerRequest {
                    subject: ShareSubjectChoice::Named(Ed25519PublicKey::from_bytes([3; 32])),
                    permission: FilePermission::View,
                    expiry: ShareExpiryChoice::Month,
                    link_uses: Some(LinkUseChoice::OneUse),
                    recipient_devices: vec![Ed25519PublicKey::from_bytes([4; 32])],
                }
                .operation_at(record_id, 1_000)
                .is_err()
            );
            assert!(
                ShareComposerRequest {
                    subject: ShareSubjectChoice::Named(Ed25519PublicKey::from_bytes([5; 32])),
                    permission: FilePermission::View,
                    expiry: ShareExpiryChoice::Month,
                    link_uses: None,
                    recipient_devices: Vec::new(),
                }
                .operation_at(record_id, 1_000)
                .is_err()
            );
        }
    }

    /// Named shares send the human root without a display name and one-time links request exactly one redemption.
    #[allow(
        clippy::too_many_lines,
        reason = "merged scenario test: each labelled phase is one original case, kept verbatim"
    )]
    #[test]
    #[cfg(unix)]
    fn share_requests_send_exact_roots_and_redemptions() {
        // --- phase: named_share_sends_the_human_root_and_no_display_name ---
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("share.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let record_id = FileRecordId(Uuid::from_u128(41));
            let person = Ed25519PublicKey::from_bytes(
                ed25519_dalek::SigningKey::from_bytes(&[42; 32])
                    .verifying_key()
                    .to_bytes(),
            );
            let recipient_device = Ed25519PublicKey::from_bytes(
                ed25519_dalek::SigningKey::from_bytes(&[43; 32])
                    .verifying_key()
                    .to_bytes(),
            );
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0_u8; 4];
                stream.read_exact(&mut length).unwrap();
                let mut request = vec![0_u8; u32::from_be_bytes(length) as usize];
                stream.read_exact(&mut request).unwrap();
                let request: ShareControlRequestV1 =
                    collab_types::canonical::from_canonical_bytes(&request).unwrap();
                assert_eq!(
                    request.operation,
                    ShareControlOperationV1::CreateNamed {
                        record_id,
                        person,
                        recipient_devices: vec![recipient_device],
                        permission: FilePermission::View,
                        expires_at_ms: None,
                    }
                );
                let response = ShareControlResponseV1 {
                    protocol_version: SHARE_CONTROL_VERSION,
                    result: ShareControlResultV1::NamedCreated(NamedGrantMetadataV1 {
                        grant_id: Uuid::from_u128(43),
                        workspace_id: Uuid::from_u128(44),
                        record_id,
                        person,
                        permission: FilePermission::View,
                        expires_at_ms: None,
                        issuer: Ed25519PublicKey::from_bytes([5; 32]),
                        delivery: fs_protocol::NamedShareDeliveryStateV1::Pending,
                    }),
                };
                let bytes = collab_types::canonical_bytes(&response).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_be_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            });
            let metadata = ui::rt::app()
                .block_on(create_named_share(
                    &socket,
                    record_id,
                    person,
                    vec![recipient_device],
                    FilePermission::View,
                    None,
                ))
                .unwrap()
                .unwrap();
            assert_eq!(metadata.person, person);
            server.join().unwrap();
        }

        // --- phase: one_time_link_explicitly_requests_exactly_one_redemption ---
        {
            let dir = tempfile::tempdir().unwrap();
            let socket = dir.path().join("share.sock");
            let listener = UnixListener::bind(&socket).unwrap();
            let record_id = FileRecordId(Uuid::from_u128(51));
            let server = std::thread::spawn(move || {
                let (mut stream, _) = listener.accept().unwrap();
                let mut length = [0_u8; 4];
                stream.read_exact(&mut length).unwrap();
                let mut request = vec![0_u8; u32::from_be_bytes(length) as usize];
                stream.read_exact(&mut request).unwrap();
                let request: ShareControlRequestV1 =
                    collab_types::canonical::from_canonical_bytes(&request).unwrap();
                match request.operation {
                    ShareControlOperationV1::CreateBearerV2 {
                        record_id: requested,
                        permission,
                        expires_at_ms,
                        max_uses,
                    } => {
                        assert_eq!(requested, record_id);
                        assert_eq!(permission, FilePermission::View);
                        assert!(expires_at_ms > unix_now_ms());
                        assert_eq!(max_uses, Some(1));
                    }
                    other => panic!("unexpected one-time-link request: {other:?}"),
                }
                let response = ShareControlResponseV1 {
                    protocol_version: SHARE_CONTROL_VERSION,
                    result: ShareControlResultV1::BearerCreatedV2 {
                        uri: "tilo-file://one-use".into(),
                    },
                };
                let bytes = collab_types::canonical_bytes(&response).unwrap();
                stream
                    .write_all(&(bytes.len() as u32).to_be_bytes())
                    .unwrap();
                stream.write_all(&bytes).unwrap();
            });

            assert_eq!(
                ui::rt::app()
                    .block_on(create_one_use_view_link(&socket, record_id))
                    .unwrap()
                    .unwrap(),
                "tilo-file://one-use"
            );
            server.join().unwrap();
        }
    }
}
