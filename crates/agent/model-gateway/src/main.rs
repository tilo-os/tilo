//! Per-user hosted model gateway.
//!
//! This intentionally is a narrow OpenAI-compatible/Anthropic relay, not a
//! general LiteLLM deployment. qOS needs one profile-selected planning call;
//! adding LiteLLM would introduce an unpinned Python control plane and more
//! provider surface than is audited here.

use std::fs;
use std::io::Read;
use std::os::fd::AsRawFd;
use std::os::unix::fs::{FileTypeExt, PermissionsExt};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;

use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::{ProviderConfig, ProviderProfile, ProviderProtocol};
use clap::{Parser, Subcommand};
use qos_model_gateway::{Frame, RPC_VERSION, Request, Response, read_frame, write_frame};
use zeroize::{Zeroize, Zeroizing};

#[derive(Parser)]
#[command(name = "qos-model-gateway", about = "qOS hosted model gateway")]
struct Cli {
    #[command(subcommand)]
    action: Action,
}

#[derive(Subcommand)]
enum Action {
    /// systemd-owned service endpoint; not a user-facing command.
    #[command(hide = true)]
    Serve {
        #[arg(long)]
        socket: PathBuf,
    },
    SecretStatus {
        #[arg(long)]
        profile: String,
    },
    RemoveApiKey {
        #[arg(long)]
        profile: String,
    },
}

const MAX_HANDOFF: usize = 4096;
const SETTINGS_APP: &str = "dev.qos.settings";
const AGENT_APP: &str = "dev.qos.agent";

#[derive(serde::Deserialize)]
struct Handoff {
    protocol: u16,
    app_id: String,
}

fn main() {
    let cli = Cli::parse();
    let result = match cli.action {
        Action::Serve { socket } => serve(&socket),
        Action::SecretStatus { profile } => control(Request::SecretStatus {
            profile_id: profile,
        }),
        Action::RemoveApiKey { profile } => control(Request::RemoveApiKey {
            profile_id: profile,
        }),
    };
    if let Err(error) = result {
        // Never interpolate a request or provider response here; either could
        // contain a bearer value or user document content.
        eprintln!("qos-model-gateway: {error}");
        std::process::exit(1);
    }
}

fn control(request: Request) -> Result<(), String> {
    let response = qos_model_gateway::request(request).map_err(|error| error.to_string())?;
    if let Some(configured) = response.configured {
        println!("{configured}");
    }
    Ok(())
}

fn serve(socket: &PathBuf) -> Result<(), String> {
    if let Some(parent) = socket.parent() {
        fs::create_dir_all(parent).map_err(|error| error.to_string())?;
    }
    if socket.exists() {
        let metadata = fs::symlink_metadata(socket).map_err(|error| error.to_string())?;
        if !metadata.file_type().is_socket() {
            return Err("refusing to replace a non-socket gateway path".into());
        }
        fs::remove_file(socket).map_err(|error| error.to_string())?;
    }
    let listener = UnixListener::bind(socket).map_err(|error| error.to_string())?;
    fs::set_permissions(socket, fs::Permissions::from_mode(0o600))
        .map_err(|error| error.to_string())?;
    for stream in listener.incoming() {
        match stream {
            Ok(stream) => serve_client(stream),
            Err(error) => return Err(error.to_string()),
        }
    }
    Ok(())
}

fn serve_client(mut stream: UnixStream) {
    if !same_user(&stream) {
        return;
    }
    let Ok(handoff) = read_handoff(&mut stream) else {
        return;
    };
    loop {
        let response = match read_frame::<Frame<Request>>(&mut stream) {
            Ok(frame) if frame.version == RPC_VERSION => handle(&handoff.app_id, frame.value),
            Ok(_) => rejected("unsupported gateway RPC version"),
            Err(_) => return,
        };
        if write_frame(&mut stream, &response).is_err() {
            return;
        }
    }
}

fn read_handoff(stream: &mut UnixStream) -> Result<Handoff, String> {
    let mut raw_length = [0_u8; 4];
    stream
        .read_exact(&mut raw_length)
        .map_err(|_| "missing launcher authentication".to_string())?;
    let length = usize::try_from(u32::from_le_bytes(raw_length))
        .map_err(|_| "invalid launcher authentication".to_string())?;
    if length > MAX_HANDOFF {
        return Err("oversized launcher authentication".into());
    }
    let mut body = vec![0_u8; length];
    stream
        .read_exact(&mut body)
        .map_err(|_| "truncated launcher authentication".to_string())?;
    let handoff: Handoff =
        serde_json::from_slice(&body).map_err(|_| "invalid launcher authentication".to_string())?;
    if handoff.protocol != 1 || (handoff.app_id != SETTINGS_APP && handoff.app_id != AGENT_APP) {
        return Err("launcher is not authorized for the model gateway".into());
    }
    Ok(handoff)
}

