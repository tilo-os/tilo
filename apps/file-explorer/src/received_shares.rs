//! Client for the per-user received-file service inherited from the launcher.
//!
//! Files remains networkless. The launcher connects to `qos-fs.sock` outside
//! the sandbox and gives this process only that connected descriptor. Bearer
//! URIs are written inside a bounded frame and never placed in child argv.

use std::os::fd::{BorrowedFd, RawFd};

use collab_types::{
    MAX_RECIPIENT_CONTROL_FRAME_BYTES, RECIPIENT_CONTROL_VERSION, RecipientControlOperationV1,
    RecipientControlRequestV1, RecipientControlResponseV1, RecipientControlResultV1,
    RecipientSubscriptionStatusV1,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

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
        let request = RecipientControlRequestV1 {
            protocol_version: RECIPIENT_CONTROL_VERSION,
            operation,
        };
        let bytes = collab_types::canonical_bytes(&request)
            .map_err(|_| t("files-received-encode-failed"))?;
        if bytes.is_empty() || bytes.len() > MAX_RECIPIENT_CONTROL_FRAME_BYTES {
            return Err(t("files-received-request-too-large"));
        }
        stream
            .write_all(&(bytes.len() as u32).to_be_bytes())
            .await
            .map_err(|_| t("files-received-unavailable"))?;
        stream
            .write_all(&bytes)
            .await
            .map_err(|_| t("files-received-unavailable"))?;
        stream
            .flush()
            .await
            .map_err(|_| t("files-received-unavailable"))?;
        let mut prefix = [0u8; 4];
        stream
            .read_exact(&mut prefix)
            .await
            .map_err(|_| t("files-received-no-response"))?;
        let length = u32::from_be_bytes(prefix) as usize;
        if length == 0 || length > MAX_RECIPIENT_CONTROL_FRAME_BYTES {
            return Err(t("files-received-response-too-large"));
        }
        let mut response = vec![0u8; length];
        stream
            .read_exact(&mut response)
            .await
            .map_err(|_| t("files-received-response-truncated"))?;
        collab_types::canonical::from_canonical_bytes(&response)
            .map_err(|_| t("files-received-response-invalid"))
    })
}

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

fn service_fd() -> Option<RawFd> {
    // `qos-fs` is the shipped manifest name. Accept `fs` while older local
    // development manifests are still present.
    runtime_portal::service_fd("qos-fs").or_else(|| runtime_portal::service_fd("fs"))
}
