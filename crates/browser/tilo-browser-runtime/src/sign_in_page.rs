//! What the sign-in route answers a service with (`web/sign-in.mjs`).
//!
//! The page reads the service's link, shows the person what it asks, and
//! posts a form back; everything signed happens here, on the peer's own
//! paths. The root the page holds certifies this browser's device key, that
//! device proves the person to the service
//! ([`tilo_browser_peer::sign_in_proof`]), and — when the person lets an
//! agent act for them — the root certifies the key the service generated for
//! the grant ([`tilo_browser_peer::issue_agent_grant`]).
//!
//! The certificate the proof carries is issued for this answer alone: a host
//! issues the page's device certificate when it joins a workspace, and a
//! sign-in to a service has no workspace behind it.

use serde::{Deserialize, Serialize};
use tilo_browser_peer::{
    DeviceSigner as _, IssuedRole, encode, grant_label, issue_agent_grant, issue_device_cert,
    sign_in_proof,
};
use tilo_user_identity::{AgentScope, DeviceCapabilities};
use wasm_bindgen::prelude::*;

use crate::peer::{PageSigner, device_key, js_error};

/// The label of the certificate this route issues for the browser's own
/// device key: the page itself, not a workspace host's member.
const BROWSER_LABEL: &str = "Tilo on the web";

/// Beyond this a page's number is no longer a whole millisecond
/// (2^53 - 1): the largest expiry the route reads.
const MAX_MILLISECONDS: f64 = 9_007_199_254_740_991.0;

/// What the service's sign-in link asked for, as the page read and checked
/// it (`web/sign-in.mjs`): the fields of
/// `<web entry>/sign-in?audience=…&challenge=…&return=…&pending=…&client=…&grant_key=…&grant_scope=…&grant_expires=…&grant_kind=…`.
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct Ask {
    /// The service's origin, as it names itself.
    audience: String,
    /// Its challenge, hex.
    challenge: String,
    /// What the service calls the client that asks: its text, never its
    /// markup.
    #[serde(default)]
    client: String,
    /// The key the service generated for the person's grant, hex: present
    /// with the other two grant fields, or with neither.
    #[serde(default)]
    grant_key: Option<String>,
    /// What the grant reaches: `all`, or one workspace's UUID.
    #[serde(default)]
    grant_scope: Option<String>,
    /// When the grant expires, in Unix milliseconds; none for a server key
    /// that lasts until it is revoked.
    #[serde(default)]
    expires_at_ms: Option<f64>,
    /// The grant is a server key the person creates for their own backend:
    /// it reaches all their workspaces and may last until revoked.
    #[serde(default)]
    server_key: bool,
}

#[derive(Serialize)]
struct Answer {
    proof: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    grant: Option<String>,
}

/// This browser's answer to the service: JSON `{proof, grant?}`, each the
/// canonical bytes of what the service decodes, in base64url without
/// padding.
///
/// `root` is the person's unlocked root (`identity.mjs`) and `custody` the
/// device key this page signs with (`device-custody.mjs`); `ask` is JSON:
/// `{audience, challenge, client, grantKey?, grantScope?, expiresAtMs?, serverKey?}`.
///
/// # Errors
///
/// Rejects when the ask is malformed, the root is not unlocked, the grant's
/// scope, expiry or key is refused, or a signature fails.
#[wasm_bindgen(js_name = signInAnswer)]
pub async fn sign_in_answer(
    root: JsValue,
    custody: JsValue,
    ask: String,
) -> Result<String, JsError> {
    let ask: Ask = serde_json::from_str(&ask).map_err(js_error)?;
    let root = PageSigner::from_js(&root)?;
    let device = PageSigner::from_js(&custody)?;
    let challenge = device_key(&ask.challenge)?;
    let cert = issue_device_cert(
        &root,
        device.device_public_key(),
        BROWSER_LABEL,
        IssuedRole::Browser,
    )
    .await
    .map_err(js_error)?;
    let proof = sign_in_proof(cert, &device, &ask.audience, challenge)
        .await
        .map_err(js_error)?;
    let proof = encode(&proof).map_err(js_error)?;
    let grant = match (ask.grant_key, ask.grant_scope, ask.expires_at_ms) {
        (None, None, None) if !ask.server_key => None,
        (Some(key), Some(scope), expires) if expires.is_some() || ask.server_key => {
            let scope = AgentScope::parse(&scope)
                .ok_or_else(|| js_error("a grant's scope is `all` or a workspace id"))?;
            let capabilities = match (ask.server_key, scope) {
                (false, scope) => DeviceCapabilities::agent_grant(scope),
                (true, AgentScope::AllWorkspaces) => DeviceCapabilities::server_key_grant(),
                (true, AgentScope::Workspace(_)) => {
                    return Err(js_error("a server key reaches all your workspaces"));
                }
            };
            let cert = issue_agent_grant(
                &root,
                device_key(&key)?,
                &grant_label(&ask.client, &ask.audience),
                capabilities,
                expires.map(nanoseconds).transpose()?,
            )
            .await
            .map_err(js_error)?;
            Some(encode(&cert).map_err(js_error)?)
        }
        _ => {
            return Err(js_error(
                "a grant asks for its key, scope and expiry together; only a server key may \
                 last until revoked",
            ));
        }
    };
    serde_json::to_string(&Answer { proof, grant }).map_err(js_error)
}

/// `ms` as Unix nanoseconds, exactly: a whole millisecond the page can hold.
fn nanoseconds(ms: f64) -> Result<u64, JsError> {
    if !(0.0..=MAX_MILLISECONDS).contains(&ms) || ms.fract() != 0.0 {
        return Err(js_error(
            "the grant's expiry is a whole number of milliseconds",
        ));
    }
    #[expect(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "a whole number in range, checked above"
    )]
    let ms = ms as u64;
    ms.checked_mul(1_000_000)
        .ok_or_else(|| js_error("the grant's expiry is out of range"))
}
