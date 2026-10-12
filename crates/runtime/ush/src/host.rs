//! The Host target: the device's own login shell, unconfined.
//!
//! Everything else this runtime hands out is a guest. The Host target is the
//! one deliberate exception, and it is narrow on purpose: it is a pty running
//! the account's login shell (from the passwd database, never from a request)
//! in the supervisor's own session environment, with no namespace, filter or
//! Landlock ruleset. What makes it safe to offer is who may ask for it, and
//! that is decided by [`decide_host`] before any process exists:
//!
//! 1. **Only the Terminal app.** The launcher writes the app id onto a
//!    sandboxed app's service connection before the app holds it
//!    ([`read_caller`]); no other app id is ever answered. A connection
//!    without that preamble comes from a process outside every app sandbox —
//!    the shell hosting Terminal in-process — which already acts with the
//!    user's authority.
//! 2. **Never for an agent.** A session an agent asked for carries
//!    `requested_by: agent`; the Terminal forwards that, and the supervisor
//!    refuses it regardless of any allow.
//! 3. **Only for the device owner.** The person signed in to this session
//!    must be the person who owns the device ([`DeviceOwnership`]).
//! 4. **Only after the person allowed it once.** A `CapOverride` on
//!    `Scope::Device("host-shell")` for the Terminal's app id, in the same
//!    runtime's store ([`HostPolicy`], next to the environments' homes).

use std::io::Read;
use std::path::{Path, PathBuf};

use collab_types::caps::{
    Access, Actor, CapDecision, CapOverride, Principal, ResourceRef, Scope, Subject,
    decide_capability,
};

/// The device scope of the Host target: `Scope::Device("host-shell")`.
pub const HOST_SHELL_DEVICE: &str = "host-shell";

/// The person's decisions about the Host target on this device: plain
/// `CapOverride`s in the shared vocabulary, kept in the runtime's store
/// (`host-shell.json`). Missing, unreadable or malformed means nothing was
/// allowed.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct HostPolicy {
    #[serde(default)]
    pub capabilities: Vec<CapOverride>,
}

/// This login session's allows: `Expiry::EndOfSession` and `Expiry::AfterUse`
/// grants, which are never written to disk. They live in the supervisor,
/// which the user's service manager runs for the login session, so they are
/// gone at logout.
static SESSION_GRANTS: std::sync::Mutex<Vec<CapOverride>> = std::sync::Mutex::new(Vec::new());

impl HostPolicy {
    /// Every decision in force: those kept on disk and this session's.
    pub fn in_force(store: &crate::store::Store) -> Self {
        let mut policy = Self::load(&Self::path(store));
        if let Ok(session) = SESSION_GRANTS.lock() {
            policy.capabilities.extend(session.iter().cloned());
        }
        policy
    }

    /// Record the person's allow for `app_id` with its lifetime, from the
    /// shared vocabulary: "Always" (`Never`, or a deadline) on disk; "This
    /// session" (`EndOfSession`) and "Allow" (`AfterUse`) in this session
    /// only.
    ///
    /// # Errors
    /// Returns the write error for a decision kept on disk.
    pub fn record_allow(
        store: &crate::store::Store,
        app_id: &str,
        expiry: collab_types::caps::Expiry,
    ) -> std::io::Result<()> {
        let mut grant = Self::default();
        let id = grant.allow_host_shell(app_id);
        let Some(mut cap) = grant.capabilities.pop() else {
            return Ok(());
        };
        cap.expiry = expiry;
        if expiry.persists_across_restart() {
            let path = Self::path(store);
            let mut policy = Self::load(&path);
            policy.capabilities.retain(|c| c.id != id);
            policy.capabilities.push(cap);
            return policy.save(&path);
        }
        if let Ok(mut session) = SESSION_GRANTS.lock() {
            session.retain(|c| c.id != id);
            session.push(cap);
        }
        Ok(())
    }

    /// A one-time allow is spent by the session it let open.
    pub fn spend_one_time(app_id: &str) {
        let id = format!("ush-host-shell-{app_id}");
        if let Ok(mut session) = SESSION_GRANTS.lock() {
            session.retain(|c| !(c.id == id && c.expiry == collab_types::caps::Expiry::AfterUse));
        }
    }

    /// Where the decisions are kept for `store`.
    pub fn path(store: &crate::store::Store) -> PathBuf {
        store.root().join("host-shell.json")
    }

    /// Read the decisions. Never fails: anything unreadable allows nothing.
    pub fn load(path: &Path) -> Self {
        std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or_default()
    }

