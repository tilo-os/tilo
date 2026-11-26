//! The model manager's Hugging Face client, and signing a person in to it from a terminal.
//!
//! The manager keeps no credential of its own. The Hugging Face sign-in is a key of the
//! workspace's `tilo.models` provider, kept in the workspace's vault and set through the
//! published `models#set-key` action — from Settings' AI page, or from [`sign_in_with_code`]
//! here. Whenever a download needs it, the token comes with that request alone
//! ([`HubClient::new`]): nothing in this process reads an account, a vault item or a socket.

use std::fs::File;
use std::io::{Read, Write};
use std::os::unix::fs::OpenOptionsExt as _;
use std::path::{Path, PathBuf};
use std::time::Duration;

use account_credentials::hugging_face;
use agent_provider_config::local::hub::HUB_ORIGIN;
use local_model_service::hub::HubApi;
use local_model_service::{HttpsTransport, ServiceError};
use tilo_action_line::LineClient;
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};
use zeroize::Zeroizing;

/// The workspace's model providers, as the service that keeps them publishes their actions.
mod model_providers {
    tilo_resource_macros::bindings!(
        provider = "tilo.models",
        surface = "../../core/workspace-records/models-surface.json"
    );
}
use model_providers::{Models, ProviderView, models};

/// How long the terminal waits for the workspace to answer one action.
const ANSWER_WAIT: Duration = Duration::from_secs(60);

/// The manager's connection to this computer's daemon, as the person whose terminal runs it.
fn daemon() -> Result<LineClient, String> {
    let socket = tilo_machine::daemon_native()
        .map_err(|error| format!("this computer's daemon is not known: {error}"))?;
    LineClient::connect(&socket).map_err(|error| {
        format!(
            "this computer's daemon does not answer at {} ({error}): run `tilo status` to start it",
            socket.display()
        )
    })
}

/// One `models` action of the person's own workspace, with its arguments `argument`, and what
/// it answered.
fn invoke(
    line: &dyn ActionProvider,
    target: RemoteActionTarget,
    argument: &impl serde::Serialize,
) -> Result<serde_json::Value, String> {
    let argument = match serde_json::to_value(argument) {
        Ok(serde_json::Value::Object(argument)) => argument,
        _ => serde_json::Map::new(),
    };
    let request = InvokeRequest::plain(target, argument);
    // The daemon decides who calls by the connection; the call names nobody.
    let caller = Caller::local_person("");
    let answer = line
        .invoke(&caller, request)
        .wait_timeout(ANSWER_WAIT)
        .ok()
        .flatten()
        .ok_or_else(|| "the workspace did not answer".to_owned())?;
    let receipt = answer.map_err(|error| match error {
        ActionCallError::Refused { reason } => reason,
        other => other.own_words(),
    })?;
    match receipt.status {
        ReceiptStatus::Completed { output } => Ok(output.value.unwrap_or_default()),
        ReceiptStatus::Failed { error } => Err(error.message),
        ReceiptStatus::Rejected { error } => Err(error.own_words()),
        ReceiptStatus::Cancelled { .. } => Err("The call was stopped".to_owned()),
        ReceiptStatus::Accepted => Err("the workspace did not answer".to_owned()),
    }
}

/// Whether `endpoint` is a server on this computer, which is where the model manager's own
/// models are served.
fn on_this_computer(endpoint: &str) -> bool {
    let rest = endpoint
        .split_once("://")
        .map_or(endpoint, |(_, rest)| rest);
    let authority = rest.split('/').next().unwrap_or(rest);
    let host = match authority.rsplit_once(':') {
        Some((host, port)) if port.chars().all(|character| character.is_ascii_digit()) => host,
        _ => authority,
    };
    matches!(host, "127.0.0.1" | "localhost" | "[::1]" | "::1")
}

/// The provider a terminal sign-in sets the key of: the one `named`, or the single provider
/// this computer runs.
fn provider_named(line: &dyn ActionProvider, named: Option<&str>) -> Result<String, String> {
    if let Some(named) = named {
        return Ok(named.to_owned());
    }
    let listed: Models =
        serde_json::from_value(invoke(line, models::list(), &serde_json::Map::new())?)
            .map_err(|error| format!("the workspace's providers do not read: {error}"))?;
    let mut local: Vec<ProviderView> = listed
        .providers
        .into_iter()
        .filter(|provider| on_this_computer(&provider.endpoint))
        .collect();
    match local.len() {
        1 => Ok(local.remove(0).id),
        0 => Err(
            "This workspace keeps no model provider on this computer: add the one this \
                  computer runs in Settings' AI page, or name its id with `--provider`"
                .to_owned(),
        ),
        _ => {
            let ids: Vec<String> = local.into_iter().map(|provider| provider.id).collect();
            Err(format!(
                "This computer runs several model providers: name the one the Hugging Face \
                 sign-in is for with `--provider` ({})",
                ids.join(", ")
            ))
        }
    }
}

