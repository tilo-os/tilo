//! Room-scoped MCP on the relay's existing HTTPS listener.
//!
//! A bearer credential is an existing certified browser session, not a new
//! shared host secret. Each credential/room pair has a separate MCP endpoint:
//! knowing another client's MCP session id conveys no authority.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use http_body_util::BodyExt as _;
use hyper::{Method, Request, Response, StatusCode, header};
use tilo_actions::InvocationId;
use tilo_actions::remote::{
    ActionHost, ActionHostError, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeRequest, Pending, PendingPoll, RemoteActionMessage,
};
use tilo_runtime::{mcp::McpServer, mcp_http::Endpoint};

use super::sessions::{Session, Sessions};
use super::{Body, Web, body, reply, session_status, status};
use crate::room_access::{self, RoomOperation};

const MCP_HEADERS: &str =
    "authorization, content-type, mcp-session-id, mcp-protocol-version, x-tilo-session";
type Endpoints = HashMap<([u8; 32], String), Arc<Endpoint<RoomHost>>>;

pub(super) struct RoomsMcp {
    endpoints: Mutex<Endpoints>,
    // Only initialization is serialized; status and invoke remain concurrent.
    admission: tokio::sync::Mutex<()>,
    limit: usize,
}

impl RoomsMcp {
    pub(super) fn new(limit: usize) -> Self {
        Self {
            endpoints: Mutex::default(),
            admission: tokio::sync::Mutex::new(()),
            limit,
        }
    }

    fn endpoint(&self, host: RoomHost, room: String) -> Option<Arc<Endpoint<RoomHost>>> {
        let mut endpoints = self.endpoints.lock().ok()?;
        let key = (host.session.token(), room.clone());
        endpoints.retain(|cached, endpoint| {
            host.sessions.get(&cached.0).is_ok() && (cached == &key || endpoint.session_count() > 0)
        });
        if let Some(endpoint) = endpoints.get(&key) {
            return Some(Arc::clone(endpoint));
        }
        if endpoints.len() >= self.limit {
            return None;
        }
        let actor = hex::encode(host.session.peer.user_public_key);
        let endpoint = Endpoint::new(host.clone(), room.clone(), actor, None, None);
        endpoint
            .serve_sessions(Arc::new(move || {
                let peer = &host.session.peer;
                let caller = Caller::authenticated(peer.user_public_key, peer.device_public_key)
                    .from_foreign_device(peer.cert.capabilities.foreign)
                    .in_room(room.clone())
                    .in_new_execution();
                McpServer::admitted(host.clone(), caller)
            }))
            .ok()?;
        endpoints.insert(key, Arc::clone(&endpoint));
        Some(endpoint)
    }

    fn session_count(&self) -> usize {
        self.endpoints.lock().map_or(self.limit, |endpoints| {
            endpoints
                .values()
                .map(|endpoint| endpoint.session_count())
                .sum()
        })
    }
}

pub(super) fn is_route(path: &str) -> bool {
    path.starts_with("/peer/v1/rooms/") && path.ends_with("/mcp")
}

fn room(path: &str) -> Option<String> {
    let room = path.strip_prefix("/peer/v1/rooms/")?.strip_suffix("/mcp")?;
    let id = uuid::Uuid::parse_str(room).ok()?;
    (!id.is_nil() && id.to_string() == room).then(|| room.to_owned())
}

fn credential(headers: &hyper::HeaderMap) -> Option<[u8; 32]> {
    let value = if let Some(value) = headers.get(header::AUTHORIZATION) {
        value.to_str().ok()?.strip_prefix("Bearer ")?
    } else {
        headers.get(super::SESSION_HEADER)?.to_str().ok()?
    };
    hex::decode(value).ok()?.try_into().ok()
}

pub(super) async fn handle(web: &Web, request: Request<hyper::body::Incoming>) -> Response<Body> {
    let origin = request.headers().get(header::ORIGIN).cloned();
    // CORS alone does not reject an Origin: explicitly enforce the MCP
    // transport's origin check before reading credentials or JSON.
    if origin.as_ref().is_some_and(|origin| {
        origin.to_str().map_or(true, |origin| {
            !web.allowed_origins.iter().any(|allowed| allowed == origin)
        })
    }) {
        return status(StatusCode::FORBIDDEN, "origin not allowed");
    }
    let mut response = if request.method() == Method::OPTIONS {
        reply(StatusCode::NO_CONTENT, Vec::new())
    } else {
        respond(web, request)
            .await
            .unwrap_or_else(|response| response)
    };
    super::cors(
        &web.allowed_origins,
        MCP_HEADERS,
        origin.as_ref(),
        &mut response,
    );
    if origin.is_some() {
        response.headers_mut().insert(
            header::ACCESS_CONTROL_ALLOW_METHODS,
            header::HeaderValue::from_static("POST, GET, DELETE, OPTIONS"),
        );
        response.headers_mut().insert(
            header::ACCESS_CONTROL_EXPOSE_HEADERS,
            header::HeaderValue::from_static("mcp-session-id"),
        );
    }
    response
}

