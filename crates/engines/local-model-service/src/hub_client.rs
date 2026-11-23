//! The model manager's Hugging Face client and the workspace's Hugging Face sign-in.
//!
//! The sign-in is the workspace's: a Hugging Face account in its accounts names the entry of its
//! vault that keeps a pasted token or the grant a sign-in left, and Settings keeps both. The
//! manager asks the session for it on the socket the session serves the model manager
//! ([`accounts::sign_ins::models_socket`]): it is told the Hugging Face accounts, and is released
//! the token, or the grant's access token renewed where it was due, never the refresh token.
//!
//! On a device without a browser the person signs in here, in a terminal, by device code
//! ([`sign_in_with_code`]); the session keeps that grant as Settings keeps one. Signing out
//! ([`sign_out`]) has the session forget the account with its sign-in.

use std::fs::File;
use std::io::{Read, Write};
use std::path::Path;
use std::time::Duration;

use account_credentials::hugging_face::{self, HubSignIn};
use accounts::sign_ins::{self, Answer, Ask, Client, Held};
use agent_provider_config::local::hub::{HUB_ORIGIN, HubAccount, HubTokenState};
use local_model_service::hub::HubApi;
use local_model_service::{HttpsTransport, ServiceError};
use zeroize::Zeroizing;

/// The token requests carry, and what Settings is told of it.
pub(crate) struct HubCredentialRead {
    pub(crate) token: Option<Zeroizing<String>>,
    pub(crate) state: HubTokenState,
}

impl HubCredentialRead {
    const NONE: Self = Self {
        token: None,
        state: HubTokenState::None,
    };
}

/// The workspace's Hugging Face credential, as the session releases it to this login.
///
/// # Errors
/// The session does not answer, or refused the sign-in in its own words.
pub(crate) fn stored_credential() -> Result<HubCredentialRead, String> {
    sign_ins::models_socket().map_or(Ok(HubCredentialRead::NONE), |socket| {
        stored_credential_at(&socket)
    })
}

/// [`stored_credential`], asked on the session's socket at `socket`.
fn stored_credential_at(socket: &Path) -> Result<HubCredentialRead, String> {
    let mut session = match Client::connect(socket) {
        Ok(session) => session,
        // No session runs for this login, so no workspace's account is known here.
        Err(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused
            ) =>
        {
            return Ok(HubCredentialRead::NONE);
        }
        Err(error) => {
            return Err(format!(
                "the session cannot be asked for the Hugging Face sign-in: {error}"
            ));
        }
    };
    let asked = |answer: std::io::Result<Answer>| {
        answer.map_err(|error| format!("the session did not answer: {error}"))
    };
    let held = match asked(session.ask(&Ask::Accounts { after: None }))? {
        Answer::Accounts { accounts, .. } => accounts.into_iter().find(|held| held.account.enabled),
        Answer::Refused { error } => return Err(error),
        Answer::SignIn { .. } | Answer::Reported => return Err(sign_ins::NOT_ANSWERED.into()),
    };
    let Some(Held {
        workspace, account, ..
    }) = held
    else {
        return Ok(HubCredentialRead::NONE);
    };
    let Some(item) = accounts::email::password_item(&account.config) else {
        return Ok(HubCredentialRead::NONE);
    };
    let signed_in = signed_in_as(&account.config);
    let ask = Ask::SignIn {
        workspace,
        item: item.to_owned(),
    };
    match (asked(session.ask(&ask))?, signed_in) {
        (Answer::SignIn { secret }, None) => Ok(HubCredentialRead {
            token: Some(secret),
            state: HubTokenState::Pasted,
        }),
        (Answer::SignIn { secret }, Some(account)) => Ok(HubCredentialRead {
            token: Some(secret),
            state: HubTokenState::SignedIn { account },
        }),
        // A sign-in Hugging Face will not renew any more has ended, and requests go without it.
        (Answer::Refused { error }, Some(account)) if error == sign_ins::SIGN_IN_AGAIN => {
            Ok(HubCredentialRead {
                token: None,
                state: HubTokenState::Ended { account },
            })
        }
        (Answer::Refused { error }, _) => Err(error),
        (Answer::Accounts { .. } | Answer::Reported, _) => Err(sign_ins::NOT_ANSWERED.into()),
    }
}

/// Who signed in, from the Hugging Face account's settings: `None` for a pasted token.
fn signed_in_as(config: &serde_json::Value) -> Option<HubAccount> {
    let text = |key: &str| {
        config
            .get(key)
            .and_then(serde_json::Value::as_str)
            .filter(|text| !text.is_empty())
            .map(str::to_owned)
    };
    Some(HubAccount {
        username: text("username")?,
        name: text("name"),
    })
}