/// Keep `token` as `provider`'s key in the workspace's `tilo.models`, through the published
/// `models#set-key` action: the same place Settings' AI page sets a provider's key.
fn keep_key(line: &dyn ActionProvider, provider: &str, token: &str) -> Result<(), String> {
    let keeping = models::SetKey {
        provider: provider.to_owned(),
        key: token.to_owned(),
    };
    invoke(line, models::set_key(), &keeping).map(|_| ())
}

/// Sign in with Hugging Face by device code, for a device without a browser: the code and its
/// page go to standard error, and the token is kept as the key of the model provider `named`
/// (`Some`), or of the one provider this computer runs, through `models#set-key`. It is never
/// printed.
pub(crate) fn sign_in_with_code(named: Option<&str>) -> Result<Option<String>, String> {
    let line = daemon()?;
    // Asked first, so nobody enters a code that nothing will keep.
    let provider = provider_named(&line, named)?;
    let device = hugging_face::begin_device().map_err(|error| error.to_string())?;
    eprintln!(
        "To sign in with Hugging Face, open {} and enter {}",
        device.verification_uri(),
        device.user_code()
    );
    let sign_in = hugging_face::finish_device(device, hugging_face::USERINFO_ENDPOINT)
        .map_err(|error| error.to_string())?;
    keep_key(&line, &provider, &sign_in.tokens.access_token)?;
    Ok(Some(format!(
        "Signed in to Hugging Face as {}: {provider} downloads with that account",
        sign_in.username
    )))
}

/// What `hub-sign-out` does: the provider `named` (or the one this computer runs) forgets its
/// key, so downloads go without the person's account.
pub(crate) fn sign_out(named: Option<&str>) -> Result<Option<String>, String> {
    let line = daemon()?;
    let provider = provider_named(&line, named)?;
    let removing = models::RemoveKey {
        provider: provider.clone(),
    };
    invoke(&line, models::remove_key(), &removing)?;
    Ok(Some(format!("Signed out of Hugging Face for {provider}")))
}

/// The token a download carries, read from the file `path` and removed once read: whoever asks
/// for the download writes it where only this account reads, for that download alone.
pub(crate) fn token_from_file(path: &Path) -> Result<Zeroizing<String>, String> {
    let token = Zeroizing::new(
        std::fs::read_to_string(path)
            .map_err(|error| format!("{} does not read: {error}", path.display()))?,
    );
    let _ = std::fs::remove_file(path);
    Ok(Zeroizing::new(token.trim().to_owned()))
}

/// A token written where only this account reads, for a worker the caller starts apart from
/// itself: its path, which the worker consumes with [`token_from_file`].
///
/// # Errors
/// The private directory or the file cannot be written.
pub(crate) fn token_file(token: &str) -> Result<PathBuf, String> {
    let directory = tilo_paths::xdg::runtime_dir()
        .ok_or_else(|| "this login has no runtime directory".to_owned())?
        .join("tilo")
        .join("model-hub");
    tilo_files::native::private::directory(&directory)
        .map_err(|error| format!("{} cannot be made: {error}", directory.display()))?;
    let mut seed = [0_u8; 16];
    getrandom::fill(&mut seed)
        .map_err(|error| format!("no randomness for the token file: {error}"))?;
    let path = directory.join(format!("token-{}", hex::encode(seed)));
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .mode(0o600)
        .open(&path)
        .map_err(|error| format!("{} cannot be written: {error}", path.display()))?;
    file.write_all(token.as_bytes())
        .and_then(|()| file.flush())
        .map_err(|error| {
            let _ = std::fs::remove_file(&path);
            format!("{} cannot be written: {error}", path.display())
        })?;
    Ok(path)
}

/// HTTPS to the Hub, with the token the request was asked with — the workspace's Hugging Face
/// sign-in — so their gated and private models answer too. Downloads redirect to the Hub's
/// content servers: redirects are followed only to HTTPS, and the client
/// drops the token when a redirect leaves `huggingface.co`.
pub(crate) struct HubClient {
    client: reqwest::blocking::Client,
    token: Option<Zeroizing<String>>,
}

