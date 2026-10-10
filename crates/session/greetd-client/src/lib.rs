//! greetd IPC client — the authenticate-then-start exchange.
//!
//! greetd speaks a small request/response protocol over the Unix socket
//! named by `$GREETD_SOCK`. The greeter:
//!
//! 1. `CreateSession { username }`
//! 2. loops over `AuthMessage`s, answering each via
//!    `PostAuthMessageResponse { response }`:
//!      * `Secret`  → the password (hidden prompt, e.g. "Password:")
//!      * `Visible` → the username (shown prompt, some PAM stacks ask)
//!      * `Info`    → an informational line (e.g. fprintd "Place your
//!        finger on the reader"); surfaced to the UI and
//!        answered with `None` so the conversation advances.
//!      * `Error`   → a `PAM_ERROR_MSG` conversation message (e.g. fprintd
//!        "swipe didn't match, try again"). This is *not* a
//!        terminal failure — the authoritative result is a
//!        top-level [`Response::Error`]/[`Response::Success`].
//!        We surface it (styled as a warning) and continue.
//! 3. on [`Response::Success`], `StartSession { cmd, env }` and (on the
//!    second `Success`) the session is launching — the greeter exits.
//! 4. on [`Response::Error`], `CancelSession` and report a mapped,
//!    user-facing [`AuthError`].
//!
//! The protocol loop is factored into [`run_exchange`], generic over any
//! `Read + Write` stream, so it can be exercised with an in-memory socket
//! pair in tests. Multi-prompt PAM (fingerprint) drops straight out: the
//! loop already handles an arbitrary sequence of auth messages and reports
//! each prompt to the caller via the `on_prompt` callback.

#![cfg_attr(
    test,
    allow(
        clippy::string_slice,
        clippy::unreachable,
        reason = "in a test these are the assertion, not a hazard"
    )
)]

use std::io::{Read, Write};
use std::os::unix::net::UnixStream;

pub use greetd_ipc::AuthMessageType;
use greetd_ipc::codec::SyncCodec;
use greetd_ipc::{ErrorType, Request, Response};
use zeroize::Zeroize;

mod conversation;
pub use conversation::{ConversationControl, authenticate_conversation};

/// Why an authentication attempt did not result in a started session.
///
/// The variants exist so a login surface can say what happened and what to
/// do about it, in its own words and language, while the raw diagnostic
/// stays available for logs as [`AuthError::detail`]. A rejected credential
/// never says *which* field was wrong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuthError {
    /// The caller cancelled the configured session before it started.
    Cancelled,
    /// PAM refused the attempt. greetd reports every PAM failure as an
    /// authentication error; `reason` is what the PAM result code means and
    /// `detail` is greetd's raw description (`pam_authenticate: AUTH_ERR`).
    Denied { reason: Denial, detail: String },
    /// The credentials were accepted, but greetd could not start the
    /// session. The `String` is the raw greetd description.
    SessionStart(String),
    /// greetd refused the request for a reason other than PAM (a session
    /// already being configured). The `String` is the raw description.
    Greetd(String),
    /// The conversation could not be carried out at all (socket connect /
    /// read / write / decode failure). The `String` is the raw error.
    Transport(String),
}

/// What a PAM refusal means for the person signing in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Denial {
    /// Wrong passcode, or an account that does not exist. The two are
    /// deliberately indistinguishable.
    Credentials,
    /// Too many failed attempts; the account is locked for now.
    Locked,
    /// The account itself has expired.
    AccountExpired,
    /// The passcode has expired and must be changed first.
    PasscodeExpired,
    /// The account may not sign in here.
    NotPermitted,
    /// PAM could not check the credentials at all (a module or system
    /// error), so retrying may succeed.
    Unavailable,
}

impl Denial {
    /// Classify greetd's description of a PAM failure, which names the PAM
    /// step and its result code, as in `pam_acct_mgmt: ACCT_EXPIRED`.
    pub fn from_description(description: &str) -> Self {
        let code = description
            .rsplit(|c: char| c == ':' || c.is_whitespace())
            .find(|word| !word.is_empty())
            .unwrap_or_default();
        match code {
            "AUTH_ERR" | "USER_UNKNOWN" | "CRED_INSUFFICIENT" => Self::Credentials,
            "MAXTRIES" => Self::Locked,
            "ACCT_EXPIRED" => Self::AccountExpired,
            "NEW_AUTHTOK_REQD" | "AUTHTOK_EXPIRED" | "CRED_EXPIRED" => Self::PasscodeExpired,
            "PERM_DENIED" => Self::NotPermitted,
            _ => Self::Unavailable,
        }
    }
}

