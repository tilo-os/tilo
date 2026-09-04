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
use std::time::{Duration, Instant};

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
            timeout_ms,
        } if app_id == AGENT_APP => plan(
            &profile_id,
            &model_id,
            body,
            Duration::from_millis(timeout_ms),
        )
        .map(|output| Response {
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

const MAX_PROVIDER_TIMEOUT: Duration = Duration::from_secs(60);
const RETRY_ATTEMPTS: u32 = 4;
const FIRST_RETRY_WAIT: Duration = Duration::from_millis(500);
const LONGEST_RETRY_WAIT: Duration = Duration::from_secs(4);
const ROOM_TO_ANSWER: Duration = Duration::from_secs(5);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Again {
    Never,
    Soon(Option<Duration>),
}

struct ProviderFailure {
    why: String,
    again: Again,
}

fn plan(
    profile_id: &str,
    model_id: &str,
    body: serde_json::Value,
    requested_timeout: Duration,
) -> Result<String, String> {
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
    let timeout = requested_timeout.min(MAX_PROVIDER_TIMEOUT);
    let began = Instant::now();
    let client = reqwest::blocking::Client::builder()
        .connect_timeout(std::time::Duration::from_secs(8))
        .build()
        .map_err(|_| "gateway HTTP client is unavailable".to_string())?;
    let result = while_busy(
        || timeout.checked_sub(began.elapsed()),
        std::thread::sleep,
        |budget| provider_attempt(&client, &profile, &endpoint, secret.as_str(), &body, budget),
    );
    secret.zeroize();
    result
}

fn provider_attempt(
    client: &reqwest::blocking::Client,
    profile: &ProviderProfile,
    endpoint: &str,
    secret: &str,
    body: &serde_json::Value,
    budget: Duration,
) -> Result<String, ProviderFailure> {
    let mut request = client
        .post(endpoint)
        .timeout(budget)
        .header("content-type", "application/json");
    match profile.protocol {
        ProviderProtocol::Anthropic => {
            request = request
                .header("anthropic-version", "2023-06-01")
                .header("x-api-key", secret);
        }
        ProviderProtocol::OpenAiCompatible => request = request.bearer_auth(secret),
    }
    let response = request.json(body).send().map_err(|error| ProviderFailure {
        why: if error.is_timeout() {
            "provider request timed out".into()
        } else {
            "provider transport failed".into()
        },
        again: if error.is_timeout() || error.is_connect() {
            Again::Soon(None)
        } else {
            Again::Never
        },
    })?;
    let status = response.status();
    if !status.is_success() {
        let retry_after = response
            .headers()
            .get(reqwest::header::RETRY_AFTER)
            .and_then(|value| value.to_str().ok());
        return Err(ProviderFailure {
            why: format!("provider returned HTTP {}", status.as_u16()),
            again: again_after(status.as_u16(), retry_after),
        });
    }
    response.text().map_err(|_| ProviderFailure {
        why: "provider response was unreadable".into(),
        again: Again::Never,
    })
}

fn again_after(status: u16, retry_after: Option<&str>) -> Again {
    let soon = || Again::Soon(retry_after.and_then(after_seconds));
    match status {
        408 | 429 | 500 | 502 | 503 | 504 | 529 => soon(),
        _ => Again::Never,
    }
}

fn after_seconds(value: &str) -> Option<Duration> {
    Duration::try_from_secs_f64(value.trim().parse::<f64>().ok()?).ok()
}

fn retry_wait(retry: u32, asked_for: Option<Duration>) -> Duration {
    if let Some(wait) = asked_for {
        return wait;
    }
    let doubling = 1u32
        .checked_shl(retry.saturating_sub(1))
        .unwrap_or(u32::MAX);
    FIRST_RETRY_WAIT
        .checked_mul(doubling)
        .unwrap_or(LONGEST_RETRY_WAIT)
        .min(LONGEST_RETRY_WAIT)
}

fn while_busy<L, W, A>(left: L, mut wait: W, mut attempt: A) -> Result<String, String>
where
    L: Fn() -> Option<Duration>,
    W: FnMut(Duration),
    A: FnMut(Duration) -> Result<String, ProviderFailure>,
{
    let began = left();
    let elapsed = |now: Option<Duration>| match (began, now) {
        (Some(start), Some(remaining)) => start.saturating_sub(remaining),
        (Some(start), None) => start,
        _ => Duration::ZERO,
    };
    let mut made = 0;
    loop {
        let Some(budget) = left().filter(|budget| !budget.is_zero()) else {
            return Err("provider request timed out".into());
        };
        made += 1;
        let ProviderFailure { why, again } = match attempt(budget) {
            Ok(output) => return Ok(output),
            Err(failure) => failure,
        };
        let Again::Soon(asked_for) = again else {
            return Err(why);
        };
        let give_up = || {
            if made == 1 {
                why.clone()
            } else {
                format!(
                    "{why} ({made} attempts over {:.1}s)",
                    elapsed(left()).as_secs_f32()
                )
            }
        };
        if made >= RETRY_ATTEMPTS {
            return Err(give_up());
        }
        let waiting = retry_wait(made, asked_for);
        if left().is_none_or(|remaining| remaining < waiting + ROOM_TO_ANSWER) {
            return Err(give_up());
        }
        wait(waiting);
    }
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

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use super::*;

    struct Fake {
        left: Duration,
        waits: Vec<Duration>,
        calls: usize,
    }

    fn run_retries(
        left: Duration,
        outcomes: &[Result<&str, (u16, Option<&str>)>],
    ) -> (Result<String, String>, Fake) {
        let fake = RefCell::new(Fake {
            left,
            waits: Vec::new(),
            calls: 0,
        });
        let result = while_busy(
            || {
                let fake = fake.borrow();
                (!fake.left.is_zero()).then_some(fake.left)
            },
            |duration| {
                let mut fake = fake.borrow_mut();
                fake.waits.push(duration);
                fake.left = fake.left.saturating_sub(duration);
            },
            |_budget| {
                let mut fake = fake.borrow_mut();
                let index = fake.calls.min(outcomes.len() - 1);
                fake.calls += 1;
                fake.left = fake.left.saturating_sub(Duration::from_millis(200));
                match outcomes[index] {
                    Ok(output) => Ok(output.to_string()),
                    Err((status, retry_after)) => Err(ProviderFailure {
                        why: format!("provider returned HTTP {status}"),
                        again: again_after(status, retry_after),
                    }),
                }
            },
        );
        (result, fake.into_inner())
    }

    #[test]
    fn transient_provider_failures_are_retried_at_the_gateway() {
        let (result, fake) = run_retries(
            Duration::from_secs(20),
            &[Err((529, None)), Err((503, None)), Ok("answer")],
        );
        assert_eq!(result.as_deref(), Ok("answer"));
        assert_eq!(fake.calls, 3);
        assert_eq!(
            fake.waits,
            [Duration::from_millis(500), Duration::from_secs(1)]
        );
    }

    #[test]
    fn settled_provider_failures_are_not_retried() {
        for status in [400, 401, 403, 404, 422] {
            let (result, fake) = run_retries(Duration::from_secs(20), &[Err((status, None))]);
            assert!(result.unwrap_err().contains(&status.to_string()));
            assert_eq!(fake.calls, 1, "HTTP {status}");
            assert!(fake.waits.is_empty(), "HTTP {status}");
        }
    }

    #[test]
    fn retry_after_and_transient_statuses_are_classified_on_the_http_side() {
        for status in [408, 429, 500, 502, 503, 504, 529] {
            assert_eq!(again_after(status, None), Again::Soon(None));
        }
        assert_eq!(
            again_after(429, Some("0.25")),
            Again::Soon(Some(Duration::from_millis(250)))
        );
        assert_eq!(again_after(401, Some("1")), Again::Never);
    }

    #[test]
    fn retries_never_consume_room_needed_for_an_answer() {
        let (result, fake) = run_retries(Duration::from_secs(3), &[Err((529, Some("1")))]);
        assert!(result.is_err());
        assert_eq!(fake.calls, 1);
        assert!(fake.waits.is_empty());
    }
}
