//! The unofficial subscription sign-ins against the live services.
//!
//! Signing in with a `ChatGPT` account and with GitHub for Copilot uses the
//! public clients of `OpenAI`'s and GitHub's own tools (`oauth-clients.toml`),
//! which either company may change or close at any time. These canaries sign
//! nothing in and spend nothing: they check that each client, redirect and
//! endpoint Tilo relies on still answers the way the sign-in expects, so a
//! change shows up here before people meet it in Settings.
//!
//! They reach the internet, so ordinary runs skip them; the
//! `nightly-live-sign-in` check runs them on a schedule.

use std::time::Duration;

use account_custody::{COPILOT_TOKEN_ENDPOINT, Capability, ClientTable, OAuthIssuer, OAuthUse};
use agent_provider_config::presets::CHATGPT_ENDPOINT;

const CHATGPT: OAuthUse = OAuthUse::new(OAuthIssuer::OpenAi, Capability::Models);
const COPILOT: OAuthUse = OAuthUse::new(OAuthIssuer::GitHub, Capability::Models);

/// The client every request is made with: no redirects followed, so a
/// moved endpoint shows as itself.
fn http() -> Result<reqwest::blocking::Client, reqwest::Error> {
    tilo_net::http::blocking_client()
        .timeout(Duration::from_secs(30))
        .redirect(reqwest::redirect::Policy::none())
        .build()
}

/// The `OAuth` error code of a token endpoint answer.
fn error_code(body: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = value.get("error")?;
    error
        .as_str()
        .or_else(|| error.get("code").and_then(serde_json::Value::as_str))
        .map(str::to_owned)
}

#[test]
#[ignore = "reaches OpenAI's live sign-in and ChatGPT backend; the nightly-live-sign-in check runs it"]
fn chatgpt_sign_in_client_and_backend_still_answer_as_expected() {
    let (provider, client) = ClientTable::defaults()
        .config(CHATGPT)
        .expect("the ChatGPT sign-in is configured");
    let http = http().expect("an HTTPS client");

    // A made-up code is refused as a bad grant, not as an unknown client
    // or redirect: the client id and its registered redirect still stand.
    let port = match &client.redirect_ports {
        tilo_oauth::RedirectPorts::Registered(ports) => ports.first().copied(),
        tilo_oauth::RedirectPorts::Any => None,
    }
    .expect("OpenAI matches a registered port");
    let redirect = format!(
        "http://{}:{port}{}",
        client.redirect.host, client.redirect.path
    );
    let verifier = "canary-verifier-0123456789abcdefghijklmnopqrstuvwxyzABCDEFG";
    let response = http
        .post(&provider.token_endpoint)
        .form(&[
            ("grant_type", "authorization_code"),
            ("client_id", client.client_id.as_str()),
            ("redirect_uri", redirect.as_str()),
            ("code", "canary-not-a-code"),
            ("code_verifier", verifier),
        ])
        .send()
        .expect("OpenAI's token endpoint answers");
    let status = response.status().as_u16();
    let body = response.text().unwrap_or_default();
    let code = error_code(&body);
    assert!(
        (400..500).contains(&status) && status != 404,
        "the token endpoint moved or failed: HTTP {status}"
    );
    assert!(
        !matches!(
            code.as_deref(),
            Some("invalid_client" | "unauthorized_client" | "unsupported_grant_type")
        ),
        "OpenAI no longer accepts the ChatGPT client: {code:?}"
    );

    // A made-up refresh token is likewise a bad grant: refreshing still
    // works the way custody refreshes.
    let response = http
        .post(&provider.token_endpoint)
        .form(&[
            ("grant_type", "refresh_token"),
            ("client_id", client.client_id.as_str()),
            ("refresh_token", "canary-not-a-refresh-token"),
        ])
        .send()
        .expect("OpenAI's token endpoint answers a refresh");
    let status = response.status().as_u16();
    let code = error_code(&response.text().unwrap_or_default());
    assert!(
        (400..500).contains(&status) && status != 404,
        "refreshing moved or failed: HTTP {status}"
    );
    assert!(
        !matches!(
            code.as_deref(),
            Some("invalid_client" | "unauthorized_client" | "unsupported_grant_type")
        ),
        "OpenAI no longer refreshes the ChatGPT client's grants: {code:?}"
    );

    // The subscription backend still serves its Responses route, and
    // refuses a request without a sign-in rather than not knowing it.
    for (method, path) in [
        ("POST", "/responses"),
        ("GET", "/models?client_version=1.0.0"),
    ] {
        let url = format!("{CHATGPT_ENDPOINT}{path}");
        let request = match method {
            "POST" => http.post(&url).json(&serde_json::json!({
                "model": "gpt-5-codex", "instructions": "", "input": [], "stream": true, "store": false
            })),
            _ => http.get(&url),
        };
        let status = request
            .send()
            .expect("the ChatGPT backend answers")
            .status()
            .as_u16();
        assert!(
            matches!(status, 401 | 403),
            "the ChatGPT backend's {path} answers HTTP {status} without a sign-in"
        );
    }
}

#[test]
#[ignore = "reaches GitHub's live device sign-in and Copilot token endpoint; the nightly-live-sign-in check runs it"]
fn copilot_device_sign_in_and_token_exchange_still_answer_as_expected() {
    let (provider, client) = ClientTable::defaults()
        .config(COPILOT)
        .expect("the Copilot sign-in is configured");
    let http = http().expect("an HTTPS client");
    // GitHub still hands this client a device code for the scope asked.
    let response = http
        .post(
            provider
                .device_authorization_endpoint
                .as_deref()
                .expect("GitHub signs in by device code"),
        )
        .header("Accept", "application/json")
        .form(&[
            ("client_id", client.client_id.as_str()),
            ("scope", &provider.scope()),
        ])
        .send()
        .expect("GitHub's device endpoint answers");
    let status = response.status().as_u16();
    let body: serde_json::Value = response.json().unwrap_or_default();
    assert_eq!(
        status, 200,
        "GitHub's device endpoint answered HTTP {status}"
    );
    for field in ["device_code", "user_code", "verification_uri"] {
        assert!(
            body.get(field)
                .and_then(serde_json::Value::as_str)
                .is_some(),
            "GitHub's device answer has no {field}: the Copilot client may be gone"
        );
    }
    // The Copilot token endpoint is still there, refusing a made-up token.
    let status = http
        .get(COPILOT_TOKEN_ENDPOINT)
        .header("Authorization", "token canary-not-a-token")
        .header("Accept", "application/json")
        .header("editor-version", "vscode/1.107.0")
        .header("user-agent", "GitHubCopilotChat/0.35.0")
        .send()
        .expect("the Copilot token endpoint answers")
        .status()
        .as_u16();
    assert_eq!(
        status, 401,
        "the Copilot token endpoint answered HTTP {status} to a made-up token"
    );
}