impl AuthError {
    /// A short English sentence for surfaces without their own catalogue.
    /// Never a raw PAM or greetd string.
    pub fn user_message(&self) -> String {
        match self {
            AuthError::Cancelled => "Sign-in cancelled".to_string(),
            AuthError::Denied { reason, .. } => match reason {
                Denial::Credentials => "Incorrect username or passcode",
                Denial::Locked => "Too many failed attempts. Try again later",
                Denial::AccountExpired => "This account has expired",
                Denial::PasscodeExpired => "Your passcode has expired",
                Denial::NotPermitted => "This account cannot sign in here",
                Denial::Unavailable => "Your passcode could not be checked. Try again",
            }
            .to_string(),
            AuthError::SessionStart(_) | AuthError::Greetd(_) => {
                "Could not start the session".to_string()
            }
            AuthError::Transport(_) => "Could not reach the login service".to_string(),
        }
    }

    /// The raw diagnostic, for logs only. `None` when there is nothing
    /// beyond the classification.
    pub fn detail(&self) -> Option<String> {
        let d = match self {
            AuthError::Cancelled => return None,
            AuthError::Denied { detail, .. } => detail,
            AuthError::SessionStart(d) | AuthError::Greetd(d) | AuthError::Transport(d) => d,
        };
        let d = d.trim();
        (!d.is_empty()).then(|| d.to_string())
    }
}

/// A prompt emitted by the PAM conversation that the user should see while
/// authentication is in progress — e.g. fprintd's "Place your finger on
/// the reader" (`is_error == false`) or "swipe didn't match, try again"
/// (`is_error == true`). Delivered to `on_prompt` as the conversation
/// advances so the greeter can reflect it live.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthPrompt {
    pub text: String,
    /// True for PAM_ERROR_MSG-style messages (render as a warning). These
    /// are informational, not terminal — see the module docs.
    pub is_error: bool,
}

/// Map a top-level greetd [`Response::Error`] into an [`AuthError`].
fn map_error(error_type: &ErrorType, description: String) -> AuthError {
    match error_type {
        ErrorType::AuthError => AuthError::Denied {
            reason: Denial::from_description(&description),
            detail: description,
        },
        ErrorType::Error => AuthError::Greetd(description),
    }
}

/// Tear down greetd's half-open session so the next attempt starts clean,
/// and wait for its answer: closing the socket first leaves greetd writing
/// that answer into a broken pipe.
fn cancel_session<S: Read + Write>(stream: &mut S) {
    if Request::CancelSession.write_to(stream).is_ok() {
        let _ = Response::read_from(stream);
    }
}

/// Connect to greetd at `sock_path` and run the full
/// authenticate-and-start exchange, reporting any PAM conversation prompts
/// to `on_prompt`. On success this **never returns** — it
/// `process::exit(0)`s so greetd takes over the VT. On failure it returns
/// a mapped [`AuthError`] for the status line.
///
/// # Errors
///
/// Returns [`AuthError::Transport`] if `sock_path` can't be connected to or
/// the exchange fails at the transport level, [`AuthError::Denied`] if PAM
/// refuses the attempt, [`AuthError::SessionStart`] if the accepted session
/// cannot start, or [`AuthError::Greetd`] on any other greetd-reported
/// failure.
#[allow(
    clippy::exit,
    reason = "the documented success path: greetd takes over the VT, so the \
              greeter process ends here rather than returning to its caller"
)]
pub fn authenticate_and_start<F: FnMut(AuthPrompt), G: FnMut()>(
    sock_path: &str,
    username: &str,
    password: &str,
    cmd: &[String],
    env: &[String],
    on_prompt: F,
    on_accepted: G,
) -> Result<(), AuthError> {
    let mut stream = UnixStream::connect(sock_path).map_err(|e| {
        AuthError::Transport(format!("cannot connect to greetd ({sock_path}): {e}"))
    })?;

    run_exchange_with(
        &mut stream,
        username,
        password,
        cmd,
        env,
        on_prompt,
        on_accepted,
    )?;

    // Session accepted — hand the VT to greetd. The greeter must exit so
    // greetd can reparent the started session.
    std::process::exit(0);
}

