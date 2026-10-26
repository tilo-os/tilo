//! Signing in with Discord (the OAuth 2 authorization code flow): the consent
//! page a browser is sent to, the token endpoint the room's host trades
//! the code at, and `/users/@me` for the access token it got.

use percent_encoding::{NON_ALPHANUMERIC, utf8_percent_encode};
use serde_json::{Value, json};

use super::{Answer, Recorded};
use crate::{State, User};

/// The account the consent page authorizes when no test signed one in.
fn signed_in(state: &State) -> User {
    state
        .oauth_user
        .clone()
        .unwrap_or_else(|| User::new("4242", "Quik2007"))
}

fn arg<'a>(query: &'a [(String, String)], name: &str) -> &'a str {
    query
        .iter()
        .find(|(key, _)| key == name)
        .map_or("", |(_, value)| value.as_str())
}

fn encode(text: &str) -> String {
    utf8_percent_encode(text, NON_ALPHANUMERIC).to_string()
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn html(status: u16, body: &str) -> Answer {
    Answer {
        status,
        body: format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><title>Discord</title>\
             <style>body{{font:16px system-ui;background:#313338;color:#f2f3f5;display:grid;place-items:center;height:100vh;margin:0}}\
             main{{background:#2b2d31;padding:32px;border-radius:8px;width:380px}}\
             a{{display:inline-block;padding:10px 18px;border-radius:4px;text-decoration:none;color:#fff;margin-right:8px}}\
             .go{{background:#5865f2}}.no{{background:#4e5058}}</style></head><body><main>{body}</main></body></html>"
        )
        .into_bytes(),
        content_type: "text/html; charset=utf-8".into(),
        headers: Vec::new(),
    }
}

fn redirect(to: String) -> Answer {
    Answer {
        status: 302,
        body: Vec::new(),
        content_type: "text/plain".into(),
        headers: vec![("location", to)],
    }
}

/// The consent page's routes, outside the API; `None` for any other path.
pub(super) fn page(state: &mut State, path: &str, query: &[(String, String)]) -> Option<Answer> {
    match path {
        "/oauth2/authorize" => Some(consent_page(state, query)),
        "/oauth2/authorize/approve" => Some(approve(state, query)),
        _ => None,
    }
}

/// `GET /oauth2/authorize`: who is asking, for what, with Authorize and
/// Cancel.
fn consent_page(state: &State, query: &[(String, String)]) -> Answer {
    if arg(query, "response_type") != "code"
        || arg(query, "client_id").is_empty()
        || arg(query, "redirect_uri").is_empty()
    {
        return html(400, "<h1>Invalid OAuth2 request</h1>");
    }
    let user = signed_in(state);
    let again = query
        .iter()
        .map(|(key, value)| format!("{}={}", encode(key), encode(value)))
        .collect::<Vec<_>>()
        .join("&");
    let cancel = format!(
        "{}?error=access_denied&state={}",
        arg(query, "redirect_uri"),
        encode(arg(query, "state"))
    );
    html(
        200,
        &format!(
            "<p>Signed in as <b id=\"discord-user\">{}</b></p>\
             <h1>An external application wants to access your Discord account</h1>\
             <p>Scope: <code>{}</code></p>\
             <a class=\"no\" id=\"cancel\" href=\"{}\">Cancel</a>\
             <a class=\"go\" id=\"authorize\" href=\"/oauth2/authorize/approve?{}\">Authorize</a>",
            escape(&user.name),
            escape(arg(query, "scope")),
            escape(&cancel),
            escape(&again),
        ),
    )
}

/// `GET /oauth2/authorize/approve`: back to the redirect address with a
/// code good once.
fn approve(state: &mut State, query: &[(String, String)]) -> Answer {
    let redirect_uri = arg(query, "redirect_uri").to_owned();
    if redirect_uri.is_empty() {
        return html(400, "<h1>Invalid OAuth2 request</h1>");
    }
    let code = format!("code-{}", state.snowflake());
    let user = signed_in(state);
    state
        .oauth_codes
        .insert(code.clone(), (user, redirect_uri.clone()));
    redirect(format!(
        "{redirect_uri}?code={}&state={}",
        encode(&code),
        encode(arg(query, "state"))
    ))
}

/// The token endpoint and the account behind an access token; `None` for
/// the bot's routes.
pub(super) fn route(state: &mut State, request: &Recorded, segments: &[&str]) -> Option<Answer> {
    match (request.method.as_str(), segments) {
        ("POST", ["oauth2", "token"]) => Some(token(state, request)),
        ("GET", ["users", "@me"]) => {
            let bearer = request.bearer.as_deref()?;
            Some(match state.oauth_tokens.get(bearer) {
                Some(user) => Answer::json(200, &user.json()),
                None => Answer::unauthorized(),
            })
        }
        _ => None,
    }
}

fn token(state: &mut State, request: &Recorded) -> Answer {
    let field = |name: &str| request.json.get(name).and_then(Value::as_str).unwrap_or("");
    let invalid = |error: &str| Answer::json(400, &json!({"error": error}));
    if request.basic.is_none() {
        return invalid("invalid_client");
    }
    if field("grant_type") != "authorization_code" {
        return invalid("unsupported_grant_type");
    }
    let Some((user, redirect_uri)) = state.oauth_codes.remove(field("code")) else {
        return invalid("invalid_grant");
    };
    if redirect_uri != field("redirect_uri") {
        return invalid("invalid_grant");
    }
    let access = format!("access-{}", state.snowflake());
    state.oauth_tokens.insert(access.clone(), user);
    Answer::json(
        200,
        &json!({
            "access_token": access, "token_type": "Bearer", "expires_in": 604_800,
            "refresh_token": "unused", "scope": "identify",
        }),
    )
}
