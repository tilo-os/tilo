//! The device admin's control socket.
//!
//! The semantic half of the control protocol — `actions_enumerate`,
//! `actions_invoke`, `actions_status`, `actions_cancel`, `state_dump` and
//! `foreign_frames` — served at `$XDG_RUNTIME_DIR/tilo-session/admin-control.sock`
//! in the device admin's own session, by every build, once first login is
//! complete. It is how the admin, and an agent the admin runs over their own
//! login or ssh, drives the desktop through the same typed actions the
//! palette, assistive tools and collaborators use. It changes nothing about
//! the session it serves: first-use onboarding, the lock screen and the
//! capability gate behave exactly as they do with no client attached.
//!
//! Who it is served to is decided twice.
//!
//! **The session.** Only the device admin's session binds it: an account in
//! the machine's administrator group whose identity is the one that claimed
//! the device at its first login and holds its primary certificate
//! ([`tilo_identity_host::provisioning::active_identity_owns_device`]).
//! Before first use finishes, the same socket may serve only onboarding and
//! the existing authentication queue to the exact target account of a root
//! authenticated prepared delivery. This is not an absent-owner shortcut;
//! the delivery binds the account, Home inode, ciphertext and intended person.
//! Every request rechecks that binding and promotion requires certified ownership.
//!
//! **The peer.** Every connection is checked against the kernel's account of
//! the process on the other end (`SO_PEERCRED`): it must run as this session's
//! own user, in this session's user and mount namespaces (a sandboxed app has
//! its own, and is never shown the path anyway), and must not be an app this
//! session launched, confined or not. An app acts through its own
//! collaboration connection, with its own identity and grants, never with the
//! admin's.
//!
//! Every invocation then passes the same capability gate as the palette
//! (`PolicyVisibility`), so the socket reaches exactly what the admin may
//! do at the keyboard. While locked, ordinary actions remain refused; the
//! sole local owner action submits a private credential to the same PAM
//! verification worker as the lock screen. Acceptance never clears the lock.

use std::ffi::CStr;
use std::io::{self, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::{Path, PathBuf};

use crate::shell::test_control_socket::{admin_socket_path, bind_private};

/// The group whose members administer the machine: `sudo` and the polkit
/// rules shipped with the image both name it.
const ADMIN_GROUP: &CStr = c"wheel";
mod prepared;

/// Why this session serves no admin control socket.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum NotServed {
    /// `XDG_RUNTIME_DIR` is unset or relative, so there is no private place
    /// to put the socket.
    NoRuntimeDir,
    /// The account is not in the administrator group.
    NotAdministrator,
    /// The account's identity does not own the device, or it has none yet.
    NotDeviceOwner,
    /// The device's ownership could not be read; reported, never guessed.
    OwnershipUnreadable(String),
    /// Another live session already answers on the socket.
    InUse(PathBuf),
    /// Binding failed.
    Bind(String),
}

impl std::fmt::Display for NotServed {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NoRuntimeDir => f.write_str("XDG_RUNTIME_DIR is unset or relative"),
            Self::NotAdministrator => write!(
                f,
                "this account is not in the `{}` group",
                ADMIN_GROUP.to_string_lossy()
            ),
            Self::NotDeviceOwner => f.write_str("this account's identity does not own the device"),
            Self::OwnershipUnreadable(error) => {
                write!(f, "the device's ownership could not be read: {error}")
            }
            Self::InUse(path) => write!(f, "{} already has a live listener", path.display()),
            Self::Bind(error) => write!(f, "bind failed: {error}"),
        }
    }
}

/// Decide whether a session serves admin control from the two facts that
/// make its account the device admin.
pub(crate) fn session_decision(
    in_admin_group: bool,
    owns_device: io::Result<bool>,
) -> Result<(), NotServed> {
    if !in_admin_group {
        return Err(NotServed::NotAdministrator);
    }
    match owns_device {
        Ok(true) => Ok(()),
        Ok(false) => Err(NotServed::NotDeviceOwner),
        Err(error) => Err(NotServed::OwnershipUnreadable(error.to_string())),
    }
}

/// Recheck certified device administration for a trusted authentication prompt.
/// Missing, locked or unreadable identity custody is never first-owner consent.
pub(crate) fn authentication_session_decision(identity_dir: &Path) -> Result<(), NotServed> {
    session_decision(
        in_admin_group(),
        tilo_identity_host::provisioning::active_identity_owns_device(
            identity_dir,
            &tilo_identity_host::provisioning::collaboration_identity_paths(),
        ),
    )
}