/// The greetd protocol loop, generic over the transport so it is unit
/// testable against an in-memory `UnixStream` pair. Returns `Ok(())` once
/// greetd has accepted `StartSession` (the caller then exits the process),
/// or a mapped [`AuthError`] on failure. Every PAM conversation prompt is
/// reported to `on_prompt`.
///
/// # Errors
///
/// Returns [`AuthError::Transport`] on a read/write/decode failure,
/// [`AuthError::Denied`] if PAM refuses the attempt, or
/// [`AuthError::Greetd`] on any other greetd-reported failure.
pub fn run_exchange<S: Read + Write, F: FnMut(AuthPrompt)>(
    stream: &mut S,
    username: &str,
    password: &str,
    cmd: &[String],
    env: &[String],
    on_prompt: F,
) -> Result<(), AuthError> {
    run_exchange_with(stream, username, password, cmd, env, on_prompt, || {})
}

/// [`run_exchange`] plus an `on_accepted` hook, fired the moment greetd
/// reports the credentials good and *before* `StartSession` is issued.
///
/// It exists so the UI can resolve its in-flight state into a visible
/// success frame. Without it the only observable outcomes are "failed" and
/// "the process vanished", so a successful login looks, for the moment it
/// takes greetd to hand over the VT, exactly like a greeter that has hung.
///
/// # Errors
///
/// Returns [`AuthError::Transport`] on a read/write/decode failure,
/// [`AuthError::Denied`] if PAM refuses the attempt, or
/// [`AuthError::Greetd`] on any other greetd-reported failure.
pub fn run_exchange_with<S: Read + Write, F: FnMut(AuthPrompt), G: FnMut()>(
    stream: &mut S,
    username: &str,
    password: &str,
    cmd: &[String],
    env: &[String],
    mut on_prompt: F,
    on_accepted: G,
) -> Result<(), AuthError> {
    exchange(
        &mut StreamExchange(stream),
        username,
        cmd,
        env,
        move |kind, text| {
            Ok(match kind {
                AuthMessageType::Secret => Some(password.to_string()),
                AuthMessageType::Visible => Some(username.to_string()),
                AuthMessageType::Info | AuthMessageType::Error => {
                    on_prompt(AuthPrompt {
                        text,
                        is_error: matches!(kind, AuthMessageType::Error),
                    });
                    None
                }
            })
        },
        on_accepted,
    )
}

trait ExchangeIo {
    fn send(&mut self, request: Request) -> Result<(), AuthError>;
    fn receive(&mut self) -> Result<Response, AuthError>;
    fn check_cancelled(&mut self) -> Result<(), AuthError> {
        Ok(())
    }
    fn prepare_start(&mut self) -> Result<(), AuthError> {
        Ok(())
    }
    fn terminal_failure(&mut self) -> Result<(), AuthError> {
        Ok(())
    }
    fn cancel(&mut self) {
        if self.send(Request::CancelSession).is_ok() {
            let _ = self.receive();
        }
    }
}

struct StreamExchange<'a, S>(&'a mut S);

impl<S: Read + Write> ExchangeIo for StreamExchange<'_, S> {
    fn send(&mut self, request: Request) -> Result<(), AuthError> {
        write_request(request, self.0)
            .map_err(|error| AuthError::Transport(format!("write request: {error}")))
    }

    fn receive(&mut self) -> Result<Response, AuthError> {
        Response::read_from(self.0)
            .map_err(|error| AuthError::Transport(format!("read response: {error}")))
    }
}

fn write_request<S: Write>(
    mut request: Request,
    stream: &mut S,
) -> Result<(), greetd_ipc::codec::Error> {
    let result = request.write_to(stream);
    if let Request::PostAuthMessageResponse {
        response: Some(response),
    } = &mut request
    {
        response.zeroize();
    }
    result
}