    /// Write the decisions that outlive a restart.
    ///
    /// # Errors
    /// Returns the write error.
    pub fn save(&self, path: &Path) -> std::io::Result<()> {
        let durable = Self {
            capabilities: self
                .capabilities
                .iter()
                .filter(|c| c.persists_across_restart())
                .cloned()
                .collect(),
        };
        tilo_files::native::atomic::write_creating_parent(
            path,
            &serde_json::to_vec_pretty(&durable).map_err(std::io::Error::other)?,
        )
    }

    /// Has the person allowed `app_id` to open the Host target? Asked of the
    /// same `decide_capability` every other decision goes through, so a
    /// `Restrict` takes it away again.
    pub fn allows_host_shell(&self, app_id: &str, now: Option<u64>) -> bool {
        decide_capability(
            &Actor::app(app_id),
            &ResourceRef::Device(HOST_SHELL_DEVICE.to_string()),
            Access::Write,
            &self.capabilities,
            now,
        ) == CapDecision::Allow
    }

    /// Record the person's allow for `app_id`; allowing twice rewrites one
    /// record. Returns its id.
    pub fn allow_host_shell(&mut self, app_id: &str) -> String {
        let id = format!("ush-host-shell-{app_id}");
        self.capabilities.retain(|c| c.id != id);
        self.capabilities.push(
            CapOverride::grant(
                Subject::Principal(Principal::App(app_id.to_string())),
                Scope::Device(HOST_SHELL_DEVICE.to_string()),
                Access::Write,
            )
            .with_id(id.clone())
            .granted_by(Principal::Person("local".to_string())),
        );
        id
    }
}

/// The one app that may open the Host target.
pub const TERMINAL_APP_ID: &str = "so.tilo.terminal";

/// The preamble `tilo-launcher` writes on a sandboxed app's `ush` connection
/// before handing the descriptor over.
pub const LAUNCH_MAGIC: [u8; 4] = *b"QUS1";
/// The handoff's JSON is bounded: it names one app id.
const MAX_HANDOFF_BYTES: u32 = 1024;

/// Who is on the other end of a service connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Caller {
    /// A sandboxed app, named by the launcher.
    App(String),
    /// A process outside every app sandbox (the shell hosting Terminal
    /// in-process). It already holds the user's authority.
    Session,
}

impl Caller {
    /// The app id the capability model asks about.
    pub fn app_id(&self) -> &str {
        match self {
            Caller::App(id) => id,
            Caller::Session => TERMINAL_APP_ID,
        }
    }
}

/// The launcher's handoff: `QUS1`, a little-endian `u32` length, then
/// `{"protocol":1,"app_id":"…"}`.
#[derive(Debug, serde::Deserialize)]
struct Handoff {
    protocol: u16,
    app_id: String,
}

