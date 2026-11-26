//! The Host target: the device's own login shell, unconfined.
//!
//! The one deliberate exception, and narrow on purpose: a pty running the account's login
//! shell (from the passwd database, never from a request) in the supervisor's own
//! environment, with no namespace, filter or Landlock ruleset. [`decide_host`] decides
//! who may ask before any process exists:
//!
//! 1. only the Terminal app, whose id the launcher writes onto its sandboxed service
//!    connection ([`read_caller`]); a connection without that preamble comes from a
//!    process outside every app sandbox, which already acts with the user's authority;
//! 2. never for an agent: a session asked for by an agent carries `requested_by: agent`,
//!    which the Terminal forwards and the supervisor refuses regardless of any allow;
//! 3. only for the device owner ([`DeviceOwnership`]);
//! 4. only while the person allows it: their answer for the host shell is one of their device
//!    answers, which the session keeps with the others and hands here ([`hand_allow`]).

use std::io::Read;
use std::path::{Path, PathBuf};

use tilo_action_policy::Lifetime;

/// How long the person allows the Host target on this device, as the session last handed it. Their
/// answer lives in their own store with their other device answers; nothing of it is written here,
/// and it is gone with the supervisor.
static ALLOWED: std::sync::Mutex<Option<Lifetime>> = std::sync::Mutex::new(None);

fn allowed() -> std::sync::MutexGuard<'static, Option<Lifetime>> {
    ALLOWED
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// Take the person's answer the session handed: allowed for `lifetime`, or not at all.
pub fn hand_allow(lifetime: Option<Lifetime>) {
    *allowed() = lifetime;
}

/// Whether the person allows the Host target at `now_ms`.
pub fn host_allowed(now_ms: u64) -> bool {
    allowed().is_some_and(|lifetime| lifetime.is_live_at(Some(now_ms)))
}

/// A one-time allow is spent by the shell it opened.
pub fn spend_one_time() {
    let mut allowed = allowed();
    if *allowed == Some(Lifetime::Once) {
        *allowed = None;
    }
}

pub use tilo_app_id::apps::TERMINAL as TERMINAL_APP_ID;

pub const LAUNCH_MAGIC: [u8; 4] = *b"QUS1";
const MAX_HANDOFF_BYTES: u32 = 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    App(String),
    /// A process outside every app sandbox (the shell hosting Terminal
    /// in-process). It already holds the user's authority.
    Session,
}

impl Caller {
    pub fn app_id(&self) -> &str {
        match self {
            Caller::App(id) => id,
            Caller::Session => TERMINAL_APP_ID,
        }
    }
}

#[derive(Debug, serde::Deserialize)]
struct Handoff {
    protocol: u16,
    app_id: String,
    #[serde(default)]
    workspace: Option<String>,
}

pub fn encode_handoff(app_id: &str) -> std::io::Result<Vec<u8>> {
    encode_launch_handoff(app_id, None)
}

pub fn encode_launch_handoff(app_id: &str, workspace: Option<&str>) -> std::io::Result<Vec<u8>> {
    let mut handoff = serde_json::json!({ "protocol": 1, "app_id": app_id });
    if let (Some(workspace), Some(fields)) = (workspace, handoff.as_object_mut()) {
        fields.insert("workspace".into(), workspace.into());
    }
    let json = serde_json::to_vec(&handoff).map_err(std::io::Error::other)?;
    let len = u32::try_from(json.len())
        .ok()
        .filter(|len| *len > 0 && *len <= MAX_HANDOFF_BYTES)
        .ok_or_else(|| std::io::Error::other("ush handoff is out of bounds"))?;
    let mut out = Vec::with_capacity(json.len().saturating_add(8));
    out.extend_from_slice(&LAUNCH_MAGIC);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&json);
    Ok(out)
}

/// Read who is calling from the head of a connection. Returns the caller and
/// the bytes already consumed that belong to the request (a connection
/// without a handoff starts directly with its JSON line).
pub fn read_caller(stream: &mut impl Read) -> std::io::Result<(Caller, Vec<u8>)> {
    read_handoff(stream).map(|(caller, _, carried)| (caller, carried))
}

