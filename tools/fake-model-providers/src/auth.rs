//! `OpenAI`'s sign-in the way Codex's `login --device-auth` does it: a one-time code, the
//! page the person enters it on, the polling that is handed a PKCE grant, and the token
//! endpoint's four grants.
//!
//! The device exchange (`POST …/deviceauth/token`) answers 403 until the code was entered,
//! like the real sign-in, so a server's polling loop is exercised for real.

use serde_json::json;
use sha2::Digest as _;

use crate::{Answer, DeviceAuth, Granted, Recorded, State, tokens};

/// The routes under `/auth`.
pub(crate) fn route(state: &mut State, request: &Recorded, rest: &str) -> Answer {
    match (request.method.as_str(), rest) {
        ("POST", "/api/accounts/deviceauth/usercode") => usercode(state, request),
        ("POST", "/api/accounts/deviceauth/token") => entered(state, request),
        ("GET", "/codex/device") => page(),
        ("POST", "/codex/device") => submit(state, request),
        ("POST", "/oauth/token") => token(state, request),
        _ => Answer::json(404, &json!({"error": "not found"})),
    }
}

/// Ask for a one-time code: the sign-in a server polls with.
fn usercode(state: &mut State, request: &Recorded) -> Answer {
    let client = request.field("client_id").unwrap_or_default();
    if client.is_empty() {
        return Answer::json(400, &json!({"error": "invalid_client"}));
    }
    let id = state.mint();
    let device_auth_id = format!("dev-{id:016x}");
    let user_code = code(id);
    state.device_auths.push(DeviceAuth {
        id: device_auth_id.clone(),
        user_code: user_code.clone(),
        approved: false,
        granted: None,
    });
    Answer::json(
        200,
        &json!({
            "device_auth_id": device_auth_id,
            "user_code": user_code,
            // `OpenAI` writes the interval between two asks as a string.
            "interval": "1",
        }),
    )
}

/// `user_code` in the shape `OpenAI` uses: eight letters and digits in two groups.
fn code(seed: u64) -> String {
    const ALPHABET: &[u8] = b"ABCDEFGHJKLMNPQRSTUVWXYZ23456789";
    let mut rest = seed.wrapping_mul(0x9E37_79B9_7F4A_7C15);
    let mut code = String::with_capacity(9);
    for place in 0..8 {
        if place == 4 {
            code.push('-');
        }
        let at = usize::try_from(rest % 32).unwrap_or_default();
        code.push(char::from(ALPHABET[at]));
        rest /= 32;
    }
    code
}

/// The page the person enters their code on.
fn page() -> Answer {
    Answer::html(
        200,
        "<!doctype html><html><head><meta charset=\"utf-8\"><title>Sign in to OpenAI</title>\
         <style>body{font:16px system-ui;background:#0d0d0d;color:#ececec;display:grid;\
         place-items:center;height:100vh;margin:0}main{background:#171717;padding:32px;\
         border-radius:12px;width:360px}form{display:grid;gap:12px;margin-top:16px}\
         input{font:inherit;padding:10px;border-radius:8px;border:1px solid #444;\
         background:#0d0d0d;color:inherit;letter-spacing:.15em;text-transform:uppercase}\
         button{font:inherit;padding:10px 16px;border:0;border-radius:8px;background:#10a37f;\
         color:#fff;cursor:pointer}</style></head><body><main>\
         <h1>Enter your code</h1>\
         <p>Type the code shown where you started signing in.</p>\
         <form method=\"post\" action=\"/auth/codex/device\">\
         <label for=\"code\">Device code</label>\
         <input id=\"code\" name=\"code\" autocomplete=\"one-time-code\" autofocus required>\
         <button type=\"submit\">Continue</button></form></main></body></html>",
    )
}

/// The page's form: enter a code and the sign-in it belongs to is approved.
fn submit(state: &mut State, request: &Recorded) -> Answer {
    let typed = request.field("code").unwrap_or_default().trim();
    let found = state
        .device_auths
        .iter_mut()
        .find(|auth| auth.user_code.eq_ignore_ascii_case(typed));
    match found {
        Some(auth) => {
            auth.approved = true;
            Answer::html(
                200,
                "<!doctype html><html><head><meta charset=\"utf-8\">\
                 <title>Signed in</title></head><body><h1>Device signed in</h1>\
                 <p>You can close this window and return to Tilo.</p></body></html>",
            )
        }
        None => Answer::html(
            400,
            "<!doctype html><html><head><meta charset=\"utf-8\">\
             <title>Wrong code</title></head><body><h1>That code is not valid</h1>\
             <p>Check the code and try again.</p>\
             <p><a href=\"/auth/codex/device\">Enter another code</a></p></body></html>",
        ),
    }
}