/// Revalidate the current control phase. Prepared access is only the actual
/// root installation's delivered target; it never turns absence into ownership.
pub(crate) fn control_session_decision(
    identity_dir: &Path,
    first_use: bool,
) -> Result<(), NotServed> {
    match authentication_session_decision(identity_dir) {
        Ok(()) => Ok(()),
        Err(NotServed::NotDeviceOwner) if first_use && in_admin_group() => {
            prepared::check(identity_dir)
                .map_err(|error| NotServed::OwnershipUnreadable(error.to_string()))
        }
        Err(error) => Err(error),
    }
}

pub(crate) fn bind_first_use(
    identity_dir: &Path,
    headless: bool,
) -> Result<(UnixListener, PathBuf), NotServed> {
    control_session_decision(identity_dir, true)?;
    let path =
        admin_socket_path(std::env::var_os("XDG_RUNTIME_DIR")).ok_or(NotServed::NoRuntimeDir)?;
    let path = if headless {
        path.with_file_name("headless-owner-control.sock")
    } else {
        path
    };
    bind_listener(path)
}

/// Bind the admin control socket for this session, or say why not.
///
/// `identity_dir` is the session's identity directory. The socket is
/// private (0700 directory, 0600 socket) and replaces only a stale socket: a
/// path another session still answers on is left alone.
pub(crate) fn bind(identity_dir: &Path) -> Result<(UnixListener, PathBuf), NotServed> {
    let path =
        admin_socket_path(std::env::var_os("XDG_RUNTIME_DIR")).ok_or(NotServed::NoRuntimeDir)?;
    bind_at(identity_dir, path)
}

/// An explicitly requested headless session serves the same owner-only
/// semantic protocol at a separate socket. It cannot replace the real seat's
/// listener, and it retains every ownership and caller check of that seat.
pub(crate) fn bind_headless_owner(
    identity_dir: &Path,
) -> Result<(UnixListener, PathBuf), NotServed> {
    let path = admin_socket_path(std::env::var_os("XDG_RUNTIME_DIR"))
        .ok_or(NotServed::NoRuntimeDir)?
        .with_file_name("headless-owner-control.sock");
    bind_at(identity_dir, path)
}

fn bind_at(identity_dir: &Path, path: PathBuf) -> Result<(UnixListener, PathBuf), NotServed> {
    authentication_session_decision(identity_dir)?;
    bind_listener(path)
}

fn bind_listener(path: PathBuf) -> Result<(UnixListener, PathBuf), NotServed> {
    if path.exists() && UnixStream::connect(&path).is_ok() {
        return Err(NotServed::InUse(path));
    }
    let listener = bind_private(&path).map_err(|error| NotServed::Bind(error.to_string()))?;
    listener
        .set_nonblocking(true)
        .map_err(|error| NotServed::Bind(error.to_string()))?;
    Ok((listener, path))
}

/// What the kernel says about the process on the other end of a connection.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Peer {
    pub uid: libc::uid_t,
    /// Whether it shares this session's user and mount namespaces.
    pub shares_namespaces: bool,
    /// The package of the app this session launched that it descends from.
    pub launched_app: Option<String>,
}

/// Why a connection was refused.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PeerRefusal {
    /// No peer credentials: nothing to decide on.
    Unidentified,
    OtherAccount,
    Sandboxed,
    LaunchedApp(String),
}

impl PeerRefusal {
    /// The message the refused client reads: what it is not, and the route
    /// that would work.
    pub(crate) fn message(&self) -> String {
        match self {
            Self::Unidentified => "this program couldn't be identified".to_string(),
            Self::OtherAccount => "this works only for the account whose desktop this is; \
                 sign in as that account"
                .to_string(),
            // An app's process — the Terminal app and everything run in it —
            // reaches the desktop's actions with that app's workspace scope,
            // through `tilo action`, never with the owner's whole authority.
            Self::Sandboxed | Self::LaunchedApp(_) => {
                "programs inside an app can't use tilo-control; use `tilo action` instead"
                    .to_string()
            }
        }
    }
}

/// Decide one connection from its peer's facts.
pub(crate) fn peer_decision(peer: &Peer, own_uid: libc::uid_t) -> Result<(), PeerRefusal> {
    if peer.uid != own_uid {
        return Err(PeerRefusal::OtherAccount);
    }
    if !peer.shares_namespaces {
        return Err(PeerRefusal::Sandboxed);
    }
    if let Some(package) = &peer.launched_app {
        return Err(PeerRefusal::LaunchedApp(package.clone()));
    }
    Ok(())
}

