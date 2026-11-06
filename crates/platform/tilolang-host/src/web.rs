//! The network `web.get` and `web.post` reach from an app this process runs: this device's,
//! once per request, with the runtime's limit on how long one may take.
//!
//! A timeout, no network and a refused connection answer with status 0 and say why: the app
//! sees a value, never a trap.

use tilolang_tilo::runtime::flow::WebRequest;
use tilolang_tilo::runtime::web::{TIMEOUT, WebReply};

/// Fetch `request` over this device's network.
pub fn fetch(request: &WebRequest) -> WebReply {
    let client = match tilo_net::http::blocking_client()
        .timeout(TIMEOUT)
        .user_agent(concat!("tilo/", env!("CARGO_PKG_VERSION")))
        .build()
    {
        Ok(client) => client,
        Err(error) => return WebReply::failed(format!("no connection: {error}")),
    };
    let mut call = if request.method == "POST" {
        client.post(&request.url)
    } else {
        client.get(&request.url)
    };
    for line in &request.headers {
        if let Some((name, value)) = line.split_once(':') {
            call = call.header(name.trim(), value.trim());
        }
    }
    if let Some(tag) = &request.if_none_match {
        call = call.header("If-None-Match", tag);
    }
    if let Some(body) = &request.body {
        call = call.body(body.clone());
    }
    let response = match call.send() {
        Ok(response) => response,
        Err(error) if error.is_timeout() => {
            return WebReply::failed(format!("no answer within {} seconds", TIMEOUT.as_secs()));
        }
        Err(error) => return WebReply::failed(format!("no answer: {error}")),
    };
    let status = response.status();
    let headers = response
        .headers()
        .iter()
        .map(|(name, value)| format!("{name}: {}", value.to_str().unwrap_or_default()))
        .collect();
    let body = response.bytes().map_or_else(|_| Vec::new(), |b| b.to_vec());
    WebReply {
        status: status.as_u16(),
        status_text: status.canonical_reason().unwrap_or_default().to_owned(),
        headers,
        body,
    }
}