/// What `hub-account` answers: the workspace's Hugging Face credential's state, as JSON.
pub(crate) fn account_state() -> Result<String, String> {
    serde_json::to_string(&stored_credential()?.state).map_err(|error| error.to_string())
}

/// What a sign-in or sign-out without a session to keep it is told.
const NO_SESSION: &str = "Sign in to Tilo first: the workspace keeps the Hugging Face sign-in";

/// The session's model manager socket, answering now.
fn session_at(socket: Option<std::path::PathBuf>) -> Result<Client, String> {
    let socket = socket.ok_or(NO_SESSION)?;
    Client::connect(&socket).map_err(|error| format!("{NO_SESSION} ({error})"))
}

/// The answer to `ask`, when the session did what it asks.
fn done(session: &mut Client, ask: &Ask) -> Result<(), String> {
    match session.ask(ask) {
        Ok(Answer::Reported) => Ok(()),
        Ok(Answer::Refused { error }) => Err(error),
        Ok(Answer::Accounts { .. } | Answer::SignIn { .. }) => Err(sign_ins::NOT_ANSWERED.into()),
        Err(error) => Err(format!("the session did not answer: {error}")),
    }
}

/// The workspace's Hugging Face accounts, as the session tells them.
fn told(session: &mut Client) -> Result<Vec<Held>, String> {
    match session.ask(&Ask::Accounts { after: None }) {
        Ok(Answer::Accounts { accounts, .. }) => Ok(accounts),
        Ok(Answer::Refused { error }) => Err(error),
        Ok(Answer::SignIn { .. } | Answer::Reported) => Err(sign_ins::NOT_ANSWERED.into()),
        Err(error) => Err(format!("the session did not answer: {error}")),
    }
}

/// Sign in with Hugging Face by device code, for a device without a browser: the code and its
/// page go to standard error, and the session keeps the grant as the workspace's Hugging Face
/// account's sign-in.
pub(crate) fn sign_in_with_code() -> Result<Option<String>, String> {
    // Asked first, so nobody enters a code that nothing will keep.
    drop(session_at(sign_ins::models_socket())?);
    let device = hugging_face::begin_device().map_err(|error| error.to_string())?;
    eprintln!(
        "To sign in with Hugging Face, open {} and enter {}",
        device.verification_uri(),
        device.user_code()
    );
    let sign_in = hugging_face::finish_device(device, hugging_face::USERINFO_ENDPOINT)
        .map_err(|error| error.to_string())?;
    keep(&mut session_at(sign_ins::models_socket())?, &sign_in)?;
    Ok(Some(format!(
        "Signed in to Hugging Face as {}",
        sign_in.username
    )))
}

/// Have the session keep `sign_in`'s grant: in the workspace whose Hugging Face account it
/// replaces, or, with none, the session's choice.
fn keep(session: &mut Client, sign_in: &HubSignIn) -> Result<(), String> {
    let workspace = told(session)?.into_iter().next().map(|held| held.workspace);
    done(
        session,
        &Ask::Keep {
            workspace,
            secret: sign_ins::Withheld(Zeroizing::new(account_credentials::oauth_grant(
                &sign_in.tokens,
            ))),
            username: sign_in.username.clone(),
            name: sign_in.name.clone(),
        },
    )
}

/// What `hub-sign-out` does: the session forgets the workspace's Hugging Face accounts and
/// their sign-ins, so downloads go without them.
pub(crate) fn sign_out() -> Result<Option<String>, String> {
    forget_all(&mut session_at(sign_ins::models_socket())?).map(|()| None)
}

fn forget_all(session: &mut Client) -> Result<(), String> {
    for held in told(session)? {
        done(
            session,
            &Ask::Forget {
                workspace: held.workspace,
                account: held.account.id.as_str().to_owned(),
            },
        )?;
    }
    Ok(())
}

/// HTTPS to the Hub, with the person's token when one is stored, so their
/// gated and private models answer too. Downloads redirect to the Hub's
/// content servers: redirects are followed only to HTTPS, and the client
/// drops the token when a redirect leaves `huggingface.co`.
pub(crate) struct HubClient {
    client: reqwest::blocking::Client,
    token: Option<Zeroizing<String>>,
}

