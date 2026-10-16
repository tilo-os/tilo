//! `org.freedesktop.PolicyKit1.AuthenticationAgent` on the system bus,
//! registered for this login session.

use std::collections::HashMap;
use std::ffi::CStr;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use tokio_util::sync::CancellationToken;
use zbus::zvariant::{OwnedValue, Value};

use super::helper::SystemHelper;
use super::{
    AuthRequest, Outcome, PromptAnswer, PromptView, Prompter, StockNotice, authenticate,
    initial_identity,
};
use tilo_lock::MutexRecover as _;

/// Where the agent object lives on the system bus.
pub const AGENT_PATH: &str = "/org/tilo/PolicyKit1/AuthenticationAgent";

/// The person's answer to one sheet, once they give it.
pub type SheetAnswer = Pin<Box<dyn Future<Output = Option<PromptAnswer>> + Send>>;

/// The sheets the agent asks in, drawn by its host — in the shell, the
/// compositor's own dialog queue, which no application can imitate.
pub trait Sheets: Send + Sync {
    /// Show `view` in the sheet named `handle`, replacing what that sheet
    /// showed. The answer is `None` when the person dismissed the sheet or
    /// it could not be shown.
    fn ask(&self, handle: &str, view: PromptView) -> SheetAnswer;

    /// Take down the sheet named `handle`, if it is still up.
    fn cancel(&self, handle: &str);

    /// What a sheet says for `notice`, in the person's language.
    fn stock_notice(&self, notice: StockNotice) -> String;
}

#[derive(Debug, zbus::DBusError)]
#[zbus(prefix = "org.freedesktop.PolicyKit1.Error")]
pub enum PolkitError {
    #[zbus(error)]
    ZBus(zbus::Error),
    Failed(String),
    Cancelled(String),
}

/// The sheet the compositor draws, keyed by the request's cookie.
pub struct SheetPrompter {
    sheets: Arc<dyn Sheets>,
    handle: String,
}

impl SheetPrompter {
    pub fn new(sheets: Arc<dyn Sheets>, cookie: &str) -> Self {
        Self {
            sheets,
            handle: format!("polkit:{cookie}"),
        }
    }

    pub fn handle(&self) -> &str {
        &self.handle
    }
}

impl Prompter for SheetPrompter {
    async fn ask(&self, view: PromptView) -> Option<PromptAnswer> {
        self.sheets.ask(&self.handle, view).await
    }

    fn stock_notice(&self, notice: StockNotice) -> String {
        self.sheets.stock_notice(notice)
    }
}