/// Authorise an accepted connection from the kernel's account of its peer.
pub(crate) fn authorize(stream: &UnixStream) -> Result<(), PeerRefusal> {
    let credentials =
        super::native::wayland_peer_credentials(stream).ok_or(PeerRefusal::Unidentified)?;
    let pid = u32::try_from(credentials.pid).map_err(|_| PeerRefusal::Unidentified)?;
    let peer = Peer {
        uid: credentials.uid,
        shares_namespaces: super::native::peer_shares_host_security_namespaces(pid),
        launched_app: crate::shell::launcher::verified_package_for_descendant(pid),
    };
    peer_decision(&peer, rustix::process::geteuid().as_raw())
}

/// Tell a refused client why, in the protocol's own error shape, then let
/// the connection drop.
pub(crate) fn refuse(mut stream: UnixStream, refusal: &PeerRefusal) {
    if let PeerRefusal::LaunchedApp(package) = refusal {
        tracing::info!(%package, "admin control: a process inside an app was refused");
    }
    let reply = crate::shell::test_control::serialise_response(
        &crate::shell::test_control::Response::Err {
            code: "not_permitted".to_string(),
            message: refusal.message(),
        },
    );
    let _ = stream.write_all(format!("{reply}\n").as_bytes());
}

/// Whether this process runs with the administrator group among its groups.
/// The kernel's group list is what PAM set up at login; it is not something
/// the session can claim for itself, and it does not change while the
/// process lives, so it is resolved once.
///
/// Resolving it reads the group database through NSS, which loads its
/// modules and reads `/etc`: a wait on the disk the compositor's main loop
/// must not make (the palette's actions ask while it is open).
/// [`resolve_admin_group_in_background`] answers it on a startup thread; a call before that
/// finishes waits for its answer instead of asking again.
fn in_admin_group() -> bool {
    *IN_ADMIN_GROUP.get_or_init(admin_group_now)
}

static IN_ADMIN_GROUP: std::sync::OnceLock<bool> = std::sync::OnceLock::new();

/// Resolve [`in_admin_group`] on a thread of its own, started at startup so
/// the main loop finds the answer.
pub(crate) fn resolve_admin_group_in_background() {
    let _ = std::thread::Builder::new()
        .name("tilo-admin-group".into())
        .spawn(|| {
            in_admin_group();
        });
}

fn admin_group_now() -> bool {
    let Some(admin) = group_id(ADMIN_GROUP) else {
        return false;
    };
    process_groups().contains(&admin)
}

fn group_id(name: &CStr) -> Option<libc::gid_t> {
    // SAFETY: an all-zero `group` is a valid out-parameter for getgrnam_r,
    // which fills it in or leaves `result` null.
    let mut group: libc::group = unsafe { std::mem::zeroed() };
    let mut buffer = vec![0 as libc::c_char; 16 * 1024];
    let mut result: *mut libc::group = std::ptr::null_mut();
    // SAFETY: every pointer names live storage of the advertised length, and
    // `name` is NUL-terminated.
    let status = unsafe {
        libc::getgrnam_r(
            name.as_ptr(),
            &raw mut group,
            buffer.as_mut_ptr(),
            buffer.len(),
            &raw mut result,
        )
    };
    (status == 0 && !result.is_null()).then_some(group.gr_gid)
}