fn exchange<I: ExchangeIo, F, G>(
    stream: &mut I,
    username: &str,
    cmd: &[String],
    env: &[String],
    mut response: F,
    mut on_accepted: G,
) -> Result<(), AuthError>
where
    F: FnMut(AuthMessageType, String) -> Result<Option<String>, AuthError>,
    G: FnMut(),
{
    stream.send(Request::CreateSession {
        username: username.to_owned(),
    })?;
    loop {
        stream.check_cancelled()?;
        let message = stream.receive()?;
        stream.check_cancelled()?;
        match message {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let reply = match response(auth_message_type, auth_message) {
                    Ok(reply) => reply,
                    Err(error) => {
                        stream.cancel();
                        return Err(error);
                    }
                };
                stream.check_cancelled()?;
                stream.send(Request::PostAuthMessageResponse { response: reply })?;
            }
            Response::Success => {
                // Seal start against cancellation before exposing acceptance;
                // the ordinary acceptance hook still precedes StartSession.
                stream.prepare_start()?;
                on_accepted();
                stream.send(Request::StartSession {
                    cmd: cmd.to_vec(),
                    env: env.to_vec(),
                })?;
                match stream.receive()? {
                    Response::Success => return Ok(()),
                    Response::Error {
                        error_type,
                        description,
                    } => {
                        stream.cancel();
                        tracing::warn!(?error_type, "greetd could not start the session");
                        return Err(AuthError::SessionStart(description));
                    }
                    Response::AuthMessage { .. } => {
                        stream.cancel();
                        return Err(AuthError::SessionStart(
                            "unexpected auth prompt after StartSession".into(),
                        ));
                    }
                }
            }
            Response::Error {
                error_type,
                description,
            } => {
                stream.terminal_failure()?;
                stream.cancel();
                tracing::warn!(?error_type, "greetd auth error");
                return Err(map_error(&error_type, description));
            }
        }
    }
}

/// Run the greetd conversation to **verify** `username`/`password` WITHOUT
/// starting a session — the primitive an *active-session* unlock needs
/// (session-lock), as opposed to *login* ([`run_exchange`], which launches
/// the session on success).
///
/// The `CreateSession` + auth-message answering is identical to
/// [`run_exchange`], but on the authoritative [`Response::Success`] this
/// sends `CancelSession` (tearing down greetd's transient session slot) and
/// returns `Ok(())` — it never sends `StartSession`, so no session is
/// spawned and the caller (the compositor lock screen) simply clears the
/// lock. A refusal maps to [`AuthError::Denied`] exactly as the login path
/// does, so the non-leaky user messaging is shared.
///
/// Generic over the transport so it is unit-testable against an in-memory
/// `UnixStream` pair, mirroring [`run_exchange`]'s tests.
///
/// # Errors
///
/// Returns [`AuthError::Transport`] on a read/write/decode failure,
/// [`AuthError::Denied`] if PAM refuses the attempt, or
/// [`AuthError::Greetd`] on any other greetd-reported failure.
pub fn verify_exchange<S: Read + Write, F: FnMut(AuthPrompt)>(
    stream: &mut S,
    username: &str,
    password: &str,
    mut on_prompt: F,
) -> Result<(), AuthError> {
    Request::CreateSession {
        username: username.to_string(),
    }
    .write_to(stream)
    .map_err(|e| AuthError::Transport(format!("CreateSession: {e}")))?;

    loop {
        let response = Response::read_from(stream)
            .map_err(|e| AuthError::Transport(format!("read response: {e}")))?;
        match response {
            Response::AuthMessage {
                auth_message_type,
                auth_message,
            } => {
                let reply = match auth_message_type {
                    AuthMessageType::Secret => Some(password.to_string()),
                    AuthMessageType::Visible => Some(username.to_string()),
                    AuthMessageType::Info => {
                        on_prompt(AuthPrompt {
                            text: auth_message.clone(),
                            is_error: false,
                        });
                        None
                    }
                    AuthMessageType::Error => {
                        on_prompt(AuthPrompt {
                            text: auth_message.clone(),
                            is_error: true,
                        });
                        None
                    }
                };
                Request::PostAuthMessageResponse { response: reply }
                    .write_to(stream)
                    .map_err(|e| AuthError::Transport(format!("PostAuthMessageResponse: {e}")))?;
            }
            Response::Success => {
                // Credentials verified. Unlike the login path we do NOT
                // `StartSession`; tear down greetd's slot and report success
                // so the compositor clears the lock.
                Request::CancelSession
                    .write_to(stream)
                    .map_err(|e| AuthError::Transport(format!("CancelSession: {e}")))?;
                return Ok(());
            }
            Response::Error {
                error_type,
                description,
            } => {
                cancel_session(stream);
                tracing::warn!(?error_type, "greetd verify error");
                return Err(map_error(&error_type, description));
            }
        }
    }
}