impl HubClient {
    /// A client for requests carrying `token`: the workspace's Hugging Face sign-in, handed by
    /// whoever asked for this download, or none for a model that needs no account.
    pub(crate) fn new(token: Option<Zeroizing<String>>) -> Result<Self, ServiceError> {
        let client = tilo_net::http::blocking_client()
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if attempt.previous().len() >= 5 {
                    attempt.error("too many redirects")
                } else if attempt.url().scheme() != "https" {
                    attempt.error("redirect away from HTTPS")
                } else {
                    attempt.follow()
                }
            }))
            .tls_backend_rustls()
            .connect_timeout(Duration::from_secs(15))
            // Each blocking read waits at most this long; a large download
            // takes as long as it takes and resumes where it stopped.
            .timeout(Duration::from_secs(60))
            .build()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        Ok(Self { client, token })
    }

    /// Whether requests carry the person's token.
    pub(crate) const fn signed_in(&self) -> bool {
        self.token.is_some()
    }

    fn request(&self, url: &str) -> Result<reqwest::blocking::RequestBuilder, ServiceError> {
        if !url.starts_with(&format!("{HUB_ORIGIN}/")) {
            return Err(ServiceError::NonHttpsArtifact);
        }
        let request = self.client.get(url);
        Ok(match &self.token {
            Some(token) => request.bearer_auth(token.as_str()),
            None => request,
        })
    }
}

/// What the Hub's refusal means for the person.
fn refusal(status: reqwest::StatusCode, signed_in: bool) -> ServiceError {
    ServiceError::Backend(match status.as_u16() {
        401 if signed_in => format!(
            "Hugging Face no longer accepts the stored sign-in or token ({status}): sign in again in Settings"
        ),
        403 if signed_in => format!(
            "Hugging Face refused access ({status}): accept the model's terms on its page with the account whose token is stored here"
        ),
        401 | 403 => format!(
            "Hugging Face refused access ({status}): this model needs its terms accepted and a Hugging Face token stored here"
        ),
        404 => format!("Hugging Face has no such model or file ({status})"),
        429 => format!("Hugging Face is limiting requests ({status}); try again later"),
        _ => format!("Hugging Face answered {status}"),
    })
}

impl HubApi for HubClient {
    fn get(&mut self, url: &str, limit: usize) -> Result<Vec<u8>, ServiceError> {
        let response = self
            .request(url)?
            .send()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !response.status().is_success() {
            return Err(refusal(response.status(), self.token.is_some()));
        }
        let mut body = Vec::new();
        response
            .take(u64::try_from(limit).unwrap_or(u64::MAX).saturating_add(1))
            .read_to_end(&mut body)?;
        if body.len() > limit {
            return Err(ServiceError::Backend(
                "Hugging Face's answer is too large".into(),
            ));
        }
        Ok(body)
    }
}

impl HttpsTransport for HubClient {
    fn resume(
        &mut self,
        url: &str,
        start: u64,
        maximum_bytes: u64,
        progress: &mut dyn FnMut(u64),
        destination: &mut File,
    ) -> Result<u64, ServiceError> {
        let response = self
            .request(url)?
            .header(reqwest::header::RANGE, format!("bytes={start}-"))
            .send()
            .map_err(|error| ServiceError::Backend(error.to_string()))?;
        if !response.status().is_success() {
            return Err(refusal(response.status(), self.token.is_some()));
        }
        copy_range(response, start, maximum_bytes, progress, destination)
    }
}

