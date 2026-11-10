//! What a page asks a host before it holds a session, over HTTPS through its
//! `fetch`: the host's certificate and address, a member's approval of their
//! own device, and joining a workspace.
//!
//! Everything after that is the session, over iroh
//! (`tilo_relay_session::RelayClient`).

use std::future::Future;
use std::pin::Pin;

use tilo_relay_protocol::web::API_PREFIX;

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

/// The longest body read from a host or a site; a page's fetch reads its own.
#[cfg(not(target_family = "wasm"))]
const MAX_BODY: usize = 4 * 1024 * 1024;

/// HTTPS to one host's or site's origin, as its routes take it: what a native peer posts through,
/// and the `GET` a site's `entry.json` takes.
#[cfg(not(target_family = "wasm"))]
#[derive(Clone)]
pub struct Origin {
    client: tilo_net::Client,
    origin: String,
}

#[cfg(not(target_family = "wasm"))]
impl Origin {
    /// `origin` (`https://eu1.tilo.sh`), reached with `client`.
    #[must_use]
    pub fn new(client: &tilo_net::Client, origin: &str) -> Self {
        Self {
            client: client.clone(),
            origin: origin.trim_end_matches('/').to_owned(),
        }
    }

    /// The origin itself, as links and routes name it.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.origin
    }

    /// The origin `origin`, reached with this one's client.
    #[must_use]
    pub fn sibling(&self, origin: &str) -> Self {
        Self::new(&self.client, origin)
    }
    /// `GET` the route `path` below the origin: its status and body, when the origin answered.
    pub async fn get(&self, path: &str) -> Result<(u16, Vec<u8>), String> {
        let response = self
            .client
            .get(format!("{}{path}", self.origin))
            .send()
            .await
            .map_err(|error| error.to_string())?;
        let status = response.status().as_u16();
        let body = response
            .bytes_limited(MAX_BODY)
            .await
            .map_err(|error| error.to_string())?;
        Ok((status, body.to_vec()))
    }
}

#[cfg(not(target_family = "wasm"))]
impl HttpClient for Origin {
    fn post(&self, path: &str, body: Vec<u8>) -> HttpFuture<'_> {
        let url = format!("{}{path}", self.origin);
        let client = self.client.clone();
        Box::pin(async move {
            let response = client
                .post(url)
                .header("content-type", "application/octet-stream")
                .body(body)
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status().as_u16();
            let body = response
                .bytes_limited(MAX_BODY)
                .await
                .map_err(|error| error.to_string())?;
            Ok((status, body.to_vec()))
        })
    }
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

/// Where the host `info` describes published it is reachable, as a session dials it: its endpoint,
/// with the relay it names when it names one. `None` when it takes no device connections.
#[must_use]
pub fn host_endpoint(info: &tilo_relay_protocol::web::HostInfo) -> Option<iroh_base::EndpointAddr> {
    let address = info.address.as_ref()?;
    let endpoint = match address
        .relay_url
        .as_deref()
        .and_then(|url| url.parse::<iroh_base::RelayUrl>().ok())
    {
        Some(relay) => address.endpoint_addr.clone().with_relay_url(relay),
        None => address.endpoint_addr.clone(),
    };
    Some(endpoint)
}

/// Send a member's root-signed approver command (their pending device
/// requests, or a decision on one).
pub async fn certify_approve(
    client: &dyn HttpClient,
    approval: &tilo_relay_protocol::certify::MemberApproval,
) -> Result<tilo_relay_protocol::certify::ApproverReply, LinkError> {
    call(client, "/certify/approve", encode(approval)?).await
}

/// One step of joining a workspace with no session (`/join`).
pub async fn join(
    client: &dyn HttpClient,
    step: &tilo_relay_protocol::workspaces::JoinStep,
) -> Result<tilo_relay_protocol::workspaces::JoinReply, LinkError> {
    call(client, "/join", encode(step)?).await
}
