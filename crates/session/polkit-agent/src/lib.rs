//! The session's polkit authentication agent.
//!
//! `pkexec`, GNOME Disks, virt-manager and system-wide Flatpak installs ask
//! polkit for an administrator's consent; polkit asks the session's
//! registered agent, and without one they fail with "no authentication
//! agent". The shell registers this agent ([`service`]), which asks in a
//! sheet the compositor draws through the shell's [`service::Sheets`] —
//! never in a window an application could imitate or read keystrokes from.
//!
//! The password never touches polkit's D-Bus API: it goes to
//! `polkit-agent-helper-1` ([`helper`]), which runs PAM and reports back.
//! [`authenticate`] is the conversation with that helper, written against
//! [`HelperSpawner`] and [`Prompter`] so it can be tested without either a
//! setuid binary or a screen.

pub mod helper;
pub mod service;

use tokio::io::{AsyncBufReadExt as _, AsyncWriteExt as _};

/// Attempts before an authentication request fails outright, as polkit's
/// own agents do.
pub const MAX_ATTEMPTS: usize = 3;

/// What `polkit-agent-helper-1` says, one line at a time.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HelperEvent {
    /// PAM wants a secret (`PAM_PROMPT_ECHO_OFF`).
    SecretPrompt(String),
    /// PAM wants something visible, such as a user name (`PAM_PROMPT_ECHO_ON`).
    VisiblePrompt(String),
    Error(String),
    Info(String),
    Success,
    Failure,
}

/// `g_strcompress`: undo the C escapes the helper applies to each line.
fn unescape(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut chars = line.chars();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        match chars.next() {
            Some('n') => out.push('\n'),
            Some('t') => out.push('\t'),
            Some('r') => out.push('\r'),
            Some('\\') | None => out.push('\\'),
            Some('"') => out.push('"'),
            Some(other) => {
                out.push('\\');
                out.push(other);
            }
        }
    }
    out
}

impl HelperEvent {
    /// Parse one helper line; `None` for a line the protocol does not have.
    pub fn parse(line: &str) -> Option<Self> {
        let line = line.trim_end_matches(['\n', '\r']);
        let text = |prefix: &str| {
            line.strip_prefix(prefix)
                .map(|rest| unescape(rest.trim_start()))
        };
        if line == "SUCCESS" {
            Some(Self::Success)
        } else if line == "FAILURE" {
            Some(Self::Failure)
        } else if let Some(prompt) = text("PAM_PROMPT_ECHO_OFF") {
            Some(Self::SecretPrompt(prompt))
        } else if let Some(prompt) = text("PAM_PROMPT_ECHO_ON") {
            Some(Self::VisiblePrompt(prompt))
        } else if let Some(message) = text("PAM_ERROR_MSG") {
            Some(Self::Error(message))
        } else {
            text("PAM_TEXT_INFO").map(Self::Info)
        }
    }
}

/// One running helper: its output lines and its input.
pub struct HelperIo {
    pub output: Box<dyn tokio::io::AsyncBufRead + Unpin + Send>,
    pub input: Box<dyn tokio::io::AsyncWrite + Unpin + Send>,
}

/// Starts a helper conversation authenticating `user` for `cookie`.
pub trait HelperSpawner: Send + Sync {
    fn spawn(
        &self,
        user: &str,
        cookie: &str,
    ) -> impl std::future::Future<Output = std::io::Result<HelperIo>> + Send;
}

/// What the sheet shows for one prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptView {
    /// polkit's message for the action, e.g. "Authentication is required to
    /// install software".
    pub message: String,
    /// The polkit action id, shown so the request can be checked.
    pub action_id: String,
    /// Who may authorise it, as user names; more than one shows a picker.
    pub identities: Vec<String>,
    /// The identity currently being authenticated.
    pub identity: usize,
    /// PAM's prompt, e.g. "Password:".
    pub prompt: String,
    /// Whether the answer is shown as typed.
    pub echo: bool,
    /// What went wrong last time, or PAM's informational text.
    pub notice: Option<String>,
}

/// The user's answer to a [`PromptView`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAnswer {
    pub identity: usize,
    pub secret: String,
}

/// A notice for an attempt the helper ended without saying why.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StockNotice {
    /// The helper went away, or would not take the answer.
    HelperEnded,
    /// The helper refused the answer and PAM said nothing.
    Rejected,
}

impl StockNotice {
    /// Every notice, for a host that checks its catalogue has words for each.
    pub const ALL: [Self; 2] = [Self::HelperEnded, Self::Rejected];

    /// The Fluent message a host resolves for this notice.
    #[must_use]
    pub const fn message_key(self) -> &'static str {
        match self {
            Self::HelperEnded => "portal-auth-ended",
            Self::Rejected => "portal-auth-rejected",
        }
    }
}

/// Asks the user; `None` when they dismissed the sheet.
pub trait Prompter: Send + Sync {
    fn ask(
        &self,
        view: PromptView,
    ) -> impl std::future::Future<Output = Option<PromptAnswer>> + Send;

