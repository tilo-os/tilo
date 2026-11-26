//! Hugging Face searches and downloads for a workspace's AI page, asked of this computer's
//! model manager.
//!
//! The manager is a device helper and keeps no credential: the Hugging Face sign-in is the key
//! of this computer's local model provider in the workspace's `tilo.models`, kept in the
//! workspace's vault. Each request carries that key, and only that request: nothing writes it
//! down, and no answer carries it back.

use std::path::Path;

use agent_provider_config::{EndpointLocation, ProviderConfig, ProviderProfile};
use serde::Deserialize;
use serde_json::{Map, Value, json};
use tilo_workspace_records::models::ModelsAction;

use crate::person_settings::{ProviderKeys, provider_config};

/// Every request and answer the manager's service speaks.
const RPC_VERSION: u16 = 1;
/// The manager's framing: a big-endian `u32` length, then JSON.
const RPC_FRAMING: wire_frame::Framing = wire_frame::Framing::big_endian(1024 * 1024);

/// The request's body for the Hugging Face action `wanted` (a search, a model's description
/// or a download), with the workspace's sign-in when it keeps one.
fn body(wanted: ModelsAction, token: Option<&str>) -> Value {
    let mut action = Map::new();
    match wanted {
        ModelsAction::SearchHubModels { query, format } => {
            action.insert("verb".into(), "hub_search".into());
            action.insert("query".into(), query.into());
            action.insert(
                "format".into(),
                format.unwrap_or_else(|| "gguf".into()).into(),
            );
        }
        ModelsAction::ShowHubModel { model } => {
            action.insert("verb".into(), "hub_model".into());
            action.insert("reference".into(), model.into());
        }
        ModelsAction::InstallHubModel {
            repo,
            revision,
            variant,
            accept_license,
        } => {
            action.insert("verb".into(), "hub_install".into());
            action.insert("repo".into(), repo.into());
            action.insert("revision".into(), revision.into());
            action.insert("variant".into(), variant.into());
            action.insert(
                "accept_license".into(),
                accept_license.unwrap_or_default().into(),
            );
        }
        // The providers send the manager only these three.
        _ => {}
    }
    if let Some(token) = token {
        action.insert("token".into(), token.into());
    }
    json!({ "version": RPC_VERSION, "action": action })
}

/// The manager's answer.
#[derive(Deserialize)]
struct Answer {
    ok: bool,
    #[serde(default)]
    output: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// Send `wanted` to this login's model manager with the workspace's local model provider key.
pub(super) async fn ask(
    (keys, root): (ProviderKeys<'_>, &Path),
    wanted: ModelsAction,
) -> Result<Value, String> {
    ask_at(
        (keys, root),
        &tilo_paths::xdg::service_socket("models"),
        wanted,
    )
    .await
}

/// Send `wanted` to the manager at `socket`, signed in with the workspace's key when it keeps
/// one.
pub(super) async fn ask_at(
    (keys, root): (ProviderKeys<'_>, &Path),
    socket: &Path,
    wanted: ModelsAction,
) -> Result<Value, String> {
    let token = key_of(keys, root).await?;
    let body = body(wanted, token.as_deref().map(String::as_str));
    send_at(socket, &body).await
}

/// The key of this computer's local model provider in the workspace, when it keeps one: the
/// Hugging Face sign-in its downloads use.
async fn key_of(
    keys: ProviderKeys<'_>,
    root: &Path,
) -> Result<Option<zeroize::Zeroizing<String>>, String> {
    let Some(profile) = local_profile(&provider_config(root)) else {
        return Ok(None);
    };
    keys.get(&profile).await
}

/// The model provider this computer runs, as the manager's terminal sign-in names it.
fn local_profile(config: &ProviderConfig) -> Option<ProviderProfile> {
    config
        .profiles
        .iter()
        .find(|profile| profile.location == EndpointLocation::ThisDevice)
        .cloned()
}

/// Send `body` to the manager listening at `socket` and answer what it said, for tests that
/// hand their own socket.
async fn send_at(socket: &Path, body: &Value) -> Result<Value, String> {
    let socket = socket.to_owned();
    let body = serde_json::to_vec(body).map_err(|error| error.to_string())?;
    let answered = tokio::task::spawn_blocking(move || exchange(&socket, &body))
        .await
        .map_err(|_| "the model service stopped before it answered".to_owned())??;
    match answered {
        Some(output) if !output.trim().is_empty() => {
            serde_json::from_str(&output).map_err(|error| {
                format!("the model service answered something that does not read: {error}")
            })
        }
        _ => Ok(Value::Null),
    }
}

/// One request and its answer over the manager's service socket, framed as a `u32` length
/// then JSON, as the manager reads it.
#[cfg(unix)]
fn exchange(socket: &Path, body: &[u8]) -> Result<Option<String>, String> {
    /// How long the manager may take to answer: a search reads Hugging Face first.
    const ANSWER_WAIT: std::time::Duration = std::time::Duration::from_secs(120);

    let mut stream = tilo_files::native::socket::connect(socket).map_err(|error| {
        format!(
            "this computer's model service did not answer ({}): {error}",
            socket.display()
        )
    })?;
    stream
        .set_read_timeout(Some(ANSWER_WAIT))
        .and_then(|()| stream.set_write_timeout(Some(ANSWER_WAIT)))
        .map_err(|error| format!("this computer's model service did not answer: {error}"))?;
    RPC_FRAMING
        .write(&mut stream, body)
        .map_err(|error| format!("this computer's model service did not answer: {error}"))?;
    let Some(answered) = RPC_FRAMING.read(&mut stream).map_err(|error| {
        format!("the model service answered something that does not read: {error}")
    })?
    else {
        return Err("the model service closed before it answered".to_owned());
    };
    let answer: Answer = serde_json::from_slice(&answered).map_err(|error| {
        format!("the model service answered something that does not read: {error}")
    })?;
    if answer.ok {
        Ok(answer.output)
    } else {
        Err(answer
            .error
            .unwrap_or_else(|| "the model service refused the request".to_owned()))
    }
}

/// The manager's service socket is a Unix one: a host without it runs no model manager.
#[cfg(not(unix))]
fn exchange(_socket: &Path, _body: &[u8]) -> Result<Option<String>, String> {
    Err("this computer runs no model service".to_owned())
}