async fn respond(
    web: &Web,
    request: Request<hyper::body::Incoming>,
) -> Result<Response<Body>, Response<Body>> {
    let room = room(request.uri().path())
        .ok_or_else(|| status(StatusCode::BAD_REQUEST, "invalid room UUID"))?;
    let token = credential(request.headers())
        .ok_or_else(|| status(StatusCode::UNAUTHORIZED, "certified session required"))?;
    let session = web
        .sessions
        .get(&token)
        .map_err(|error| session_status(&error))?;
    let host = RoomHost {
        sessions: Arc::clone(&web.sessions),
        session,
        routes: Arc::default(),
        workspace: crate::room_workspace_id(&room)
            .map_err(|_| status(StatusCode::BAD_REQUEST, "invalid room UUID"))?,
    };
    host.authorize()
        .await
        .map_err(|_| status(StatusCode::FORBIDDEN, "room access denied"))?;
    let serves_apps = host.sessions.context().relay.app_serving.is_some();
    if !serves_apps {
        return Err(status(
            StatusCode::NOT_FOUND,
            "this host does not serve apps",
        ));
    }
    host.session.touch();
    let endpoint = web
        .mcp
        .endpoint(host.clone(), room)
        .ok_or_else(|| status(StatusCode::SERVICE_UNAVAILABLE, "MCP capacity unavailable"))?;
    drop(host);
    let (parts, bytes) = body(request, tilo_actions::remote::MAX_REMOTE_ACTION_FRAME_BYTES).await?;
    let initializing =
        parts.method == Method::POST && !parts.headers.contains_key("mcp-session-id");
    let _admission = if initializing {
        let admission = web.mcp.admission.lock().await;
        if web.mcp.session_count() >= web.mcp.limit {
            return Err(status(
                StatusCode::SERVICE_UNAVAILABLE,
                "MCP capacity unavailable",
            ));
        }
        Some(admission)
    } else {
        None
    };
    let response = endpoint
        .dispatch(&parts.method, "/mcp", &parts.headers, &bytes)
        .await;
    drop(endpoint);
    let (parts, body) = response.into_parts();
    let bytes = body
        .collect()
        .await
        .expect("Full body is infallible")
        .to_bytes();
    Ok(Response::from_parts(parts, Body::Full(Some(bytes))))
}

/// The existing app-serving plane, pinned to the authenticated session and
/// room. Catalogue and invocation admission are rechecked even when one MCP
/// request expands into several action-host calls.
#[derive(Clone)]
struct RoomHost {
    sessions: Arc<Sessions>,
    session: Arc<Session>,
    workspace: u64,
    routes: Arc<Mutex<Providers>>,
}

/// MCP addresses live instances by numeric peer id; room actions address
/// records by their canonical provider. These ids are routing aliases only,
/// allocated locally, never identities or permission grants. Never reuse an
/// id while the credential/room endpoint lives.
#[derive(Default)]
struct Providers {
    names: Vec<String>,
    ids: HashMap<String, usize>,
}

impl Providers {
    fn publish(&mut self, provider: &str) -> Result<String, ActionHostError> {
        if tilo_actions::remote::workspace_provider_room(provider).is_some() {
            return Ok(provider.to_owned());
        }
        if let Some(id) = self.ids.get(provider) {
            return Ok(id.to_string());
        }
        if self.names.len() >= 4096 {
            return Err(ActionHostError::Unavailable {
                reason: "MCP provider capacity unavailable".into(),
            });
        }
        self.names.push(provider.to_owned());
        let id = self.names.len();
        self.ids.insert(provider.to_owned(), id);
        Ok(id.to_string())
    }

    fn resolve(&self, provider: &str) -> Result<String, ActionHostError> {
        if tilo_actions::remote::workspace_provider_room(provider).is_some() {
            return Ok(provider.to_owned());
        }
        provider
            .parse::<usize>()
            .ok()
            .and_then(|id| id.checked_sub(1))
            .and_then(|index| self.names.get(index))
            .cloned()
            .ok_or(ActionHostError::UnknownAction)
    }

    fn page(&mut self, mut page: CataloguePage) -> Result<CataloguePage, ActionHostError> {
        for provider in &mut page.providers {
            if provider.browsable {
                provider.provider = self.publish(&provider.provider)?;
            }
        }
        for row in &mut page.rows {
            row.provider = self.publish(&row.provider)?;
        }
        // View locators retain the real provider: a browser resolves the
        // canonical resource route, not an MCP connection's routing alias.
        Ok(page)
    }
}

