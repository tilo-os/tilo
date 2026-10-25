//! HTTP in a page: `fetch`, streaming the body from its `ReadableStream`.

use std::time::Duration;

use ::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use bytes::Bytes;
use js_sys::{Reflect, Uint8Array};
use send_wrapper::SendWrapper;
use tilo_clock::Instant;
use url::Url;
use wasm_bindgen::{JsCast as _, JsValue};
use web_sys::{
    AbortController, ReadableStreamDefaultReader, ReferrerPolicy, RequestCredentials, RequestInit,
    RequestRedirect, ResponseType,
};

use super::{deadline, get, global, message, promise};
use crate::client::{Config, RawResponse, Redirects};
use crate::proxy::{self, ProxyUse};
use crate::{NetError, Request};

pub(crate) struct Backend;

impl Backend {
    #[allow(
        clippy::unnecessary_wraps,
        reason = "the same signature as the desktop backend, whose TLS setup can fail"
    )]
    pub(crate) fn new(_config: &Config) -> Result<Self, NetError> {
        Ok(Self)
    }

    #[allow(
        clippy::unused_self,
        reason = "the same signature as the desktop backend"
    )]
    pub(crate) fn send(
        &self,
        config: &Config,
        request: Request,
    ) -> impl Future<Output = Result<RawResponse, NetError>> + Send + use<> {
        SendWrapper::new(send(config.redirects, request))
    }
}

/// Where a request goes: the target itself, or the host's proxy.
struct Route {
    url: Url,
    headers: HeaderMap,
    proxied: bool,
    cross_origin: bool,
}

fn route(request: &Request) -> Result<Route, NetError> {
    if request.peer {
        return peer_route(request);
    }
    let cross_origin = super::page_origin().is_none_or(|origin| origin != request.url.origin());
    if cross_origin
        && request.proxy == Some(ProxyUse::CrossOrigin)
        && let Some(proxy) = proxy::current()
    {
        let (url, headers) = proxy::route(&proxy, &request.url, &request.headers)?;
        return Ok(Route {
            url,
            headers,
            proxied: true,
            cross_origin: false,
        });
    }
    Ok(Route {
        url: request.url.clone(),
        headers: request.headers.clone(),
        proxied: false,
        cross_origin,
    })
}

/// A request for a path on the machine that admitted this page's person:
/// it goes through that host's proxy, which reads the path as the file it
/// is on its own disk. There is nowhere else it could go, so a page with no
/// proxy is told so rather than fetching the path itself.
fn peer_route(request: &Request) -> Result<Route, NetError> {
    let Some(proxy) = proxy::current() else {
        return Err(NetError::InvalidRequest(
            "no workspace host to read this machine's files from".to_owned(),
        ));
    };
    let (url, headers) = proxy::peer_route(&proxy, request.url.path(), &request.headers)?;
    Ok(Route {
        url,
        headers,
        proxied: true,
        cross_origin: false,
    })
}

async fn send(redirects: Redirects, request: Request) -> Result<RawResponse, NetError> {
    let route = route(&request)?;
    let controller = AbortController::new().map_err(|error| NetError::Protocol(message(&error)))?;
    let init = RequestInit::new();
    init.set_method(request.method.as_str());
    let headers = web_sys::Headers::new().map_err(|error| NetError::Protocol(message(&error)))?;
    for (name, value) in &route.headers {
        // The browser drops the headers it will not let a page set.
        if let Ok(value) = value.to_str() {
            let _ = headers.append(name.as_str(), value);
        }
    }
    init.set_headers(&headers);
    if let Some(body) = &request.body {
        init.set_body(&Uint8Array::from(body.as_ref()).into());
    }
    init.set_redirect(match redirects {
        Redirects::Follow(_) => RequestRedirect::Follow,
        Redirects::None => RequestRedirect::Manual,
    });
    // The host (and its proxy) authenticates the page by its own session:
    // its cookies on the page's origin, the session header on the workspace
    // host's. Other sites never see the page's cookies.
    init.set_credentials(if route.proxied || !route.cross_origin {
        RequestCredentials::SameOrigin
    } else {
        RequestCredentials::Omit
    });
    // The page sends no referrer, since its address can carry secrets (an
    // invite, a ticket). A site called directly still learns which origin
    // calls it, as CORS's `Origin` already says, and some require it: the
    // OpenStreetMap tile servers answer a request without one with a
    // "blocked" tile.
    if route.cross_origin {
        init.set_referrer_policy(ReferrerPolicy::StrictOrigin);
    }
    init.set_signal(Some(&controller.signal()));
    let outgoing = web_sys::Request::new_with_str_and_init(route.url.as_str(), &init)
        .map_err(|error| NetError::InvalidRequest(message(&error)))?;

    let started = Instant::now();
    let fetched = deadline(request.timeout, async {
        let global = global().ok_or_else(|| NetError::connect("this page has no fetch"))?;
        let fetch: js_sys::Function = get(&global, "fetch")
            .and_then(|fetch| fetch.dyn_into().ok())
            .ok_or_else(|| NetError::connect("this page has no fetch"))?;
        let pending = fetch
            .call1(&global, &outgoing)
            .map_err(|error| NetError::connect(message(&error)))?;
        promise(js_sys::Promise::resolve(&pending))
            .await
            .map_err(|error| fetch_error(&error, route.cross_origin))
    })
    .await;
    let response: web_sys::Response = match fetched {
        Ok(response) => response.dyn_into().map_err(|_| {
            NetError::Protocol("fetch answered something other than a Response".into())
        })?,
        Err(error) => {
            controller.abort();
            return Err(error);
        }
    };
    if response.type_() == ResponseType::Opaqueredirect {
        return Err(NetError::Redirect);
    }

    let status = StatusCode::from_u16(response.status())
        .map_err(|error| NetError::Protocol(format!("status: {error}")))?;
    let mut headers = read_headers(&response.headers());
    let mut url = Url::parse(&response.url()).unwrap_or_else(|_| route.url.clone());
    let remaining = request
        .timeout
        .map(|timeout| timeout.saturating_sub(started.elapsed()));
    let mut body = Body::new(&response, controller, remaining);
    if route.proxied {
        let failure = if headers.contains_key(proxy::ERROR_HEADER) {
            let text = body.text().await.unwrap_or_default();
            Some(text)
        } else {
            None
        };
        let (unwrapped, final_url) =
            proxy::unwrap_response(&request.url, &headers, || failure.unwrap_or_default())?;
        headers = unwrapped;
        url = final_url;
    }
    Ok(RawResponse {
        status,
        headers,
        url,
        body,
    })
}