pub fn read_handoff(stream: &mut impl Read) -> std::io::Result<(Caller, Option<String>, Vec<u8>)> {
    // One byte first: a request is JSON and never starts with the magic's
    // first byte, so a short request (`{}`) is never waited on for more.
    let mut first = [0u8; 1];
    if stream.read(&mut first)? == 0 {
        return Ok((Caller::Session, None, Vec::new()));
    }
    if first != LAUNCH_MAGIC[..1] {
        return Ok((Caller::Session, None, first.to_vec()));
    }
    let mut rest = [0u8; 3];
    stream.read_exact(&mut rest)?;
    if rest != LAUNCH_MAGIC[1..] {
        return Err(std::io::Error::other("ush handoff is not understood"));
    }
    let mut len = [0u8; 4];
    stream.read_exact(&mut len)?;
    let len = u32::from_le_bytes(len);
    if len == 0 || len > MAX_HANDOFF_BYTES {
        return Err(std::io::Error::other("ush handoff is out of bounds"));
    }
    let mut json = vec![0u8; len as usize];
    stream.read_exact(&mut json)?;
    let handoff: Handoff = serde_json::from_slice(&json).map_err(std::io::Error::other)?;
    if handoff.protocol != 1 || handoff.app_id.is_empty() {
        return Err(std::io::Error::other("ush handoff is not understood"));
    }
    let workspace = handoff.workspace.filter(|workspace| !workspace.is_empty());
    Ok((Caller::App(handoff.app_id), workspace, Vec::new()))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestedBy {
    #[default]
    Person,
    Agent,
}

pub trait DeviceOwnership {
    fn signed_in_person_owns_device(&self) -> bool;
}

/// The device owner and the signed-in person, from the files provisioning
/// and sign-in write: the owner's root key at
/// `$TILO_IDENTITY_ROOT|/var/lib/tilo` + `/user-identity/pubkey.bin`, and the
/// session's active identity record in `$XDG_CONFIG_HOME/tilo/identity`.
/// Anything missing or unreadable answers "not the owner".
#[derive(Debug, Clone)]
pub struct SessionOwnership {
    pub owner_key: PathBuf,
    pub identity_dir: Option<PathBuf>,
}

impl SessionOwnership {
    pub fn discover() -> Self {
        let root = std::env::var_os("TILO_IDENTITY_ROOT")
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("/var/lib/tilo"), PathBuf::from);
        Self {
            owner_key: root.join("user-identity/pubkey.bin"),
            identity_dir: tilo_paths::xdg::config_home()
                .filter(|dir| dir.is_absolute())
                .map(|config| config.join("tilo/identity")),
        }
    }

    fn owner(&self) -> Option<[u8; 32]> {
        let meta = std::fs::symlink_metadata(&self.owner_key).ok()?;
        if !meta.file_type().is_file() || meta.len() != 32 {
            return None;
        }
        std::fs::read(&self.owner_key).ok()?.try_into().ok()
    }

    fn signed_in(&self) -> Option<[u8; 32]> {
        #[derive(serde::Deserialize)]
        struct Threshold {
            group_public_key: [u8; 32],
        }
        #[derive(serde::Deserialize)]
        struct Record {
            threshold_identity: Threshold,
        }
        let dir = self.identity_dir.as_ref()?;
        let id = std::fs::read_to_string(dir.join("active"))
            .ok()
            .map(|selector| selector.trim().to_owned())
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 64
                    && id.bytes().all(|byte| {
                        byte.is_ascii_lowercase()
                            || byte.is_ascii_digit()
                            || matches!(byte, b'_' | b'-')
                    })
            })
            .unwrap_or_else(|| "primary".to_owned());
        let bytes = std::fs::read(dir.join(format!("{id}.json"))).ok()?;
        let record: Record = serde_json::from_slice(&bytes).ok()?;
        Some(record.threshold_identity.group_public_key)
    }
}

pub fn signed_in_person() -> String {
    SessionOwnership::discover()
        .signed_in()
        .map_or_else(|| "local".to_owned(), hex::encode)
}

/// This device, as an environment's file actions name the device that
/// carries them out: the machine id, which stays the same across sessions
/// and sign-ins and never leaves the device as anything but a name.
pub fn this_device() -> String {
    std::fs::read_to_string("/etc/machine-id")
        .ok()
        .map(|id| id.trim().to_owned())
        .filter(|id| !id.is_empty())
        .unwrap_or_else(|| "this-device".to_owned())
}

impl DeviceOwnership for SessionOwnership {
    fn signed_in_person_owns_device(&self) -> bool {
        match (self.owner(), self.signed_in()) {
            (Some(owner), Some(me)) => owner == me,
            _ => false,
        }
    }
}