    /// What the sheet says for `notice`, in the person's language. The
    /// default is the notice's Fluent message key; a prompter that draws for
    /// a person resolves it in its catalogue.
    fn stock_notice(&self, notice: StockNotice) -> String {
        notice.message_key().to_owned()
    }
}

/// How an authentication request ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// The helper reported success for this identity.
    Authorized { identity: String },
    /// The user dismissed the sheet.
    Cancelled,
    /// Every attempt failed, or no helper could run.
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthRequest {
    pub action_id: String,
    pub message: String,
    pub cookie: String,
    /// User names that may authorise it, in polkit's order.
    pub identities: Vec<String>,
}

enum Attempt {
    Done(Outcome),
    /// The helper failed; try again, saying why.
    Retry(String),
    /// The user picked another identity (carrying the secret they typed).
    Switch(usize, String),
}

async fn read_event(
    output: &mut (dyn tokio::io::AsyncBufRead + Unpin + Send),
) -> Option<HelperEvent> {
    let mut line = String::new();
    loop {
        line.clear();
        match output.read_line(&mut line).await {
            Ok(0) | Err(_) => return None,
            Ok(_) => {
                if let Some(event) = HelperEvent::parse(&line) {
                    return Some(event);
                }
            }
        }
    }
}

async fn answer(input: &mut (dyn tokio::io::AsyncWrite + Unpin + Send), secret: &str) -> bool {
    if secret.contains('\n') {
        return false;
    }
    input.write_all(secret.as_bytes()).await.is_ok()
        && input.write_all(b"\n").await.is_ok()
        && input.flush().await.is_ok()
}

async fn attempt(
    spawner: &impl HelperSpawner,
    prompter: &impl Prompter,
    request: &AuthRequest,
    identity: usize,
    mut pending_secret: Option<String>,
    mut notice: Option<String>,
) -> Attempt {
    let Some(user) = request.identities.get(identity) else {
        return Attempt::Done(Outcome::Failed("no identity can authorise this".into()));
    };
    let mut io = match spawner.spawn(user, &request.cookie).await {
        Ok(io) => io,
        Err(error) => {
            return Attempt::Done(Outcome::Failed(format!(
                "the authentication helper could not start: {error}"
            )));
        }
    };
    loop {
        let Some(event) = read_event(io.output.as_mut()).await else {
            return Attempt::Retry(prompter.stock_notice(StockNotice::HelperEnded));
        };
        let (prompt, echo) = match event {
            HelperEvent::Success => {
                return Attempt::Done(Outcome::Authorized {
                    identity: user.clone(),
                });
            }
            HelperEvent::Failure => {
                return Attempt::Retry(
                    notice.unwrap_or_else(|| prompter.stock_notice(StockNotice::Rejected)),
                );
            }
            HelperEvent::Error(message) | HelperEvent::Info(message) => {
                notice = Some(message);
                continue;
            }
            HelperEvent::SecretPrompt(prompt) => (prompt, false),
            HelperEvent::VisiblePrompt(prompt) => (prompt, true),
        };
        let secret = if let Some(secret) = pending_secret.take() {
            secret
        } else {
            let view = PromptView {
                message: request.message.clone(),
                action_id: request.action_id.clone(),
                identities: request.identities.clone(),
                identity,
                prompt,
                echo,
                notice: notice.take(),
            };
            let Some(reply) = prompter.ask(view).await else {
                return Attempt::Done(Outcome::Cancelled);
            };
            if reply.identity != identity {
                return Attempt::Switch(reply.identity, reply.secret);
            }
            reply.secret
        };
        if !answer(io.input.as_mut(), &secret).await {
            return Attempt::Retry(prompter.stock_notice(StockNotice::HelperEnded));
        }
    }
}

/// Which identity to start with: the user running the session when they
/// are allowed to authorise, else polkit's first.
pub fn initial_identity(identities: &[String], current_user: Option<&str>) -> usize {
    current_user
        .and_then(|user| identities.iter().position(|identity| identity == user))
        .unwrap_or(0)
}

/// Run one authentication request to its end.
pub async fn authenticate(
    spawner: &impl HelperSpawner,
    prompter: &impl Prompter,
    request: &AuthRequest,
    first_identity: usize,
) -> Outcome {
    if request.identities.is_empty() {
        return Outcome::Failed("no identity can authorise this".into());
    }
    let mut identity = first_identity.min(request.identities.len().saturating_sub(1));
    let mut pending = None;
    let mut notice = None;
    let mut failures = 0_usize;
    loop {
        match attempt(
            spawner,
            prompter,
            request,
            identity,
            pending.take(),
            notice.take(),
        )
        .await
        {
            Attempt::Done(outcome) => return outcome,
            Attempt::Switch(next, secret) if next < request.identities.len() => {
                identity = next;
                pending = Some(secret);
            }
            Attempt::Switch(..) => {}
            Attempt::Retry(reason) => {
                failures = failures.saturating_add(1);
                if failures >= MAX_ATTEMPTS {
                    return Outcome::Failed(reason);
                }
                notice = Some(reason);
            }
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod copy_lint_tests;