impl HubClient {
    /// A client with the stored token.
    pub(crate) fn new() -> Result<Self, ServiceError> {
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
        Ok(Self {
            client,
            token: stored_credential().map_err(ServiceError::Backend)?.token,
        })
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
    use accounts::sign_ins::fixture::{Fixture, item_for};

    use super::*;

    fn hub_account(config: serde_json::Value) -> accounts::Account {
        let mut account = accounts::Account::new(
            accounts::AccountId::from("hugging-face"),
            sign_ins::MODEL_HUB_TYPE,
        );
        account.config = config;
        accounts::email::set_password_item(&mut account.config, Some(&item_for("hugging-face")));
        account
    }

    tilo_test_env::scenario! {
        /// The credential the manager downloads with is the workspace's Hugging Face account's,
        /// released by the session: none without one, a pasted token, a sign-in's access token
        /// with who signed in, and a refusal in the session's words.
        fn the_workspaces_hugging_face_sign_in_is_what_requests_carry;

        phase no_session_and_no_account_mean_no_token;
        {
            let dir = tempfile::tempdir().unwrap();
            let read = stored_credential_at(&dir.path().join("absent")).unwrap();
            assert!(read.token.is_none());
            assert_eq!(read.state, HubTokenState::None);
            let session = Fixture::serve_models(dir.path());
            let read = stored_credential_at(&session.socket()).unwrap();
            assert_eq!(read.state, HubTokenState::None);
        }

        phase a_pasted_token_is_carried;
        {
            let dir = tempfile::tempdir().unwrap();
            let session = Fixture::serve_models(dir.path());
            session.keep_account(hub_account(serde_json::json!({"server_url": HUB_ORIGIN})));
            session.keep("hugging-face", "hf_pasted");
            let read = stored_credential_at(&session.socket()).unwrap();
            assert_eq!(read.token.as_deref().map(String::as_str), Some("hf_pasted"));
            assert_eq!(read.state, HubTokenState::Pasted);
            assert_eq!(session.released(), [item_for("hugging-face")]);
        }

        phase a_sign_in_is_carried_as_who_signed_in;
        {
            let dir = tempfile::tempdir().unwrap();
            let session = Fixture::serve_models(dir.path());
            session.keep_account(hub_account(serde_json::json!({
                "server_url": HUB_ORIGIN, "username": "ada", "name": "Ada Lovelace",
            })));
            session.keep("hugging-face", "hf_oauth_access");
            let read = stored_credential_at(&session.socket()).unwrap();
            assert_eq!(read.token.as_deref().map(String::as_str), Some("hf_oauth_access"));
            assert_eq!(
                read.state,
                HubTokenState::SignedIn {
                    account: HubAccount {
                        username: "ada".into(),
                        name: Some("Ada Lovelace".into()),
                    }
                }
            );
        }

        phase a_sign_in_in_a_terminal_is_kept_and_signing_out_forgets_it;
        {
            let dir = tempfile::tempdir().unwrap();
            let session = Fixture::serve_models(dir.path());
            let sign_in = HubSignIn {
                tokens: account_credentials::TokenSet {
                    access_token: "hf_oauth_access".into(),
                    refresh_token: "hf_refresh".into(),
                    ..account_credentials::TokenSet::default()
                },
                username: "ada".into(),
                name: None,
            };
            keep(&mut Client::connect(&session.socket()).unwrap(), &sign_in).unwrap();
            // The session keeps the grant itself, which it renews, and the account names it.
            let read = stored_credential_at(&session.socket()).unwrap();
            assert_eq!(
                read.state,
                HubTokenState::SignedIn {
                    account: HubAccount {
                        username: "ada".into(),
                        name: None,
                    }
                }
            );
            let grant = read.token.unwrap();
            let kept: serde_json::Value = serde_json::from_str(&grant).unwrap();
            assert_eq!(kept["kind"], "oauth2");
            assert_eq!(kept["tokens"]["refresh_token"], "hf_refresh");

            forget_all(&mut Client::connect(&session.socket()).unwrap()).unwrap();
            assert_eq!(
                stored_credential_at(&session.socket()).unwrap().state,
                HubTokenState::None
            );
            let mut asked = Client::connect(&session.socket()).unwrap();
            let entry = Ask::SignIn {
                workspace: accounts::sign_ins::fixture::WORKSPACE.into(),
                item: item_for(sign_ins::MODEL_HUB_ACCOUNT),
            };
            assert!(
                matches!(asked.ask(&entry), Ok(Answer::Refused { .. })),
                "the sign-in went with the account"
            );
        }

        phase a_refused_sign_in_is_said_in_the_sessions_words;
        {
            let dir = tempfile::tempdir().unwrap();
            let session = Fixture::serve_models(dir.path());
            session.keep_account(hub_account(serde_json::json!({"server_url": HUB_ORIGIN})));
            let refused = stored_credential_at(&session.socket()).err().unwrap();
            assert_eq!(refused, "This workspace's vault keeps no such entry");
        }
    }
}