/// A rejected `fetch`: an abort from our timeout, or a `TypeError` that the
/// browser gives for every network failure, CORS refusals included.
fn fetch_error(error: &JsValue, cross_origin: bool) -> NetError {
    if super::is_named(error, "AbortError") || super::is_named(error, "TimeoutError") {
        return NetError::Timeout;
    }
    NetError::Connect {
        message: message(error),
        cross_origin,
    }
}

fn read_headers(headers: &web_sys::Headers) -> HeaderMap {
    let mut map = HeaderMap::new();
    let Ok(Some(entries)) = js_sys::try_iter(headers) else {
        return map;
    };
    for entry in entries.flatten() {
        let pair: js_sys::Array = entry.unchecked_into();
        let (Some(name), Some(value)) = (pair.get(0).as_string(), pair.get(1).as_string()) else {
            continue;
        };
        if let (Ok(name), Ok(value)) = (HeaderName::try_from(name), HeaderValue::try_from(value)) {
            map.append(name, value);
        }
    }
    map
}

pub(crate) struct Body(SendWrapper<Stream>);

struct Stream {
    reader: Option<ReadableStreamDefaultReader>,
    controller: AbortController,
    deadline: Option<Instant>,
}

impl Body {
    fn new(
        response: &web_sys::Response,
        controller: AbortController,
        remaining: Option<Duration>,
    ) -> Self {
        let reader = response.body().map(|stream| {
            stream
                .get_reader()
                .unchecked_into::<ReadableStreamDefaultReader>()
        });
        Self(SendWrapper::new(Stream {
            reader,
            controller,
            deadline: remaining.and_then(|remaining| Instant::now().checked_add(remaining)),
        }))
    }

    pub(crate) fn chunk(
        &mut self,
    ) -> impl Future<Output = Result<Option<Bytes>, NetError>> + Send + '_ {
        SendWrapper::new(self.0.chunk())
    }

    async fn text(&mut self) -> Result<String, NetError> {
        let mut all = Vec::new();
        while let Some(chunk) = self.0.chunk().await? {
            all.extend_from_slice(&chunk);
        }
        Ok(String::from_utf8_lossy(&all).into_owned())
    }
}

impl Stream {
    async fn chunk(&mut self) -> Result<Option<Bytes>, NetError> {
        let Some(reader) = self.reader.clone() else {
            return Ok(None);
        };
        let remaining = self
            .deadline
            .map(|deadline| deadline.saturating_duration_since(Instant::now()));
        let read = deadline(remaining, async {
            promise(reader.read())
                .await
                .map_err(|error| NetError::Body(message(&error)))
        })
        .await;
        let result = match read {
            Ok(result) => result,
            Err(error) => {
                self.finish(true);
                return Err(error);
            }
        };
        let done = Reflect::get(&result, &JsValue::from_str("done"))
            .ok()
            .and_then(|done| done.as_bool())
            .unwrap_or(true);
        if done {
            self.finish(false);
            return Ok(None);
        }
        let value = Reflect::get(&result, &JsValue::from_str("value"))
            .map_err(|error| NetError::Body(message(&error)))?;
        let bytes: Uint8Array = value
            .dyn_into()
            .map_err(|_| NetError::Body("a body chunk was not bytes".into()))?;
        Ok(Some(Bytes::from(bytes.to_vec())))
    }

    fn finish(&mut self, abort: bool) {
        if let Some(reader) = self.reader.take() {
            if abort {
                self.controller.abort();
            }
            reader.release_lock();
        }
    }
}

impl Drop for Stream {
    fn drop(&mut self) {
        // An unread body holds the connection; dropping it cancels.
        self.finish(true);
    }
}