/// Why a Host session was refused.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HostDenied {
    /// Only the Terminal app may open the Host target.
    NotTerminal(String),
    /// Agents never open the Host target.
    Agent,
    /// The signed-in person is not the device's owner.
    NotOwner,
    /// The person has not allowed the Host target on this device yet.
    NotAllowed,
}

impl HostDenied {
    pub fn reason(&self) -> String {
        match self {
            HostDenied::NotTerminal(app) => {
                format!("`{app}` may not open the device's own shell; only Terminal can")
            }
            HostDenied::Agent => "agents may not open the device's own shell".into(),
            HostDenied::NotOwner => {
                "only the device's owner may open the device's own shell".into()
            }
            HostDenied::NotAllowed => {
                "the device's own shell has not been allowed on this device".into()
            }
        }
    }
}

/// Decide a Host request. Pure, so every refusal is testable without a pty.
pub fn decide_host(
    caller: &Caller,
    requested_by: RequestedBy,
    ownership: &dyn DeviceOwnership,
    allowed: bool,
) -> Result<(), HostDenied> {
    if caller.app_id() != TERMINAL_APP_ID {
        return Err(HostDenied::NotTerminal(caller.app_id().to_string()));
    }
    if requested_by == RequestedBy::Agent {
        return Err(HostDenied::Agent);
    }
    if !ownership.signed_in_person_owns_device() {
        return Err(HostDenied::NotOwner);
    }
    if !allowed {
        return Err(HostDenied::NotAllowed);
    }
    Ok(())
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginAccount {
    pub user: String,
    pub home: PathBuf,
    pub shell: PathBuf,
}

pub fn login_account() -> Option<LoginAccount> {
    use std::ffi::CStr;
    let uid = rustix::process::getuid().as_raw();
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: zeroed passwd is a valid out-parameter for getpwuid_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    let rc = unsafe {
        libc::getpwuid_r(
            uid,
            &raw mut pwd,
            buf.as_mut_ptr(),
            buf.len(),
            &raw mut result,
        )
    };
    if rc != 0 || result.is_null() || pwd.pw_dir.is_null() || pwd.pw_name.is_null() {
        return None;
    }
    // SAFETY: getpwuid_r succeeded, so the strings live in `buf`.
    let (user, home, shell) = unsafe {
        (
            CStr::from_ptr(pwd.pw_name).to_string_lossy().into_owned(),
            CStr::from_ptr(pwd.pw_dir).to_string_lossy().into_owned(),
            (!pwd.pw_shell.is_null())
                .then(|| CStr::from_ptr(pwd.pw_shell).to_string_lossy().into_owned()),
        )
    };
    Some(LoginAccount {
        user,
        home: PathBuf::from(home),
        shell: PathBuf::from(
            shell
                .filter(|s| !s.trim().is_empty())
                .unwrap_or_else(|| "/bin/sh".into()),
        ),
    })
}

pub fn host_command(
    account: &LoginAccount,
    term: &str,
    program: &[String],
    rc: Option<&Path>,
    commands: Option<&str>,
) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(&account.shell);
    cmd.env_remove(crate::command_marks::KEY_ENV);
    if program.is_empty() {
        cmd.args(crate::shell_start::interactive_args(&account.shell, rc));
        if let Some(key) = commands {
            cmd.env(crate::command_marks::KEY_ENV, key);
        }
    } else {
        let name = account
            .shell
            .file_name()
            .map_or_else(|| "sh".into(), |n| n.to_string_lossy().into_owned());
        cmd.arg0(format!("-{name}"));
        cmd.arg("-c");
        cmd.arg(exec_script(program));
    }
    cmd.current_dir(existing_dir(&account.home));
    for key in ["LISTEN_PID", "LISTEN_FDS", "LISTEN_FDNAMES", "TILO_USH_BIN"] {
        cmd.env_remove(key);
    }
    cmd.env(crate::store::STORE_ENV, "");
    cmd.env_remove(crate::store::STORE_ENV);
    cmd.env("HOME", &account.home);
    cmd.env("SHELL", &account.shell);
    cmd.env("USER", &account.user);
    cmd.env("LOGNAME", &account.user);
    cmd.env("TERM", term);
    cmd
}

fn existing_dir(home: &Path) -> PathBuf {
    if home.is_dir() {
        home.to_path_buf()
    } else {
        PathBuf::from("/")
    }
}

fn exec_script(argv: &[String]) -> String {
    let words: Vec<String> = argv
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect();
    format!("exec {}", words.join(" "))
}
