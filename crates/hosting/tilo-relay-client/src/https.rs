//! What a page asks a host before it holds a session, over HTTPS through its
//! `fetch`: the host's certificate and address, certification, pairing, and
//! joining a workspace.
//!
//! Everything after that is the session, over iroh
//! (`tilo_relay_session::RelayClient`).

use std::future::Future;
use std::pin::Pin;

use tilo_relay_protocol::certify::CertifyReply;
use tilo_relay_protocol::web::API_PREFIX;
use tilo_user_identity::CertificationRequest;

use crate::LinkError;

/// A pending HTTP answer: status and body.
#[cfg(not(target_family = "wasm"))]
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), String>> + Send + 'a>>;
/// A pending HTTP answer: status and body.
#[cfg(target_family = "wasm")]
pub type HttpFuture<'a> = Pin<Box<dyn Future<Output = Result<(u16, Vec<u8>), String>> + 'a>>;

/// `POST` to the host origin. The page implements it with `fetch`; native
/// peers and tests with a Rust client. `Err` means the host was not reached.
pub trait HttpClient: crate::MaybeSendSync {
    /// Post `body` to `path` (below the host origin).
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_>;
}

fn path(route: &str) -> String {
    format!("{API_PREFIX}{route}")
}

async fn call<T: serde::de::DeserializeOwned>(
    client: &dyn HttpClient,
    route: &str,
    body: Vec<u8>,
) -> Result<T, LinkError> {
    let bytes = call_raw(client, route, body).await?;
    postcard::from_bytes(&bytes).map_err(|error| LinkError::Malformed(error.to_string()))
}

async fn call_raw(
    client: &dyn HttpClient,
    route: &str,
    body: Vec<u8>,
) -> Result<Vec<u8>, LinkError> {
    let (status, bytes) = client
        .post(&path(route), body)
        .await
        .map_err(LinkError::Unavailable)?;
    match status {
        200 => Ok(bytes),
        401 | 403 => Err(LinkError::Refused(
            String::from_utf8_lossy(&bytes).into_owned(),
        )),
        _ => Err(LinkError::Unavailable(format!("status {status}"))),
    }
}

fn encode<T: serde::Serialize>(value: &T) -> Result<Vec<u8>, LinkError> {
    postcard::to_allocvec(value).map_err(|error| LinkError::Malformed(error.to_string()))
}

/// What the host publishes before any session: its certificate.
pub async fn host_info(
    client: &dyn HttpClient,
) -> Result<tilo_relay_protocol::web::HostInfo, LinkError> {
    call(client, "/host", Vec::new()).await
}

/// Ask the host to queue `request` for its root holder. Returns the pairing
/// code, or the final answer when the host refused at once.
pub async fn certify_request(
    client: &dyn HttpClient,
    request: &CertificationRequest,
) -> Result<CertifyReply, LinkError> {
    call(client, "/certify", encode(request)?).await
}

/// Send a member's root-signed approver command (their pending device
/// requests, or a decision on one).
pub async fn certify_approve(
    client: &dyn HttpClient,
    approval: &tilo_relay_protocol::certify::MemberApproval,
) -> Result<tilo_relay_protocol::certify::ApproverReply, LinkError> {
    call(client, "/certify/approve", encode(approval)?).await
}

/// Long-poll the answer to a queued request. `Pending` means poll again.
pub async fn certify_wait(
    client: &dyn HttpClient,
    digest: [u8; 32],
) -> Result<CertifyReply, LinkError> {
    call(client, "/certify/wait", encode(&digest)?).await
}

/// One step of joining a workspace with no session (`/join`).
pub async fn join(
    client: &dyn HttpClient,
    step: &tilo_relay_protocol::workspaces::JoinStep,
) -> Result<tilo_relay_protocol::workspaces::JoinReply, LinkError> {
    call(client, "/join", encode(step)?).await
}