/// Append a ranged response's body to `destination`, refusing a server that
/// ignored the range or sends more than `maximum_bytes`.
pub(crate) fn copy_range(
    mut response: reqwest::blocking::Response,
    start: u64,
    maximum_bytes: u64,
    progress: &mut dyn FnMut(u64),
    destination: &mut File,
) -> Result<u64, ServiceError> {
    if start > 0 {
        if response.status() != reqwest::StatusCode::PARTIAL_CONTENT {
            return Err(ServiceError::Backend(
                "resumed HTTPS request did not return 206 Partial Content".into(),
            ));
        }
        let range = response
            .headers()
            .get(reqwest::header::CONTENT_RANGE)
            .and_then(|value| value.to_str().ok())
            .unwrap_or("");
        if !range.starts_with(&format!("bytes {start}-")) {
            return Err(ServiceError::Backend(
                "resumed HTTPS response had an invalid Content-Range".into(),
            ));
        }
    }
    if response
        .content_length()
        .is_some_and(|length| length > maximum_bytes)
    {
        return Err(ServiceError::PartialTooLarge);
    }
    let mut count = 0_u64;
    let mut buffer = vec![0_u8; 32 * 1024];
    loop {
        let read = response.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        // `Read` never reports more bytes than the buffer holds.
        let chunk = buffer.get(..read).ok_or(ServiceError::PartialTooLarge)?;
        let read = u64::try_from(read).map_err(|_| ServiceError::PartialTooLarge)?;
        count = count
            .checked_add(read)
            .ok_or(ServiceError::PartialTooLarge)?;
        if count > maximum_bytes {
            return Err(ServiceError::PartialTooLarge);
        }
        destination.write_all(chunk)?;
        // State is published by the service after every confirmed write,
        // not optimistically from Content-Length.
        progress(count);
    }
    Ok(count)
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;
    use serde_json::json;
    use tilo_action_services::TestProvider;
    use tilo_actions::ActionOutput;

    /// The provider the terminal sign-in sets a key on: the one this computer runs, or the one
    /// a person names; several local providers are refused asking for one.
    #[test]
    fn the_terminal_finds_the_provider_this_computer_runs() {
        let listed = json!({"providers": [
            {"id": "openai", "name": "OpenAI", "protocol": "open-ai-compatible",
             "endpoint": "https://api.openai.com/v1", "configured": true, "enabled": true,
             "models": []},
            {"id": "vllm", "name": "vLLM", "protocol": "open-ai-compatible",
             "endpoint": "http://127.0.0.1:8000/v1", "configured": false, "enabled": true,
             "models": []},
        ]});
        let provider = TestProvider::new().completing(move |_| ActionOutput {
            value: Some(listed.clone()),
            resources: Vec::new(),
        });
        assert_eq!(provider_named(&provider, None).unwrap(), "vllm");
        assert_eq!(provider_named(&provider, Some("named")).unwrap(), "named");
    }

    /// A download carries the token its request was asked with, and none when it asked for
    /// none; a URL away from the Hub is refused before any token could go to it.
    #[test]
    fn a_download_carries_the_token_its_request_was_asked_with() {
        let url = format!("{HUB_ORIGIN}/acme/Chat-GGUF/resolve/abc/model.gguf");
        let hub = HubClient::new(Some(Zeroizing::new("hf_asked".to_owned()))).unwrap();
        assert!(hub.signed_in());
        let request = hub.request(&url).unwrap().build().unwrap();
        assert_eq!(
            request.headers()[reqwest::header::AUTHORIZATION],
            "Bearer hf_asked"
        );
        let anonymous = HubClient::new(None).unwrap();
        assert!(!anonymous.signed_in());
        assert!(
            anonymous
                .request(&url)
                .unwrap()
                .build()
                .unwrap()
                .headers()
                .get(reqwest::header::AUTHORIZATION)
                .is_none(),
            "a request with no token carries none"
        );
        assert!(matches!(
            hub.request("https://example.test/acme/model.gguf"),
            Err(ServiceError::NonHttpsArtifact)
        ));
    }

    /// Signing in from the terminal keeps the token as the provider's key through the published
    /// `models#set-key`, and nowhere else: the call carries the provider and the token, and no
    /// answer carries it back.
    #[test]
    fn the_terminal_sign_in_keeps_the_token_as_the_providers_key() {
        let asked: Arc<Mutex<Vec<InvokeRequest>>> = Arc::default();
        let seen = Arc::clone(&asked);
        let provider = TestProvider::new().answering(move |_, request| {
            seen.lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(request.clone());
            tilo_actions::remote::Pending::ready(Ok(tilo_actions::remote::InvocationReceipt {
                invocation: request.invocation,
                status: ReceiptStatus::Completed {
                    output: ActionOutput::default(),
                },
            }))
        });
        keep_key(&provider, "vllm", "hf_kept").unwrap();
        let request = {
            let asked = asked
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            asked.last().cloned().expect("the key is kept")
        };
        assert_eq!(request.target, models::set_key());
        let tilo_actions::remote::InvokeArgument::Json(argument) = &request.argument else {
            panic!("the key is kept with its provider");
        };
        assert_eq!(argument["provider"], "vllm");
        assert_eq!(argument["key"], "hf_kept");
    }
}