impl RoomHost {
    async fn authorize(&self) -> Result<(), ActionHostError> {
        self.sessions
            .get(&self.session.token())
            .map_err(|_| ActionHostError::NotPermitted)?;
        let admitted = {
            let relay = &self.sessions.context().relay;
            // Delegations are distinct principals. Do not promote one to the
            // person's authority; this endpoint currently admits person devices.
            let is_agent = relay
                .agents
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&self.session.peer.device_public_key);
            if is_agent {
                return Err(ActionHostError::NotPermitted);
            }
            let workspace = relay
                .workspaces
                .lock()
                .await
                .get(&self.workspace)
                .cloned()
                .ok_or(ActionHostError::NotPermitted)?;
            let workspace = crate::workspace_io::fresh_membership(&workspace)
                .await
                .map_err(|_| ActionHostError::NotPermitted)?;
            room_access::decide(
                &workspace,
                &relay.trust(),
                &self.session.peer,
                RoomOperation::RemoteAction,
            )
            .admitted()
        };
        if !admitted {
            return Err(ActionHostError::NotPermitted);
        }
        Ok(())
    }

    fn serve(
        &self,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<Pending<RemoteActionMessage>> {
        if !caller.execution_is_live() || !self.session.is_open() {
            return None;
        }
        let relay = &self.sessions.context().relay;
        if relay
            .trust()
            .cert(&self.session.peer, tilo_clock::unix_now_ns())
            .is_err()
            || relay
                .agents
                .read()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .contains_key(&self.session.peer.device_public_key)
        {
            return None;
        }
        relay
            .app_serving
            .as_ref()?
            .serve(self.workspace, caller, message)
    }

    fn immediate(
        &self,
        caller: &Caller,
        message: RemoteActionMessage,
    ) -> Option<RemoteActionMessage> {
        // Observe without a wait and cancellation are synchronous on the
        // relay's room/supervisor planes. Never block an HTTP runtime thread
        // if an embedding plane instead returns a deferred answer.
        match self.serve(caller, message)?.poll_ready() {
            PendingPoll::Ready(answer) => Some(answer),
            PendingPoll::Waiting | PendingPoll::Abandoned => None,
        }
    }
}

impl ActionHost for RoomHost {
    fn query(
        &self,
        caller: &Caller,
        query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionHostError>> {
        let host = self.clone();
        let caller = caller.clone();
        let (done, answer) = tilo_actions::remote::pending();
        tokio::spawn(async move {
            let result = async {
                host.authorize().await?;
                let mut query = query;
                if let Some(provider) = &query.provider {
                    query.provider = Some(
                        host.routes
                            .lock()
                            .unwrap_or_else(std::sync::PoisonError::into_inner)
                            .resolve(provider)?,
                    );
                }
                match host
                    .serve(
                        &caller,
                        RemoteActionMessage::CatalogueRequest {
                            correlation: 0,
                            query,
                        },
                    )
                    .ok_or(ActionHostError::NotPermitted)?
                    .await
                {
                    Some(RemoteActionMessage::CatalogueResponse { result, .. }) => host
                        .routes
                        .lock()
                        .unwrap_or_else(std::sync::PoisonError::into_inner)
                        .page(result?),
                    _ => Err(ActionHostError::NotPermitted),
                }
            }
            .await;
            done.complete(result);
        });
        answer
    }

    fn invoke(
        &self,
        caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionHostError>> {
        let host = self.clone();
        let caller = caller.clone();
        let (done, answer) = tilo_actions::remote::pending();
        tokio::spawn(async move {
            let result = async {
                host.authorize().await?;
                let mut request = request;
                request.target.provider = host
                    .routes
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .resolve(&request.target.provider)?;
                match host
                    .serve(&caller, RemoteActionMessage::Invoke(request))
                    .ok_or(ActionHostError::NotPermitted)?
                    .await
                {
                    Some(RemoteActionMessage::Receipt(receipt)) => Ok(receipt),
                    _ => Err(ActionHostError::NotPermitted),
                }
            }
            .await;
            done.complete(result);
        });
        answer
    }

    fn observe(&self, caller: &Caller, id: &InvocationId) -> Option<InvocationState> {
        match self.immediate(
            caller,
            RemoteActionMessage::Observe {
                invocation: *id,
                wait_ms: None,
                seen: None,
            },
        )? {
            RemoteActionMessage::Update { state, .. } => state,
            _ => None,
        }
    }

    fn cancel(&self, caller: &Caller, id: &InvocationId) -> Result<(), ActionHostError> {
        match self.immediate(caller, RemoteActionMessage::Cancel { invocation: *id }) {
            Some(RemoteActionMessage::CancelResponse { result, .. }) => result,
            _ => Err(ActionHostError::UnknownInvocation),
        }
    }
}

#[cfg(test)]
mod tests;
