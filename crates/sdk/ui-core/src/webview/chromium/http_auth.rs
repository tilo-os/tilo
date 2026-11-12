//! A server's sign-in, between the host and the embedder
//! ([`crate::webview::http_auth`]).

use std::sync::atomic::Ordering;

use serde_json::{Value, json};

use super::ChromiumEngine;
use crate::webview::http_auth::{HttpAuthChallenge, answers};
use crate::webview::{WebViewCommand, WebViewEvent};

impl ChromiumEngine {
    /// Tell the host this tab answers sign-ins, once it asked and the host
    /// said it holds them for its embedder.
    pub(super) fn tell_http_auth(&self) {
        let shared = &self.shared;
        if !shared.auth_asked.load(Ordering::Acquire)
            || !shared.auth_can.load(Ordering::Acquire)
            || shared.auth_told.swap(true, Ordering::AcqRel)
        {
            return;
        }
        if self
            .tx
            .push(json!({"op":"http_auth","tab":self.tab,"enabled":true}))
            .is_err()
        {
            shared.auth_told.store(false, Ordering::Release);
        }
    }
}

/// An `http_auth` event: who asks for credentials, within the host's bounds
/// and of the shape an asker has.
pub(super) fn decode(value: &Value) -> Option<HttpAuthChallenge> {
    let challenge = HttpAuthChallenge {
        id: u32::try_from(value["id"].as_u64()?)
            .ok()
            .filter(|id| i32::try_from(*id).is_ok())?,
        asker: value["asker"].as_str()?.to_owned(),
        proxy: value["proxy"].as_bool()?,
        realm: value["realm"].as_str()?.to_owned(),
        scheme: value["scheme"].as_str()?.to_owned(),
        retry: value["retry"].as_bool()?,
    };
    challenge.well_formed().then_some(challenge)
}

/// What the host said of a sign-in: that a request waits on one, or that
/// it no longer does.
pub(super) fn event(value: &Value) -> Option<WebViewEvent> {
    Some(match value["event"].as_str()? {
        "http_auth" => WebViewEvent::HttpAuth(decode(value)?),
        "http_auth_closed" => WebViewEvent::HttpAuthClosed {
            id: u32::try_from(value["id"].as_u64()?).ok()?,
        },
        _ => return None,
    })
}

/// Whether `command` answers a sign-in in a way the host takes: anything
/// else would end the engine, so it is never sent.
pub(super) fn within_limits(command: &WebViewCommand) -> bool {
    matches!(command, WebViewCommand::AnswerHttpAuth { id, asker, credentials }
        if *id > 0 && i32::try_from(*id).is_ok() && answers(asker, credentials.as_ref()))
}

/// The answer to a sign-in as the host reads it: the challenge, the asker
/// the person was shown, and the credentials, or none to cancel.
pub(super) fn encode(command: &WebViewCommand) -> Option<Value> {
    let WebViewCommand::AnswerHttpAuth {
        id,
        asker,
        credentials,
    } = command
    else {
        return None;
    };
    Some(match credentials {
        Some(credentials) => json!({
            "op": "http_auth_reply", "id": id, "asker": asker,
            "username": credentials.username(), "password": credentials.password(),
        }),
        None => json!({"op": "http_auth_reply", "id": id, "asker": asker}),
    })
}