fn process_groups() -> Vec<libc::gid_t> {
    // SAFETY: a zero-sized query writes nothing and returns the count.
    let count = unsafe { libc::getgroups(0, std::ptr::null_mut()) };
    let mut groups = vec![0; usize::try_from(count).unwrap_or(0)];
    // SAFETY: `groups` holds exactly `count` entries.
    let written = unsafe { libc::getgroups(count, groups.as_mut_ptr()) };
    groups.truncate(usize::try_from(written).unwrap_or(0));
    groups.push(rustix::process::getegid().as_raw());
    groups
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(uid: libc::uid_t, shares_namespaces: bool, launched_app: Option<&str>) -> Peer {
        Peer {
            uid,
            shares_namespaces,
            launched_app: launched_app.map(str::to_string),
        }
    }

    #[test]
    fn only_the_device_admins_session_serves_admin_control() {
        assert_eq!(session_decision(true, Ok(true)), Ok(()));
        assert_eq!(
            session_decision(false, Ok(true)),
            Err(NotServed::NotAdministrator),
            "owning the identity is not enough without the administrator group"
        );
        assert_eq!(
            session_decision(true, Ok(false)),
            Err(NotServed::NotDeviceOwner),
            "another administrator account on a shared device does not own it"
        );
        assert!(matches!(
            session_decision(true, Err(io::Error::other("unreadable"))),
            Err(NotServed::OwnershipUnreadable(_))
        ));
    }

    #[test]
    fn only_the_admins_own_unconfined_processes_connect() {
        const ADMIN: libc::uid_t = 1000;
        assert_eq!(peer_decision(&peer(ADMIN, true, None), ADMIN), Ok(()));
        assert_eq!(
            peer_decision(&peer(1001, true, None), ADMIN),
            Err(PeerRefusal::OtherAccount),
            "another user"
        );
        assert_eq!(
            peer_decision(&peer(0, true, None), ADMIN),
            Err(PeerRefusal::OtherAccount),
            "root acts as itself, not as the admin's session"
        );
        assert_eq!(
            peer_decision(&peer(ADMIN, false, None), ADMIN),
            Err(PeerRefusal::Sandboxed),
            "a sandboxed app runs in its own namespaces"
        );
        assert_eq!(
            peer_decision(&peer(ADMIN, true, Some("so.tilo.notes")), ADMIN),
            Err(PeerRefusal::LaunchedApp("so.tilo.notes".into())),
            "an app the session launched, even unconfined"
        );
    }

    /// The live check over a real socket: this test process is its own
    /// account, in its own namespaces, and no session launched it.
    #[test]
    fn a_connection_from_this_account_is_authorised_by_its_credentials() {
        let (client, server) = UnixStream::pair().expect("socket pair");
        assert_eq!(authorize(&server), Ok(()));
        drop(client);
    }

    #[test]
    fn a_refused_client_reads_why_in_the_protocol_shape() {
        let (mut client, server) = UnixStream::pair().expect("socket pair");
        refuse(server, &PeerRefusal::Sandboxed);
        let mut line = String::new();
        std::io::Read::read_to_string(&mut client, &mut line).expect("read refusal");
        let reply: serde_json::Value = serde_json::from_str(line.trim()).expect("one JSON line");
        assert_eq!(reply["err"]["code"], "not_permitted");
        // Why, in plain words, and what works instead.
        assert!(
            reply["err"]["message"]
                .as_str()
                .is_some_and(
                    |message| message.contains("inside an app") && message.contains("tilo action")
                ),
            "{reply}"
        );
    }
}

/// The actual first-use resource surface; publication alone never exposes a
/// different provider through the bootstrap control phase.
pub(crate) struct FirstUseVisibility<'a> {
    pub base: &'a dyn crate::shell::search::action_api::ActionVisibility,
    pub first_use: bool,
}
impl crate::shell::search::action_api::ActionVisibility for FirstUseVisibility<'_> {
    fn can_see(
        &self,
        resource: &crate::shell::search::action_api::ResourceRef,
        kind: &crate::shell::search::ResourceKind,
    ) -> bool {
        (!self.first_use || first_use_provider(&resource.provider))
            && self.base.can_see(resource, kind)
    }
}
fn first_use_provider(provider: &str) -> bool {
    matches!(provider, "onboarding" | "authentication-prompts")
}
pub(crate) fn first_use_permitted(command: &crate::shell::test_control::Command) -> bool {
    use crate::shell::test_control::Command;
    match command {
        Command::ActionsEnumerate { .. } => true,
        Command::ActionsInvoke {
            provider,
            device: None,
            ..
        } => first_use_provider(provider),
        _ => false,
    }
}
#[cfg(test)]
mod first_use_tests {
    use super::*;
    use crate::shell::{
        search::action_api::{ActionVisibility, AllVisible, ResourceRef},
        test_control::Command,
    };
    #[test]
    fn first_use_scope_refuses_unrelated_published_actions_and_remote_devices() {
        let gate = FirstUseVisibility {
            base: &AllVisible,
            first_use: true,
        };
        let kind = crate::shell::search::ResourceKind::Action;
        assert!(gate.can_see(&ResourceRef::new("onboarding", "inspect"), &kind));
        assert!(gate.can_see(
            &ResourceRef::new("authentication-prompts", "answer@ticket"),
            &kind
        ));
        for provider in ["app-actions", "session", "permissions", "host-applications"] {
            assert!(!gate.can_see(&ResourceRef::new(provider, "inspect"), &kind));
        }
        assert!(!first_use_permitted(&Command::Windows));
        assert!(!first_use_permitted(&Command::ActionsStatus {
            invocation_id: 1,
            wait_ms: None
        }));
        let command = crate::shell::test_control::parse_command(
            r#"{"op":"actions_invoke","provider":"host-applications","id":"launch"}"#,
        )
        .expect("real codec");
        assert!(!first_use_permitted(&command));
    }
}