/// The polling: the grant once the code was entered, 403 before, as `OpenAI` answers.
fn entered(state: &mut State, request: &Recorded) -> Answer {
    let id = request.field("device_auth_id").unwrap_or_default();
    let user_code = request.field("user_code").unwrap_or_default();
    let Some(index) = state
        .device_auths
        .iter()
        .position(|auth| auth.id == id && auth.user_code.eq_ignore_ascii_case(user_code))
    else {
        return Answer::json(404, &json!({"error": "device_auth_not_found"}));
    };
    if !state.device_auths[index].approved {
        return Answer::json(403, &json!({"error": "authorization_pending"}));
    }
    if state.device_auths[index].granted.is_none() {
        let seed = state.mint();
        let verifier = format!("{seed:016x}{:016x}", state.mint());
        let granted = Granted {
            authorization_code: format!("ac-{seed:016x}"),
            code_challenge: challenge(&verifier),
            code_verifier: verifier,
        };
        state.codes.insert(
            granted.authorization_code.clone(),
            granted.code_verifier.clone(),
        );
        state.device_auths[index].granted = Some(granted);
    }
    let Some(granted) = state.device_auths[index].granted.clone() else {
        return Answer::json(500, &json!({"error": "device_auth_unreadable"}));
    };
    Answer::json(
        200,
        &json!({
            "authorization_code": granted.authorization_code,
            "code_challenge": granted.code_challenge,
            "code_verifier": granted.code_verifier,
        }),
    )
}

/// base64url of the verifier's SHA-256, without padding, as PKCE writes it.
fn challenge(verifier: &str) -> String {
    use base64::Engine as _;
    let digest = sha2::Sha256::digest(verifier.as_bytes());
    base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(digest)
}

/// The token endpoint: a sign-in's code, a refresh, or an API key's exchange.
fn token(state: &mut State, request: &Recorded) -> Answer {
    match request.field("grant_type").unwrap_or_default() {
        "authorization_code" => authorization_code(state, request),
        "refresh_token" => refresh(state, request),
        "urn:ietf:params:oauth:grant-type:token-exchange" => exchange(state, request),
        "" => Answer::json(400, &json!({"error": "invalid_request"})),
        _ => Answer::json(400, &json!({"error": "unsupported_grant_type"})),
    }
}

/// The code, its verifier and the redirect address it was issued for.
fn authorization_code(state: &mut State, request: &Recorded) -> Answer {
    let code = request.field("code").unwrap_or_default();
    let verifier = request.field("code_verifier").unwrap_or_default();
    let redirect = request.field("redirect_uri").unwrap_or_default();
    let expected = format!("{}/deviceauth/callback", state.issuer());
    let Some(issued) = state.codes.get(code) else {
        return Answer::json(400, &json!({"error": "invalid_grant"}));
    };
    if issued != verifier || redirect != expected {
        return Answer::json(400, &json!({"error": "invalid_grant"}));
    }
    state.codes.remove(code);
    Answer::json(200, &tokens(state))
}

/// A refresh rotates both of its tokens; a reused one is refused, as `OpenAI` does.
fn refresh(state: &mut State, request: &Recorded) -> Answer {
    let asked = request.field("refresh_token").unwrap_or_default();
    if !state.refresh.contains(asked) {
        return if state.used_refresh.contains(asked) {
            Answer::json(401, &json!({"error": "refresh_token_reused"}))
        } else {
            Answer::json(401, &json!({"error": "invalid_grant"}))
        };
    }
    state.refresh.remove(asked);
    state.used_refresh.insert(asked.to_owned());
    state.refresh_count = state.refresh_count.saturating_add(1);
    Answer::json(200, &tokens(state))
}

/// The account's `id_token` exchanged for an API key, as Codex gets one for `OpenAI`'s API.
fn exchange(state: &mut State, request: &Recorded) -> Answer {
    if request.field("requested_token") != Some("openai-api-key") {
        return Answer::json(400, &json!({"error": "invalid_request"}));
    }
    let subject = request.field("subject_token").unwrap_or_default();
    if crate::jwt_claims(subject).is_none() {
        return Answer::json(400, &json!({"error": "invalid_request"}));
    }
    if state.refuse_api_key {
        return Answer::json(
            401,
            &json!({
                "error": "api_key_unavailable",
                "error_description": "This account has no API organization for an API key.",
            }),
        );
    }
    let id = state.mint();
    let key = format!("{}{id:016x}", crate::API_KEY_PREFIX);
    state.api_keys.insert(key.clone());
    Answer::json(
        200,
        &json!({"access_token": key, "token_type": "Bearer", "expires_in": state.expires_in}),
    )
}