/// Connect to greetd at `sock_path` and verify `username`/`password`
/// without starting a session (see [`verify_exchange`]). The convenience
/// entry point the compositor's session-lock unlock path calls; prompts are
/// discarded (an unlock box does not surface fingerprint copy).
///
/// # Errors
///
/// Returns [`AuthError::Transport`] if `sock_path` can't be connected to,
/// [`AuthError::Denied`] if PAM refuses the attempt, or
/// [`AuthError::Greetd`] on any other greetd-reported failure.
pub fn verify_password(sock_path: &str, username: &str, password: &str) -> Result<(), AuthError> {
    let mut stream = UnixStream::connect(sock_path).map_err(|e| {
        AuthError::Transport(format!("cannot connect to greetd ({sock_path}): {e}"))
    })?;
    verify_exchange(&mut stream, username, password, |_| {})
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::thread;

    /// Convenience: a no-op prompt sink for tests that don't assert on
    /// prompts.
    fn ignore_prompts(_: AuthPrompt) {}

    #[test]
    fn login_exchange_answers_every_prompt_and_completes() {
        // Phase `successful_secret_exchange_completes`.
        // Drive a successful Secret-prompt exchange against a mock greetd
        // running on the other end of a `UnixStream` pair. Proves the client
        // answers the password and completes `StartSession`.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                // 1. CreateSession
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::CreateSession { username } if username == "alice"));

                // 2. Ask for the secret.
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Secret,
                    auth_message: "Password:".to_string(),
                }
                .write_to(&mut server)
                .unwrap();

                // 3. Receive the password.
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::PostAuthMessageResponse { response } if response == Some("s3cr3t".to_string()))
                );

                // 4. Auth complete.
                Response::Success.write_to(&mut server).unwrap();

                // 5. StartSession with our argv.
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::StartSession { cmd, .. } if cmd == vec!["/usr/bin/shell".to_string(), "--backend".to_string(), "drm".to_string()])
                );

                // 6. Session accepted.
                Response::Success.write_to(&mut server).unwrap();
            });

            let mut client = client;
            let cmd = vec![
                "/usr/bin/shell".to_string(),
                "--backend".to_string(),
                "drm".to_string(),
            ];
            let result = run_exchange(&mut client, "alice", "s3cr3t", &cmd, &[], ignore_prompts);
            server_thread.join().unwrap();
            assert_eq!(result, Ok(()));
        }
        // Phase `visible_prompt_is_answered_with_username`.
        // Some PAM stacks ask for the username via a `Visible` prompt. The
        // client must answer it with the username (not the password).
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                let _ = Request::read_from(&mut server).unwrap(); // CreateSession
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Visible,
                    auth_message: "Username:".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::PostAuthMessageResponse { response } if response == Some("bob".to_string())),
                    "Visible prompt must be answered with the username"
                );
                Response::Success.write_to(&mut server).unwrap();
                let _ = Request::read_from(&mut server).unwrap(); // StartSession
                Response::Success.write_to(&mut server).unwrap();
            });

            let mut client = client;
            let result = run_exchange(&mut client, "bob", "pw", &[], &[], ignore_prompts);
            server_thread.join().unwrap();
            assert_eq!(result, Ok(()));
        }
        // Phase `info_and_error_prompts_are_surfaced_and_do_not_abort`.
        // A fingerprint-style conversation: an `Info` prompt ("Place your
        // finger…"), then an `Error` prompt ("didn't match, try again"), then
        // `Success`. Proves these conversation messages are SURFACED to the
        // caller and do NOT abort the loop — the authoritative outcome is the
        // top-level Success.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                let _ = Request::read_from(&mut server).unwrap(); // CreateSession

                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Info,
                    auth_message: "Place your finger on the reader".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::PostAuthMessageResponse { response } if response.is_none()),
                    "Info prompt must be acked with None"
                );

                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Error,
                    auth_message: "Swipe didn't match, try again".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::PostAuthMessageResponse { response } if response.is_none()),
                    "Error-type message must be acked with None and not abort"
                );

                Response::Success.write_to(&mut server).unwrap();
                let _ = Request::read_from(&mut server).unwrap(); // StartSession
                Response::Success.write_to(&mut server).unwrap();
            });

            let mut client = client;
            let mut prompts: Vec<AuthPrompt> = Vec::new();
            let result = run_exchange(&mut client, "alice", "pw", &[], &[], |p| prompts.push(p));
            server_thread.join().unwrap();

            assert_eq!(result, Ok(()));
            assert_eq!(prompts.len(), 2, "both conversation prompts surfaced");
            assert_eq!(prompts[0].text, "Place your finger on the reader");
            assert!(!prompts[0].is_error, "Info prompt is not a warning");
            assert_eq!(prompts[1].text, "Swipe didn't match, try again");
            assert!(prompts[1].is_error, "Error-type prompt is a warning");
        }
    }

    #[test]
    fn greetd_errors_map_to_login_failures_and_cancel() {
        // Phase `auth_error_maps_to_denied_credentials_and_cancels`.
        // A bad password yields `Response::Error { AuthError }`; the client
        // cancels and maps it to a credentials denial, whose message is the
        // vague, non-leaky string.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                let _ = Request::read_from(&mut server).unwrap(); // CreateSession
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Secret,
                    auth_message: "Password:".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let _ = Request::read_from(&mut server).unwrap(); // PostAuthMessageResponse
                Response::Error {
                    error_type: ErrorType::AuthError,
                    description: "pam_authenticate: AUTH_ERR".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                // The client should send CancelSession to clean up, and wait
                // for greetd's answer instead of closing on it.
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::CancelSession));
                Response::Success.write_to(&mut server).unwrap();
            });

            let mut client = client;
            let result = run_exchange(&mut client, "alice", "wrong", &[], &[], ignore_prompts);
            server_thread.join().unwrap();

            let err = result.unwrap_err();
            assert!(matches!(
                err,
                AuthError::Denied {
                    reason: Denial::Credentials,
                    ..
                }
            ));
            // The raw PAM string must NOT leak into the user-facing message.
            assert_eq!(err.user_message(), "Incorrect username or passcode");
            assert_eq!(err.detail().as_deref(), Some("pam_authenticate: AUTH_ERR"));
        }
        // Phase `generic_error_maps_to_greetd_with_detail`.
        // A non-auth greetd error (`ErrorType::Error`) maps to
        // [`AuthError::Greetd`] with the raw description preserved as detail.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                let _ = Request::read_from(&mut server).unwrap(); // CreateSession
                Response::Error {
                    error_type: ErrorType::Error,
                    description: "session slot busy".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::CancelSession));
            });

            let mut client = client;
            let result = run_exchange(&mut client, "alice", "pw", &[], &[], ignore_prompts);
            server_thread.join().unwrap();

            assert_eq!(
                result,
                Err(AuthError::Greetd("session slot busy".to_string()))
            );
            let err = result.unwrap_err();
            assert_eq!(err.user_message(), "Could not start the session");
            assert_eq!(err.detail(), Some("session slot busy".to_string()));
        }
        // Phase `start_session_failure_is_mapped_and_cancelled`.
        // A failure during `StartSession` (after a successful auth) is mapped
        // too, and the client cancels the half-open session.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                let _ = Request::read_from(&mut server).unwrap(); // CreateSession
                Response::Success.write_to(&mut server).unwrap(); // auth ok
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::StartSession { .. }));
                Response::Error {
                    error_type: ErrorType::Error,
                    description: "no such session command".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::CancelSession));
            });

            let mut client = client;
            let result = run_exchange(&mut client, "alice", "pw", &[], &[], ignore_prompts);
            server_thread.join().unwrap();
            assert_eq!(
                result,
                Err(AuthError::SessionStart(
                    "no such session command".to_string()
                ))
            );
        }
    }

    /// `AuthError::user_message` / `detail` mapping, checked directly so a
    /// future refactor can't silently change the user-facing copy.
    #[test]
    fn auth_error_messages_are_plain_and_details_stay_raw() {
        {
            let denied = |description: &str| AuthError::Denied {
                reason: Denial::from_description(description),
                detail: description.to_string(),
            };
            assert_eq!(
                denied("pam_authenticate: AUTH_ERR").user_message(),
                "Incorrect username or passcode"
            );
            assert_eq!(denied("").detail(), None);

            let g = AuthError::Greetd("  boom  ".to_string());
            assert_eq!(g.user_message(), "Could not start the session");
            assert_eq!(g.detail(), Some("boom".to_string()), "detail is trimmed");

            let t = AuthError::Transport("connection refused".to_string());
            assert_eq!(t.user_message(), "Could not reach the login service");
            assert_eq!(t.detail(), Some("connection refused".to_string()));

            // An all-whitespace description collapses to no detail.
            assert_eq!(AuthError::Greetd("   ".to_string()).detail(), None);
        }
    }

    #[test]
    fn verify_exchange_checks_credentials_without_starting_a_session() {
        // Phase `verify_exchange_succeeds_and_never_starts_a_session`.
        // The verify-only exchange (session-lock unlock) answers the secret,
        // and on `Success` sends `CancelSession` and returns `Ok` **without**
        // ever sending `StartSession` — no session is launched by an unlock.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                // 1. CreateSession
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::CreateSession { username } if username == "alice"));
                // 2. Ask for the secret.
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Secret,
                    auth_message: "Password:".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                // 3. Receive the password.
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::PostAuthMessageResponse { response } if response == Some("s3cr3t".to_string()))
                );
                // 4. Auth complete.
                Response::Success.write_to(&mut server).unwrap();
                // 5. The verify path MUST cancel, not StartSession.
                let req = Request::read_from(&mut server).unwrap();
                assert!(
                    matches!(req, Request::CancelSession),
                    "verify must CancelSession on success, never StartSession"
                );
            });

            let mut client = client;
            let result = verify_exchange(&mut client, "alice", "s3cr3t", ignore_prompts);
            server_thread.join().unwrap();
            assert_eq!(result, Ok(()));
        }
        // Phase `verify_exchange_bad_password_maps_to_bad_credentials`.
        // A bad password in the verify path maps to the same non-leaky
        // credentials denial and cancels the slot.
        {
            let (client, mut server) = UnixStream::pair().unwrap();

            let server_thread = thread::spawn(move || {
                let _ = Request::read_from(&mut server).unwrap(); // CreateSession
                Response::AuthMessage {
                    auth_message_type: AuthMessageType::Secret,
                    auth_message: "Password:".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let _ = Request::read_from(&mut server).unwrap(); // PostAuthMessageResponse
                Response::Error {
                    error_type: ErrorType::AuthError,
                    description: "pam_authenticate: AUTH_ERR".to_string(),
                }
                .write_to(&mut server)
                .unwrap();
                let req = Request::read_from(&mut server).unwrap();
                assert!(matches!(req, Request::CancelSession));
            });

            let mut client = client;
            let result = verify_exchange(&mut client, "alice", "wrong", ignore_prompts);
            server_thread.join().unwrap();
            assert!(matches!(
                result,
                Err(AuthError::Denied {
                    reason: Denial::Credentials,
                    ..
                })
            ));
            assert_eq!(
                result.unwrap_err().user_message(),
                "Incorrect username or passcode"
            );
        }
    }

    #[test]
    fn every_pam_outcome_greetd_reports_has_a_meaning() {
        for (description, reason) in [
            ("pam_authenticate: AUTH_ERR", Denial::Credentials),
            ("pam_authenticate: USER_UNKNOWN", Denial::Credentials),
            ("pam_authenticate: CRED_INSUFFICIENT", Denial::Credentials),
            ("pam_authenticate: MAXTRIES", Denial::Locked),
            ("pam_acct_mgmt: ACCT_EXPIRED", Denial::AccountExpired),
            ("pam_acct_mgmt: NEW_AUTHTOK_REQD", Denial::PasscodeExpired),
            ("pam_authenticate: CRED_EXPIRED", Denial::PasscodeExpired),
            ("pam_acct_mgmt: PERM_DENIED", Denial::NotPermitted),
            ("pam_authenticate: AUTHINFO_UNAVAIL", Denial::Unavailable),
            ("pam_authenticate: SERVICE_ERR", Denial::Unavailable),
            ("abort error: pam_authenticate: ABORT", Denial::Unavailable),
            ("", Denial::Unavailable),
        ] {
            assert_eq!(
                Denial::from_description(description),
                reason,
                "{description}"
            );
            let message = AuthError::Denied {
                reason,
                detail: description.to_string(),
            }
            .user_message();
            assert!(
                !message.contains("pam") && !message.contains('_'),
                "{description} leaked into {message:?}"
            );
        }
    }
}