/// Encode the launcher's handoff for `app_id`.
///
/// # Errors
///
/// Returns an error if the app id is too long to fit the bounded handoff.
pub fn encode_handoff(app_id: &str) -> std::io::Result<Vec<u8>> {
    let json = serde_json::to_vec(&serde_json::json!({ "protocol": 1, "app_id": app_id }))
        .map_err(std::io::Error::other)?;
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
///
/// # Errors
///
/// Returns an error if a handoff starts but is malformed or truncated.
pub fn read_caller(stream: &mut impl Read) -> std::io::Result<(Caller, Vec<u8>)> {
    // One byte first: a request is JSON and never starts with the magic's
    // first byte, so a short request (`{}`) is never waited on for more.
    let mut first = [0u8; 1];
    if stream.read(&mut first)? == 0 {
        return Ok((Caller::Session, Vec::new()));
    }
    if first != LAUNCH_MAGIC[..1] {
        return Ok((Caller::Session, first.to_vec()));
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
    Ok((Caller::App(handoff.app_id), Vec::new()))
}

/// Who asked for a session, as the Terminal reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum RequestedBy {
    /// The person, through the Terminal's own controls.
    #[default]
    Person,
    /// An agent, through the Terminal's published actions.
    Agent,
}

/// Whether the person signed in to this session owns the device.
pub trait DeviceOwnership {
    fn signed_in_person_owns_device(&self) -> bool;
}

/// The device owner and the signed-in person, from the files provisioning
/// and sign-in write: the owner's root key at
/// `$TILO_IDENTITY_ROOT|/var/lib/tilo` + `/user-identity/pubkey.bin`, and the
/// session's default identity in `$XDG_CONFIG_HOME/tilo/identity/registry.json`.
/// Anything missing or unreadable answers "not the owner".
#[derive(Debug, Clone)]
pub struct SessionOwnership {
    pub owner_key: PathBuf,
    pub registry: Option<PathBuf>,
}

impl SessionOwnership {
    pub fn discover() -> Self {
        let root = std::env::var_os("TILO_IDENTITY_ROOT")
            .filter(|value| !value.is_empty())
            .map_or_else(|| PathBuf::from("/var/lib/tilo"), PathBuf::from);
        Self {
            owner_key: root.join("user-identity/pubkey.bin"),
            registry: tilo_paths::xdg::config_home()
                .filter(|dir| dir.is_absolute())
                .map(|config| config.join("tilo/identity/registry.json")),
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
        struct Profile {
            user_public_key: [u8; 32],
        }
        #[derive(serde::Deserialize)]
        struct Registry {
            default_identity: Option<String>,
            #[serde(default)]
            identities: std::collections::BTreeMap<String, Profile>,
        }
        let bytes = std::fs::read(self.registry.as_ref()?).ok()?;
        let registry: Registry = serde_json::from_slice(&bytes).ok()?;
        let id = registry.default_identity?;
        registry.identities.get(&id).map(|p| p.user_public_key)
    }
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
    /// The line returned to the app.
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
///
/// # Errors
///
/// Returns the first gate the request fails, in the order the module
/// documentation lists them.
pub fn decide_host(
    caller: &Caller,
    requested_by: RequestedBy,
    ownership: &dyn DeviceOwnership,
    policy: &HostPolicy,
    now: Option<u64>,
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
    if !policy.allows_host_shell(TERMINAL_APP_ID, now) {
        return Err(HostDenied::NotAllowed);
    }
    Ok(())
}

/// The account a Host shell runs as, from the passwd database.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoginAccount {
    pub user: String,
    pub home: PathBuf,
    pub shell: PathBuf,
}

/// The real user's passwd entry.
pub fn login_account() -> Option<LoginAccount> {
    use std::ffi::CStr;
    // SAFETY: getuid has no preconditions.
    let uid = unsafe { libc::getuid() };
    let mut buf = vec![0 as libc::c_char; 16 * 1024];
    // SAFETY: zeroed passwd is a valid out-parameter for getpwuid_r.
    let mut pwd: libc::passwd = unsafe { std::mem::zeroed() };
    let mut result: *mut libc::passwd = std::ptr::null_mut();
    // SAFETY: every pointer refers to live storage of the stated size.
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

/// The command a Host session runs: the login shell as a login shell
/// (`argv[0]` prefixed with `-`), or the requested program run by it. With
/// `runs` (a key and the rc file [`crate::runs::bash_rc`] wrote) a `bash`
/// starts on that rc file instead, which reads the profile as a login shell
/// would and marks each command run.
pub fn host_command(
    account: &LoginAccount,
    term: &str,
    program: &[String],
    runs: Option<(&str, &Path)>,
) -> std::process::Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = std::process::Command::new(&account.shell);
    let name = account
        .shell
        .file_name()
        .map_or_else(|| "sh".into(), |n| n.to_string_lossy().into_owned());
    cmd.env_remove(crate::runs::KEY_ENV);
    if let (Some((key, rc)), true) = (runs, program.is_empty()) {
        cmd.arg("--rcfile").arg(rc).arg("-i");
        cmd.env(crate::runs::KEY_ENV, key);
    } else {
        cmd.arg0(format!("-{name}"));
    }
    if !program.is_empty() {
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

/// One `sh -c` script that runs `argv` exactly, each word single-quoted.
fn exec_script(argv: &[String]) -> String {
    let words: Vec<String> = argv
        .iter()
        .map(|word| format!("'{}'", word.replace('\'', "'\\''")))
        .collect();
    format!("exec {}", words.join(" "))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Owner(bool);
    impl DeviceOwnership for Owner {
        fn signed_in_person_owns_device(&self) -> bool {
            self.0
        }
    }

    fn allowed() -> HostPolicy {
        let mut policy = HostPolicy::default();
        policy.allow_host_shell(TERMINAL_APP_ID);
        policy
    }

    #[test]
    fn the_host_target_is_terminal_only_owner_only_person_only_and_allowed_once() {
        let terminal = Caller::App(TERMINAL_APP_ID.into());
        let now = Some(1_000);
        assert_eq!(
            decide_host(
                &terminal,
                RequestedBy::Person,
                &Owner(true),
                &allowed(),
                now
            ),
            Ok(())
        );
        assert_eq!(
            decide_host(
                &Caller::Session,
                RequestedBy::Person,
                &Owner(true),
                &allowed(),
                now
            ),
            Ok(()),
            "the shell hosting Terminal in-process is the Terminal"
        );
        assert_eq!(
            decide_host(
                &Caller::App("so.tilo.notes".into()),
                RequestedBy::Person,
                &Owner(true),
                &allowed(),
                now
            ),
            Err(HostDenied::NotTerminal("so.tilo.notes".into()))
        );
        assert_eq!(
            decide_host(&terminal, RequestedBy::Agent, &Owner(true), &allowed(), now),
            Err(HostDenied::Agent)
        );
        assert_eq!(
            decide_host(
                &terminal,
                RequestedBy::Person,
                &Owner(false),
                &allowed(),
                now
            ),
            Err(HostDenied::NotOwner)
        );
        assert_eq!(
            decide_host(
                &terminal,
                RequestedBy::Person,
                &Owner(true),
                &HostPolicy::default(),
                now
            ),
            Err(HostDenied::NotAllowed)
        );

        // An allow for another app is not an allow for Terminal, and a
        // restriction from the permissions panel takes Terminal's away.
        let mut other = HostPolicy::default();
        other.allow_host_shell("so.tilo.notes");
        assert!(!other.allows_host_shell(TERMINAL_APP_ID, now));
        let mut restricted = allowed();
        restricted
            .capabilities
            .push(collab_types::caps::CapOverride::new(
                collab_types::caps::Subject::Principal(collab_types::caps::Principal::App(
                    TERMINAL_APP_ID.into(),
                )),
                collab_types::caps::Scope::Device(HOST_SHELL_DEVICE.into()),
                collab_types::caps::Effect::Restrict,
                collab_types::caps::Access::Read,
            ));
        assert!(!restricted.allows_host_shell(TERMINAL_APP_ID, now));

        // Allowing twice keeps one record.
        let mut twice = allowed();
        twice.allow_host_shell(TERMINAL_APP_ID);
        assert_eq!(twice.capabilities.len(), 1);
    }

    #[test]
    fn the_launcher_handoff_names_the_app_and_a_bare_request_is_the_session() {
        let mut wire = encode_handoff("so.tilo.terminal").unwrap();
        wire.extend_from_slice(b"{\"op\":\"open-session\"}\n");
        let mut reader = std::io::Cursor::new(wire);
        let (caller, carried) = read_caller(&mut reader).unwrap();
        assert_eq!(caller, Caller::App("so.tilo.terminal".into()));
        assert!(carried.is_empty());
        let mut rest = String::new();
        reader.read_to_string(&mut rest).unwrap();
        assert!(rest.starts_with("{\"op\""));

        let mut bare = std::io::Cursor::new(b"{\"op\":\"open-session\"}\n".to_vec());
        let (caller, carried) = read_caller(&mut bare).unwrap();
        assert_eq!(caller, Caller::Session);
        assert_eq!(carried, b"{");

        let mut forged = LAUNCH_MAGIC.to_vec();
        forged.extend_from_slice(&u32::MAX.to_le_bytes());
        assert!(read_caller(&mut std::io::Cursor::new(forged)).is_err());
    }

    #[test]
    fn the_owner_check_compares_the_device_claim_with_the_signed_in_identity() {
        let dir = tempfile::tempdir().unwrap();
        let ownership = SessionOwnership {
            owner_key: dir.path().join("pubkey.bin"),
            registry: Some(dir.path().join("registry.json")),
        };
        assert!(!ownership.signed_in_person_owns_device(), "nothing claimed");
        std::fs::write(&ownership.owner_key, [7u8; 32]).unwrap();
        assert!(
            !ownership.signed_in_person_owns_device(),
            "nobody signed in"
        );
        let registry = |key: u8| {
            serde_json::json!({
                "schema_version": 1,
                "default_identity": "me",
                "identities": {"me": {"id": "me", "display_name": "Me",
                    "record_path": "/x", "user_public_key": vec![key; 32]}},
                "workspace_identities": {}
            })
            .to_string()
        };
        std::fs::write(ownership.registry.as_ref().unwrap(), registry(7)).unwrap();
        assert!(ownership.signed_in_person_owns_device());
        std::fs::write(ownership.registry.as_ref().unwrap(), registry(8)).unwrap();
        assert!(
            !ownership.signed_in_person_owns_device(),
            "another person signed in to the owner's device"
        );
    }

    #[test]
    fn a_host_command_is_the_passwd_login_shell_in_the_home() {
        let dir = tempfile::tempdir().unwrap();
        let account = LoginAccount {
            user: "you".into(),
            home: dir.path().to_path_buf(),
            shell: PathBuf::from("/usr/bin/fish"),
        };
        let cmd = host_command(&account, "xterm-256color", &[], None);
        assert_eq!(cmd.get_program(), "/usr/bin/fish");
        assert_eq!(cmd.get_args().count(), 0);
        assert_eq!(cmd.get_current_dir(), Some(dir.path()));
        let home = (std::ffi::OsStr::new("HOME"), Some(dir.path().as_os_str()));
        assert!(cmd.get_envs().any(|pair| pair == home));

        let run = host_command(&account, "xterm", &["htop".into(), "-d".into()], None);
        let args: Vec<_> = run.get_args().collect();
        assert_eq!(args, ["-c", "exec 'htop' '-d'"]);
    }
}