fn handle(app_id: &str, request: Request) -> Response {
    let result = match request {
        Request::Plan {
            profile_id,
            model_id,
            body,
        } if app_id == AGENT_APP => plan(&profile_id, &model_id, body).map(|output| Response {
            ok: true,
            output: Some(output),
            configured: None,
            error: None,
        }),
        Request::Plan { .. } => Err("only qos-agent may plan through the model gateway".into()),
        Request::SecretStatus { profile_id } if app_id == SETTINGS_APP => {
            secret_status(&profile_id).map(|configured| Response {
                ok: true,
                output: None,
                configured: Some(configured),
                error: None,
            })
        }
        Request::SetApiKey {
            profile_id,
            api_key,
        } if app_id == SETTINGS_APP => set_key(&profile_id, api_key).map(|()| Response {
            ok: true,
            output: None,
            configured: None,
            error: None,
        }),
        Request::RemoveApiKey { profile_id } if app_id == SETTINGS_APP => remove_key(&profile_id)
            .map(|()| Response {
                ok: true,
                output: None,
                configured: None,
                error: None,
            }),
        _ => Err("caller is not authorized for this gateway operation".into()),
    };
    result.unwrap_or_else(|error| rejected(&error))
}

fn rejected(error: &str) -> Response {
    Response {
        ok: false,
        output: None,
        configured: None,
        error: Some(error.into()),
    }
}

fn vault_id(profile_id: &str) -> Result<AccountId, String> {
    // A fixed namespace means unrelated account secrets cannot be addressed.
    if profile_id.is_empty()
        || profile_id.len() > 128
        || !profile_id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-')
    {
        return Err("invalid provider profile id".into());
    }
    Ok(AccountId::new(format!("agent-provider-{profile_id}")))
}

fn configured_profile(profile_id: &str) -> Result<ProviderProfile, String> {
    let config = ProviderConfig::load();
    if !config.validation_errors().is_empty() {
        return Err("provider configuration is invalid".into());
    }
    config
        .profiles
        .into_iter()
        .find(|profile| profile.id == profile_id && profile.enabled)
        .ok_or_else(|| "provider is not enabled".into())
}

fn secret_status(profile_id: &str) -> Result<bool, String> {
    let _ = configured_profile(profile_id)?;
    Ok(SecretVault::open()
        .get(&vault_id(profile_id)?)
        .is_some_and(|secret| !secret.password.is_empty()))
}

fn set_key(profile_id: &str, api_key: String) -> Result<(), String> {
    let _ = configured_profile(profile_id)?;
    let mut key = Zeroizing::new(api_key);
    if key.trim().is_empty() || key.len() > 16 * 1024 {
        return Err("API key is empty or too large".into());
    }
    let mut vault = SecretVault::open();
    vault.put(&vault_id(profile_id)?, &Secret::password(key.as_str()));
    key.zeroize();
    vault.save();
    Ok(())
}

fn remove_key(profile_id: &str) -> Result<(), String> {
    let _ = configured_profile(profile_id)?;
    let mut vault = SecretVault::open();
    vault.remove(&vault_id(profile_id)?);
    vault.save();
    Ok(())
}

fn plan(profile_id: &str, model_id: &str, body: serde_json::Value) -> Result<String, String> {
    let profile = configured_profile(profile_id)?;
    if !profile.models.iter().any(|model| model.id == model_id) {
        return Err("model is not configured for this provider".into());
    }
    let mut secret = SecretVault::open()
        .get(&vault_id(profile_id)?)
        .map(|secret| Zeroizing::new(secret.password))
        .filter(|secret| !secret.is_empty())
        .ok_or_else(|| "API key is not configured".to_string())?;
    let endpoint = provider_url(&profile);
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .timeout(std::time::Duration::from_secs(60))
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())?;
    let mut request = client
        .post(endpoint)
        .header("content-type", "application/json");
    match profile.protocol {
        ProviderProtocol::Anthropic => {
            request = request
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", secret.as_str());
        }
        ProviderProtocol::OpenAiCompatible => {
            request = request.bearer_auth(secret.as_str());
        }
    }
    let result = request
        .json(&body)
        .send()
        .and_then(|response| response.error_for_status())
        .and_then(|response| response.text())
        .map_err(|_| "provider request failed".to_string());
    secret.zeroize();
    result
}

fn provider_url(profile: &ProviderProfile) -> String {
    let base = profile.endpoint.trim_end_matches('/');
    match profile.protocol {
        ProviderProtocol::Anthropic if base.ends_with("/v1/messages") => base.into(),
        ProviderProtocol::Anthropic => format!("{base}/v1/messages"),
        ProviderProtocol::OpenAiCompatible if base.ends_with("/chat/completions") => base.into(),
        ProviderProtocol::OpenAiCompatible => format!("{base}/chat/completions"),
    }
}

fn same_user(stream: &UnixStream) -> bool {
    let mut credentials = std::mem::MaybeUninit::<libc::ucred>::uninit();
    let mut length = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    // SAFETY: getsockopt writes into a correctly sized ucred buffer.
    let result = unsafe {
        libc::getsockopt(
            stream.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            credentials.as_mut_ptr().cast(),
            &mut length,
        )
    };
    result == 0
        && usize::try_from(length).ok() == Some(std::mem::size_of::<libc::ucred>())
        && unsafe { credentials.assume_init().uid } == unsafe { libc::geteuid() }
}