fn user_name(uid: u32) -> Option<String> {
    let mut pwd = std::mem::MaybeUninit::<libc::passwd>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 16 * 1024];
    // SAFETY: every pointer is to a live local buffer of the stated size;
    // `result` is only read back as the success flag.
    let status = unsafe {
        libc::getpwuid_r(
            uid,
            pwd.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so `pwd` is initialised and `pw_name`
    // points into `buffer`, which is still alive.
    let name = unsafe { CStr::from_ptr(pwd.assume_init().pw_name) };
    Some(name.to_string_lossy().into_owned())
}

fn group_members(gid: u32) -> Vec<String> {
    let mut group = std::mem::MaybeUninit::<libc::group>::uninit();
    let mut result = std::ptr::null_mut();
    let mut buffer = vec![0_u8; 64 * 1024];
    // SAFETY: as in `user_name`.
    let status = unsafe {
        libc::getgrgid_r(
            gid,
            group.as_mut_ptr(),
            buffer.as_mut_ptr().cast(),
            buffer.len(),
            &raw mut result,
        )
    };
    if status != 0 || result.is_null() {
        return Vec::new();
    }
    // SAFETY: getgrgid_r succeeded, so `gr_mem` is a null-terminated array
    // of C strings inside `buffer`.
    let mut members = Vec::new();
    unsafe {
        let mut cursor = group.assume_init().gr_mem;
        while !cursor.is_null() && !(*cursor).is_null() && members.len() < 64 {
            members.push(CStr::from_ptr(*cursor).to_string_lossy().into_owned());
            cursor = cursor.add(1);
        }
    }
    members
}

fn value_u32<S: std::hash::BuildHasher>(
    details: &HashMap<String, OwnedValue, S>,
    key: &str,
) -> Option<u32> {
    match &**details.get(key)? {
        Value::U32(value) => Some(*value),
        Value::I32(value) => u32::try_from(*value).ok(),
        _ => None,
    }
}

/// polkit's identities as user names: users directly, groups by member.
pub fn resolve_identities<S: std::hash::BuildHasher>(
    identities: &[(String, HashMap<String, OwnedValue, S>)],
) -> Vec<String> {
    let mut names = Vec::new();
    for (kind, details) in identities {
        let found = match kind.as_str() {
            "unix-user" => value_u32(details, "uid")
                .and_then(user_name)
                .into_iter()
                .collect(),
            "unix-group" => value_u32(details, "gid")
                .map(group_members)
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        for name in found {
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    names
}

fn session_user() -> Option<String> {
    user_name(rustix::process::getuid().as_raw())
}

pub struct AuthenticationAgent {
    sheets: Arc<dyn Sheets>,
    live: Arc<Mutex<HashMap<String, CancellationToken>>>,
}

impl AuthenticationAgent {
    pub fn new(sheets: Arc<dyn Sheets>) -> Self {
        Self {
            sheets,
            live: Arc::default(),
        }
    }
}

#[allow(
    clippy::unused_async,
    reason = "the D-Bus interface fixes the method's shape; cancelling is immediate"
)]
#[zbus::interface(name = "org.freedesktop.PolicyKit1.AuthenticationAgent")]
impl AuthenticationAgent {
    /// polkit asks for consent to `action_id`; answered when the user has
    /// authenticated, dismissed the sheet, or run out of attempts.
    async fn begin_authentication(
        &self,
        action_id: String,
        message: String,
        icon_name: String,
        details: HashMap<String, String>,
        cookie: String,
        identities: Vec<(String, HashMap<String, OwnedValue>)>,
    ) -> Result<(), PolkitError> {
        // The sheet is drawn by the compositor in its own style; an
        // application-chosen icon would only help an imitation.
        let _ = icon_name;
        let request = AuthRequest {
            action_id: action_id.clone(),
            message,
            cookie: cookie.clone(),
            identities: resolve_identities(&identities),
        };
        let token = CancellationToken::new();
        self.live
            .lock_recover()
            .insert(cookie.clone(), token.clone());
        let prompter = SheetPrompter::new(Arc::clone(&self.sheets), &cookie);
        let first = initial_identity(&request.identities, session_user().as_deref());
        let helper = SystemHelper::default();
        let outcome = tokio::select! {
            outcome = authenticate(&helper, &prompter, &request, first) => outcome,
            () = token.cancelled() => Outcome::Cancelled,
        };
        self.sheets.cancel(prompter.handle());
        self.live.lock_recover().remove(&cookie);
        match outcome {
            Outcome::Authorized { identity } => {
                // Workspace history is derived from collaborative documents
                // and has no event feed to append to; the journal is the
                // durable record of an elevation.
                tracing::info!(
                    target: "shell::polkit",
                    action_id,
                    identity,
                    requester = details.get("polkit.caller-pid").map(String::as_str),
                    "administrator authentication granted"
                );
                Ok(())
            }
            Outcome::Cancelled => Err(PolkitError::Cancelled("dismissed".into())),
            Outcome::Failed(reason) => Err(PolkitError::Failed(reason)),
        }
    }

    /// polkit withdrew the request (the requester went away, or another
    /// agent answered).
    async fn cancel_authentication(&self, cookie: String) {
        let token = self.live.lock_recover().remove(&cookie);
        if let Some(token) = token {
            token.cancel();
        }
    }
}

/// Keeps the agent registered while held.
pub struct AgentHandle {
    connection: zbus::Connection,
}

impl AgentHandle {
    pub fn connection(&self) -> &zbus::Connection {
        &self.connection
    }
}

async fn session_id(system: &zbus::Connection) -> zbus::Result<String> {
    if let Some(id) = std::env::var("XDG_SESSION_ID")
        .ok()
        .filter(|id| !id.is_empty())
    {
        return Ok(id);
    }
    let login = zbus::Proxy::new(
        system,
        "org.freedesktop.login1",
        "/org/freedesktop/login1",
        "org.freedesktop.login1.Manager",
    )
    .await?;
    let path: zbus::zvariant::OwnedObjectPath = login
        .call("GetSessionByPID", &(std::process::id(),))
        .await?;
    let session = zbus::Proxy::new(
        system,
        "org.freedesktop.login1",
        path,
        "org.freedesktop.login1.Session",
    )
    .await?;
    session.get_property("Id").await
}

/// Serve the agent on the system bus and register it with polkit for this
/// login session.
///
/// # Errors
///
/// When the system bus, logind or polkit cannot be reached.
pub async fn serve(sheets: Arc<dyn Sheets>) -> zbus::Result<AgentHandle> {
    let connection = zbus::Connection::system().await?;
    connection
        .object_server()
        .at(AGENT_PATH, AuthenticationAgent::new(sheets))
        .await?;
    let session = session_id(&connection).await?;
    let authority = zbus::Proxy::new(
        &connection,
        "org.freedesktop.PolicyKit1",
        "/org/freedesktop/PolicyKit1/Authority",
        "org.freedesktop.PolicyKit1.Authority",
    )
    .await?;
    let mut subject_details: HashMap<&str, Value<'_>> = HashMap::new();
    subject_details.insert("session-id", Value::from(session.as_str()));
    let locale = std::env::var("LANG").unwrap_or_else(|_| "C".into());
    authority
        .call::<_, _, ()>(
            "RegisterAuthenticationAgent",
            &(
                ("unix-session", subject_details),
                locale.as_str(),
                AGENT_PATH,
            ),
        )
        .await?;
    Ok(AgentHandle { connection })
}
