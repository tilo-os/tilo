//! Client for the per-user received-file service inherited from the launcher.
//!
//! Files remains networkless. The launcher connects to `tilo-fs.sock` outside
//! the sandbox and gives this process only that connected descriptor. Bearer
//! URIs are written inside a bounded frame and never placed in child argv.
//!
//! Every call is async: the UI spawns it on the app runtime
//! (`ui::rt::app()`) and receives the outcome through its wake mechanism.

use collab_types::{
    RECIPIENT_CONTROL_FRAMING, RECIPIENT_CONTROL_VERSION, RecipientControlOperationV1,
    RecipientControlRequestV1, RecipientControlResponseV1, RecipientControlResultV1,
    RecipientSubscriptionStatusV1,
};
use runtime_portal::local_socket::{self, ServiceStreamError};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use wire_frame::FrameError;

use crate::i18n::{arg, t, t_args};

/// Admit a copied bearer link through the inherited recipient service.
///
/// # Errors
///
/// Returns a localized error when the service is unavailable, refuses the
/// link, or returns an invalid response.
pub async fn admit_link(uri: String) -> Result<RecipientSubscriptionStatusV1, String> {
    transact(RecipientControlOperationV1::AdmitV2 { uri })
        .await
        .and_then(|response| match response.result {
            RecipientControlResultV1::Admitted { status, .. } => Ok(status),
            RecipientControlResultV1::Error { code } => Err(t_args(
                "files-received-refused",
                &[arg("code", format!("{code:?}"))],
            )),
            RecipientControlResultV1::Status(_) => Err(t("files-received-wrong-response")),
        })
}

/// Retry one previously admitted received-file subscription.
///
/// # Errors
///
/// Returns a localized error when the service is unavailable, refuses the
/// retry, or returns an invalid response.
pub async fn retry(
    key: collab_types::RecipientSubscriptionKeyV1,
) -> Result<RecipientSubscriptionStatusV1, String> {
    transact(RecipientControlOperationV1::Retry { key })
        .await
        .and_then(|response| match response.result {
            RecipientControlResultV1::Status(status) => Ok(status),
            RecipientControlResultV1::Error { code } => Err(t_args(
                "files-received-retry-refused",
                &[arg("code", format!("{code:?}"))],
            )),
            RecipientControlResultV1::Admitted { .. } => Err(t("files-received-wrong-response")),
        })
}

/// One request/response exchange with the recipient service. Must be polled
/// in a tokio runtime context (the app runtime), which drives the socket.
async fn transact(
    operation: RecipientControlOperationV1,
) -> Result<RecipientControlResponseV1, String> {
    let mut stream = connected_service_stream()?;
    exchange(&mut stream, operation).await
}

/// One request and its response on the recipient-control socket.
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

fn connected_service_stream() -> Result<local_socket::Stream, String> {
    // Work on a duplicate of the inherited descriptor: handing it to Tokio
    // must not close the launcher's stable descriptor after one transaction.
    // `tilo-fs` is the shipped manifest name. Accept `fs` while older local
    // development manifests are still present.
    local_socket::duplicate_service(&["tilo-fs", "fs"]).map_err(|error| match error {
        ServiceStreamError::Missing => t("files-received-not-available"),
        ServiceStreamError::Duplicate(_) => t("files-received-fd-clone-failed"),
        ServiceStreamError::Invalid(_) => t("files-received-fd-invalid"),
    })
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
