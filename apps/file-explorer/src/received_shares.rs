//! Client for the per-user received-file service inherited from the launcher.
//!
//! Files remains networkless. The launcher connects to `tilo-fs.sock` outside
//! the sandbox and gives this process only that connected descriptor. Bearer
//! URIs are written inside a bounded frame and never placed in child argv.

#[cfg(unix)]
use std::os::fd::{BorrowedFd, RawFd};

#[cfg(unix)]
use collab_types::{
    RECIPIENT_CONTROL_FRAMING, RECIPIENT_CONTROL_VERSION, RecipientControlRequestV1,
};
use collab_types::{
    RecipientControlOperationV1, RecipientControlResponseV1, RecipientControlResultV1,
    RecipientSubscriptionStatusV1,
};
#[cfg(unix)]
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
#[cfg(unix)]
use wire_frame::FrameError;

use crate::i18n::{arg, t, t_args};

/// Admit a copied bearer link through the inherited recipient service.
///
/// # Errors
///
/// Returns a localized error when the service is unavailable, refuses the
/// link, or returns an invalid response.
pub fn admit_link(uri: String) -> Result<RecipientSubscriptionStatusV1, String> {
    transact(RecipientControlOperationV1::AdmitV2 { uri }).and_then(|response| {
        match response.result {
            RecipientControlResultV1::Admitted { status, .. } => Ok(status),
            RecipientControlResultV1::Error { code } => Err(t_args(
                "files-received-refused",
                &[arg("code", format!("{code:?}"))],
            )),
            RecipientControlResultV1::Status(_) => Err(t("files-received-wrong-response")),
        }
    })
}

/// Retry one previously admitted received-file subscription.
///
/// # Errors
///
/// Returns a localized error when the service is unavailable, refuses the
/// retry, or returns an invalid response.
pub fn retry(
    key: collab_types::RecipientSubscriptionKeyV1,
) -> Result<RecipientSubscriptionStatusV1, String> {
    transact(RecipientControlOperationV1::Retry { key }).and_then(|response| {
        match response.result {
            RecipientControlResultV1::Status(status) => Ok(status),
            RecipientControlResultV1::Error { code } => Err(t_args(
                "files-received-retry-refused",
                &[arg("code", format!("{code:?}"))],
            )),
            RecipientControlResultV1::Admitted { .. } => Err(t("files-received-wrong-response")),
        }
    })
}

#[cfg(unix)]
fn transact(operation: RecipientControlOperationV1) -> Result<RecipientControlResponseV1, String> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_io()
        .build()
        .map_err(|error| {
            t_args(
                "files-received-runtime-failed",
                &[arg("error", error.to_string())],
            )
        })?;
    runtime.block_on(async move {
        let mut stream = connected_service_stream()?;
        exchange(&mut stream, operation).await
    })
}

/// One request and its response on the recipient-control socket.
#[cfg(unix)]
async fn exchange<S: AsyncRead + AsyncWrite + Unpin>(
    stream: &mut S,
    operation: RecipientControlOperationV1,
) -> Result<RecipientControlResponseV1, String> {
    let request = RecipientControlRequestV1 {
        protocol_version: RECIPIENT_CONTROL_VERSION,
        operation,
    };
    let bytes =
        collab_types::canonical_bytes(&request).map_err(|_| t("files-received-encode-failed"))?;
    let frame = RECIPIENT_CONTROL_FRAMING
        .encode(&bytes)
        .map_err(|_| t("files-received-request-too-large"))?;
    stream
        .write_all(&frame)
        .await
        .map_err(|_| t("files-received-unavailable"))?;
    stream
        .flush()
        .await
        .map_err(|_| t("files-received-unavailable"))?;
    let response = match RECIPIENT_CONTROL_FRAMING.read_async(stream).await {
        Ok(Some(response)) => response,
        Err(FrameError::TooLarge(_) | FrameError::EmptyFrame) => {
            return Err(t("files-received-response-too-large"));
        }
        Err(FrameError::Truncated) => return Err(t("files-received-response-truncated")),
        Ok(None) | Err(_) => return Err(t("files-received-no-response")),
    };
    collab_types::canonical::from_canonical_bytes(&response)
        .map_err(|_| t("files-received-response-invalid"))
}

#[cfg(unix)]
fn connected_service_stream() -> Result<tokio::net::UnixStream, String> {
    let raw = service_fd().ok_or_else(|| t("files-received-not-available"))?;
    // Clone the inherited descriptor: converting the clone into Tokio ownership
    // must not close the launcher's stable descriptor after one transaction.
    let borrowed = unsafe { BorrowedFd::borrow_raw(raw) };
    let owned = borrowed
        .try_clone_to_owned()
        .map_err(|_| t("files-received-fd-clone-failed"))?;
    let stream = std::os::unix::net::UnixStream::from(owned);
    stream
        .set_nonblocking(true)
        .map_err(|_| t("files-received-fd-invalid"))?;
    tokio::net::UnixStream::from_std(stream).map_err(|_| t("files-received-fd-invalid"))
}

#[cfg(unix)]
fn service_fd() -> Option<RawFd> {
    // `tilo-fs` is the shipped manifest name. Accept `fs` while older local
    // development manifests are still present.
    runtime_portal::service_fd("tilo-fs").or_else(|| runtime_portal::service_fd("fs"))
}

#[cfg(not(unix))]
fn transact(_operation: RecipientControlOperationV1) -> Result<RecipientControlResponseV1, String> {
    Err(t("files-received-not-available"))
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    /// The recipient-control socket bytes: a big-endian `u32` length, then the
    /// canonical request or response. Empty frames are refused.
    #[test]
    fn recipient_control_wire_bytes_are_pinned() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_io()
            .build()
            .unwrap();
        runtime.block_on(async {
            let (mut client, mut service) = tokio::io::duplex(64 * 1024);
            let operation = RecipientControlOperationV1::AdmitV2 { uri: "u".into() };
            let body = collab_types::canonical_bytes(&RecipientControlRequestV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                operation: operation.clone(),
            })
            .unwrap();
            let response = collab_types::canonical_bytes(&RecipientControlResponseV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                result: RecipientControlResultV1::Error {
                    code: collab_types::RecipientControlErrorCode::NotFound,
                },
            })
            .unwrap();
            let mut reply = u32::try_from(response.len())
                .unwrap()
                .to_be_bytes()
                .to_vec();
            reply.extend_from_slice(&response);
            service.write_all(&reply).await.unwrap();

            let decoded = exchange(&mut client, operation.clone()).await.unwrap();
            assert!(matches!(
                decoded.result,
                RecipientControlResultV1::Error {
                    code: collab_types::RecipientControlErrorCode::NotFound
                }
            ));
            let mut sent = vec![0_u8; 4 + body.len()];
            service.read_exact(&mut sent).await.unwrap();
            assert_eq!(sent[..4], u32::try_from(body.len()).unwrap().to_be_bytes());
            assert_eq!(sent[4..], body);

            service.write_all(&[0, 0, 0, 0]).await.unwrap();
            assert!(exchange(&mut client, operation).await.is_err());
        });
    }
}
