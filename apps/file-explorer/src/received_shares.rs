//! Client for the per-user received-file service inherited from the launcher.
//!
//! Files remains networkless. The launcher connects to `tilo-fs.sock` outside
//! the sandbox and gives this process only that connected descriptor. Bearer
//! URIs are written inside a bounded frame and never placed in child argv.
//!
//! Every call is async: the UI spawns it on the app runtime
//! (`ui::rt::app()`) and receives the outcome through its wake mechanism.

use fs_protocol::{
    RECIPIENT_CONTROL_FRAMING, RECIPIENT_CONTROL_VERSION, RecipientControlOperationV1,
    RecipientControlRequestV1, RecipientControlResponseV1, RecipientControlResultV1,
    RecipientSubscriptionStatusV1,
};
use runtime_portal::local_socket::{self, ServiceStreamError};
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use wire_frame::FrameError;

use crate::i18n::{arg, t, t_args};

/// How a received share link starts: the recipient service decodes the rest.
pub const LINK_SCHEME: &str = "tilo-file://";

/// The launch switch that hands Files a received link on standard input,
/// which keeps the bearer secret out of the process list.
pub const STDIN_FLAG: &str = "--open-share-stdin";

/// The longest received link Files reads.
const MAX_LINK_BYTES: usize = 16 * 1024;

/// Read one received link from `input` (standard input for
/// [`STDIN_FLAG`]).
///
/// # Errors
///
/// Returns a localized reason when the input cannot be read, is too long,
/// is not text or is empty.
pub fn read_link(input: impl std::io::Read) -> Result<String, String> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    if input
        .take((MAX_LINK_BYTES + 1) as u64)
        .read_to_end(&mut bytes)
        .is_err()
    {
        return Err(crate::i18n::t("files-received-link-read-failed"));
    }
    if bytes.len() > MAX_LINK_BYTES {
        return Err(crate::i18n::t("files-received-link-too-large"));
    }
    let Ok(uri) = String::from_utf8(bytes) else {
        return Err(crate::i18n::t("files-received-link-invalid-text"));
    };
    let uri = uri.trim_end_matches(['\r', '\n']);
    if uri.is_empty() {
        return Err(crate::i18n::t("files-received-link-empty"));
    }
    Ok(uri.to_owned())
}

/// Admit a copied bearer link through the inherited recipient service.
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
    let bytes = fs_protocol::canonical::canonical_bytes(&request)
        .map_err(|_| t("files-received-encode-failed"))?;
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
    fs_protocol::canonical::from_canonical_bytes(&response)
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

#[cfg(test)]
#[cfg(unix)]
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
            let body = fs_protocol::canonical::canonical_bytes(&RecipientControlRequestV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                operation: operation.clone(),
            })
            .unwrap();
            let response = fs_protocol::canonical::canonical_bytes(&RecipientControlResponseV1 {
                protocol_version: RECIPIENT_CONTROL_VERSION,
                result: RecipientControlResultV1::Error {
                    code: fs_protocol::RecipientControlErrorCode::NotFound,
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
                    code: fs_protocol::RecipientControlErrorCode::NotFound
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

    /// A link on standard input is read whole, without its line ending;
    /// empty, oversized and non-text input is refused.
    #[test]
    fn a_link_on_standard_input_is_read_and_checked() {
        crate::i18n::set_locale("en-US");
        assert_eq!(
            read_link(&b"tilo-file://v2/abc#key\n"[..]).unwrap(),
            "tilo-file://v2/abc#key"
        );
        assert!(read_link(&b"\n"[..]).is_err());
        assert!(read_link(&[0xff, 0xfe][..]).is_err());
        assert!(read_link(&vec![b'a'; MAX_LINK_BYTES + 1][..]).is_err());
    }
}
