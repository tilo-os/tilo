//! How a program of this computer asks its daemon to do something as the device.
//!
//! The daemon (`tilo-runtime`) is the one process that writes as the device: it takes the device
//! key ([`crate::persist::take_device_key`]) and nothing else reads it. A program that needs the
//! device to sign, seal or prove something calls one of the daemon's device actions
//! ([`PROVIDER`], [`RESOURCE`]) at its native endpoint, the socket every client of this computer
//! reaches it at ([`sockets`]), and gets the answer, or the daemon's refusal ([`DeviceRefusal`]).
//! A daemon that is not running is said plainly ([`NOT_RUNNING`]); nothing reads the key in its
//! place.

use std::io::{BufRead as _, Write as _};
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde_json::Value;

#[cfg(doc)]
use super::PROVIDER;
use super::RESOURCE;

/// The refusal's code when the device's daemon is not running.
pub const NOT_RUNNING: &str = "not-running";
/// The code of a device action's refusal that named none.
pub const REFUSED: &str = "refused";
/// The code of a refusal that the daemon took the request and stopped answering.
pub const UNANSWERED: &str = "unanswered";

/// Where the system unit keeps the daemon's sockets on Tilo OS.
const SYSTEM_SOCKETS: &str = tilo_machine::DAEMON_SOCKETS;
/// The native endpoint's socket, in the daemon's sockets.
const NATIVE: &str = tilo_machine::DAEMON_NATIVE;
/// The longest a socket's path may be: a socket's address fits 104 bytes on macOS (108 on
/// Linux), its closing NUL among them.
const SOCKET_PATH_MAX: usize = 103;

/// How long one read of a followed call waits for it to move before it is read again: within
/// [`ANSWERING_WITHIN`], so a stage that lasts longer than one answer is waited for (a person's
/// part that takes a while to start) is still answered, as pending, before the caller's deadline.
const FOLLOW_WAIT_MS: u64 = 5_000;

/// How long a call's answer is waited for, and how long a followed call may go without a report
/// before the caller is told the daemon took it and did not answer. A daemon whose person's part
/// is still starting, or whose workspace cannot be read, answers nothing at all: what a command
/// does meanwhile is say so, within seconds.
const ANSWERING_WITHIN: Duration = Duration::from_secs(60);

/// Why the daemon did not do a device action: its stable code, and its words.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DeviceRefusal {
    pub code: String,
    pub message: String,
}

impl DeviceRefusal {
    /// A refusal said in `message` alone.
    #[must_use]
    pub fn said(message: String) -> Self {
        Self {
            code: REFUSED.to_owned(),
            message,
        }
    }

    /// Whether the refusal is that the device's daemon is not running.
    #[must_use]
    pub fn not_running(&self) -> bool {
        self.code == NOT_RUNNING
    }
}

impl std::fmt::Display for DeviceRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl std::error::Error for DeviceRefusal {}

/// The directory this computer's daemon keeps its sockets in, for the login whose device
/// directory is `login`, and whether it is the system unit's: a daemon serving a profile named
/// apart (a VM host's, a stack's) in a directory of that profile's ([`profile_sockets`]); under
/// Tilo OS's system unit its runtime directory; a person's own service in `$XDG_RUNTIME_DIR/tilo`
/// on Linux and the account's temporary directory on macOS, made private to this account.
///
/// # Errors
/// The sockets' directory cannot be made, is not this account's own, or a socket's path in it is
/// too long for a socket's address.
pub fn sockets(login: &Path) -> Result<(PathBuf, bool), String> {
    let named = named_profile();
    let sockets = match placed(tilo_machine::system_daemon(), named.as_deref()) {
        Placed::System => PathBuf::from(SYSTEM_SOCKETS),
        Placed::Apart(profile) => profile_sockets(login, profile)?,
        Placed::Own => own_sockets(login)?,
    };
    socket_fits(&sockets.join(NATIVE))?;
    let system = sockets == Path::new(SYSTEM_SOCKETS);
    Ok((sockets, system))
}

/// The profile this environment names its daemon apart with, when it names one
/// (`TILO_RUNTIME_PROFILE`): a host's, a stack's, a test's. Such a daemon is started by whoever
/// named the profile, and no command starts a service for it; a client waits for that daemon. The
/// profile Tilo OS's system unit names for its own daemon, which its programs inherit, is the
/// computer's and names none apart.
#[must_use]
pub fn named_profile() -> Option<PathBuf> {
    std::env::var_os(tilo_machine::DAEMON_PROFILE_VAR)
        .filter(|profile| !profile.is_empty())
        .map(PathBuf::from)
        .filter(|profile| {
            !tilo_machine::serves_the_computer(tilo_machine::system_daemon(), Some(profile))
        })
}

/// Where a daemon's sockets go, before any directory is made.
#[derive(Debug, PartialEq, Eq)]
enum Placed<'a> {
    /// The system unit's runtime directory, which every account of the computer reaches.
    System,
    /// A directory of the profile's own beside this account's sockets.
    Apart(&'a Path),
    /// This account's own service's directory.
    Own,
}

/// Where the daemon serving the profile `named` listens, on a computer whose daemon is the
/// system's (`system`) or not. The system unit names its own profile and listens where every
/// client finds it; a daemon serving any other profile, as one account's, listens in that
/// profile's own directory, and a command run beside it with the same profile asks it there.
fn placed(system: bool, named: Option<&Path>) -> Placed<'_> {
    if tilo_machine::serves_the_computer(system, named) {
        Placed::System
    } else if let Some(profile) = named {
        Placed::Apart(profile)
    } else {
        Placed::Own
    }
}

/// The native endpoint's socket in the daemon's `sockets`.
#[must_use]
pub fn native_in(sockets: &Path) -> PathBuf {
    sockets.join(NATIVE)
}

/// Whether a socket's address fits `path`: one it does not is refused, naming it, never cut
/// short.
///
/// # Errors
/// The path is longer than a socket's address fits.
pub fn socket_fits(path: &Path) -> Result<(), String> {
    let len = path.as_os_str().len();
    if len > SOCKET_PATH_MAX {
        return Err(format!(
            "{} is too long for a socket's path ({len} bytes, at most {SOCKET_PATH_MAX})",
            path.display()
        ));
    }
    Ok(())
}

/// The sockets of the daemon serving a `profile` named apart (a test's, a VM host's, a stack's),
/// for the login whose device directory is `login`: a private directory of this account's own,
/// named by the profile's path, beside the account's own service's sockets. It is the account's
/// own runtime directory, never the system unit's, which every account reaches. A profile's path
/// is the person's state, which may be as long as they keep it, so it never becomes a socket's
/// path; two profiles never share a directory.
///
/// # Errors
/// The sockets' directory cannot be made, or it is not this account's own private directory.
pub fn profile_sockets(login: &Path, profile: &Path) -> Result<PathBuf, String> {
    let base = account_sockets(login)?;
    private_dir(&base)?;
    let dir = base.join(profile_socket_name(profile));
    private_dir(&dir)?;
    Ok(dir)
}

/// Where this account's own programs keep their sockets, on every host.
#[cfg(unix)]
fn account_sockets(login: &Path) -> Result<PathBuf, String> {
    tilo_machine::account_sockets(owner(login)?)
}

/// Where this account's own programs keep their sockets, on every host.
#[cfg(windows)]
fn account_sockets(login: &Path) -> Result<PathBuf, String> {
    let _ = login;
    tilo_machine::daemon_sockets()
}

/// A profile's name among this account's sockets: a hash of its path, so the daemon serving it
/// and every client that names it compute the same directory.
fn profile_socket_name(profile: &Path) -> String {
    // FNV-1a: stable across processes and builds, and no hashing dependency for it.
    let mut hash = 0xcbf2_9ce4_8422_2325_u64;
    for byte in profile.as_os_str().as_encoded_bytes() {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("profile-{hash:016x}")
}

/// The account that owns the login's directory `login`.
#[cfg(unix)]
fn owner(login: &Path) -> Result<u32, String> {
    use std::os::unix::fs::MetadataExt as _;
    Ok(std::fs::metadata(login)
        .map_err(|error| format!("{} is not readable: {error}", login.display()))?
        .uid())
}

/// The directory a person's own service keeps its sockets in, made private to this account.
#[cfg(unix)]
fn own_sockets(login: &Path) -> Result<PathBuf, String> {
    let dir = tilo_machine::daemon_sockets(owner(login)?)?;
    private_dir(&dir)?;
    Ok(dir)
}

/// Make `dir`, which keeps this account's sockets, this account's own private directory.
#[cfg(unix)]
fn private_dir(dir: &Path) -> Result<(), String> {
    use std::os::unix::fs::{DirBuilderExt as _, MetadataExt as _};
    std::fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .map_err(|error| format!("{} could not be made: {error}", dir.display()))?;
    let made = std::fs::symlink_metadata(dir)
        .map_err(|error| format!("{} is not readable: {error}", dir.display()))?;
    let parent = dir
        .parent()
        .and_then(|parent| std::fs::metadata(parent).ok());
    if !made.is_dir()
        || made.mode() & 0o077 != 0
        || parent.is_some_and(|parent| parent.uid() != made.uid())
    {
        return Err(format!(
            "{} is not this account's own private directory",
            dir.display()
        ));
    }
    Ok(())
}

/// The directory a person's own service names its pipes by, made private to this account: what
/// Windows keeps of a pipe is its name, which this account's profile alone has.
#[cfg(windows)]
fn own_sockets(login: &Path) -> Result<PathBuf, String> {
    let _ = login;
    let dir = tilo_machine::daemon_sockets()?;
    private_dir(&dir)?;
    Ok(dir)
}

/// Make `dir`, which keeps this account's sockets, this account's own private directory.
#[cfg(windows)]
fn private_dir(dir: &Path) -> Result<(), String> {
    tilo_files::native::private::directory(dir)
        .map_err(|error| format!("{} could not be made private: {error}", dir.display()))
}

/// The device actions of the daemon listening at one native endpoint.
#[derive(Clone, Debug)]
pub struct DeviceActions {
    native: PathBuf,
}

impl DeviceActions {
    /// The device actions of the daemon whose native endpoint is `native`.
    #[must_use]
    pub fn at(native: impl Into<PathBuf>) -> Self {
        Self {
            native: native.into(),
        }
    }

    /// The device actions of this computer's daemon, for the login whose identity `paths` keeps.
    ///
    /// # Errors
    /// The login's directory or the sockets' directory cannot be made ([`sockets`]).
    pub fn here(paths: crate::IdentityPaths) -> Result<Self, String> {
        let login = super::Place::here(paths).map_err(|error| format!("{error:#}"))?;
        let (sockets, _) = sockets(login.dir())?;
        Ok(Self::at(native_in(&sockets)))
    }

    /// The daemon's native endpoint these actions are asked at.
    #[must_use]
    pub fn native(&self) -> &Path {
        &self.native
    }

    /// This device's public key, as its daemon answers what the device is.
    ///
    /// # Errors
    /// The daemon does not answer, refused, or named no 32-byte key.
    pub fn device_key(&self) -> Result<[u8; 32], DeviceRefusal> {
        let info = self.ask("info", serde_json::json!({}))?;
        info.get("device")
            .and_then(Value::as_str)
            .and_then(|key| hex::decode(key).ok())
            .and_then(|key| key.try_into().ok())
            .ok_or_else(|| {
                DeviceRefusal::said(format!("the device service answered no device key: {info}"))
            })
    }

    /// `message` signed with this device's key by its daemon ([`super::SIGN`]): the key never
    /// leaves it.
    ///
    /// # Errors
    /// The daemon does not answer, refused, or answered no 64-byte signature.
    pub fn sign(&self, message: &[u8]) -> Result<[u8; 64], DeviceRefusal> {
        let signed = self.ask(
            super::SIGN,
            serde_json::json!({ "message": hex::encode(message) }),
        )?;
        signed
            .get("signature")
            .and_then(Value::as_str)
            .and_then(|signature| hex::decode(signature).ok())
            .and_then(|signature| signature.try_into().ok())
            .ok_or_else(|| {
                DeviceRefusal::said(format!(
                    "the device service answered no signature: {signed}"
                ))
            })
    }

    /// Renew this device's certificates of the asking account's person that are due, with the
    /// root the daemon keeps for that account ([`super::RENEW_CERTIFICATES`]): only the daemon
    /// opens it. Answers how many were renewed.
    ///
    /// # Errors
    /// The daemon does not answer, or refused: it keeps no root of the person's while one of
    /// their certificates is due, or one could not be renewed.
    pub fn renew_certificates(&self) -> Result<u64, DeviceRefusal> {
        let renewed = self.ask(super::RENEW_CERTIFICATES, serde_json::json!({}))?;
        renewed
            .get("renewed")
            .and_then(Value::as_u64)
            .ok_or_else(|| {
                DeviceRefusal::said(format!("the device service answered no renewal: {renewed}"))
            })
    }

    /// The device action `action` called with `args`: its result.
    ///
    /// # Errors
    /// The daemon does not answer, or refused.
    pub fn act(&self, action: &str, args: Value) -> Result<Value, String> {
        self.ask(action, args).map_err(|refused| refused.message)
    }

    /// [`Self::act`], with the refusal's code beside its words, for a caller that answers one
    /// refusal in its own way.
    ///
    /// # Errors
    /// The daemon does not answer ([`NOT_RUNNING`] when nothing listens), or refused.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller builds the answer's arguments for this one call"
    )]
    pub fn ask(&self, action: &str, args: Value) -> Result<Value, DeviceRefusal> {
        let (typed, text, failed) = self.call(action, &args, None).map_err(|refusal| {
            if refusal.code == UNANSWERED {
                DeviceRefusal {
                    code: refusal.code,
                    message: format!(
                        "{}: run `tilo status` to see how it stands, then try again",
                        refusal.message
                    ),
                }
            } else {
                refusal
            }
        })?;
        answered(&typed, &text, failed)
    }

    /// [`Self::ask`] for an action that may wait (for the person's answer at a device of theirs,
    /// say): its call is followed by its invocation until it settles, and a call that reports
    /// the stage it is in is answered each time it moves. The daemon has [`ANSWERING_WITHIN`]
    /// for each report: one that stops reporting is told as such, naming the last stage it
    /// reached.
    ///
    /// # Errors
    /// The daemon does not answer, or the call failed or was refused.
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller builds the answer's arguments for this one call"
    )]
    pub fn ask_followed(&self, action: &str, args: Value) -> Result<Value, DeviceRefusal> {
        let mut following = None;
        // The stage the daemon last said the call is in, in words that follow "it last
        // reported that": what a caller is told when it stops reporting.
        let mut stage: Option<String> = None;
        loop {
            let (typed, text, failed) = match self.call(action, &args, following.as_deref()) {
                Ok(answer) => answer,
                Err(refusal) if refusal.code == UNANSWERED => {
                    return Err(DeviceRefusal {
                        code: refusal.code,
                        message: stage.map_or_else(
                            || {
                                format!(
                                    "{}: run `tilo status` to see how it stands, then try again",
                                    refusal.message
                                )
                            },
                            |stage| {
                                format!(
                                    "{}; it last reported that {stage}: run `tilo status` to see \
                                     how it stands, then try again",
                                    refusal.message
                                )
                            },
                        ),
                    });
                }
                Err(refusal) => return Err(refusal),
            };
            if let Some(reported) = typed.get("waiting_for").and_then(Value::as_str) {
                stage = Some(format!("it is {}", reported.trim_end_matches('.')));
            }
            match typed.get("status").and_then(Value::as_str) {
                Some("asked" | "pending") => {
                    stage.get_or_insert_with(|| "it took the request".to_owned());
                    following = Some(
                        typed
                            .get("invocation_id")
                            .and_then(Value::as_str)
                            .ok_or_else(|| {
                                DeviceRefusal::said("Tilo named no call to follow".into())
                            })?
                            .to_owned(),
                    );
                }
                _ => return answered(&typed, &text, failed),
            }
        }
    }

    /// The call `action` with `args`, or the read of the call `following` names: the daemon's
    /// typed answer, its words, and whether it says it failed.
    fn call(
        &self,
        action: &str,
        args: &Value,
        following: Option<&str>,
    ) -> Result<(Value, String, bool), DeviceRefusal> {
        let mut arguments = serde_json::json!({
            "path": format!("{RESOURCE}#{action}"),
            "args": args,
        });
        if let (Some(invocation), Some(fields)) = (following, arguments.as_object_mut()) {
            fields.insert("invocation_id".into(), invocation.into());
            fields.insert("wait_ms".into(), FOLLOW_WAIT_MS.into());
        }
        self.tool("tilo_do", &arguments)
    }

    /// [`Self::ask`] for an action whose answer may come with a descriptor handed beside it: the
    /// answer's first message is taken by `receive`, which answers its bytes and what it carried.
    ///
    /// # Errors
    /// The daemon does not answer, or refused.
    #[cfg(unix)]
    #[expect(
        clippy::needless_pass_by_value,
        reason = "the caller builds the answer's arguments for this one call"
    )]
    pub fn ask_receiving<T>(
        &self,
        action: &str,
        args: Value,
        receive: impl FnOnce(&std::os::unix::net::UnixStream) -> std::io::Result<(Vec<u8>, T)>,
    ) -> Result<(Value, T), DeviceRefusal> {
        let arguments = serde_json::json!({
            "path": format!("{RESOURCE}#{action}"),
            "args": args,
        });
        let native = self.asked("tilo_do", &arguments)?;
        let (first, carried) = receive(&native).map_err(|error| read_failed(&error))?;
        let mut answer = String::from_utf8_lossy(&first).into_owned();
        if !answer.ends_with('\n') {
            std::io::BufReader::new(native)
                .read_line(&mut answer)
                .map_err(|error| read_failed(&error))?;
        }
        let (typed, text, failed) = parse_answer(&answer)?;
        Ok((answered(&typed, &text, failed)?, carried))
    }

    /// The daemon's answer to the tool `tool` with `arguments`: its typed answer, its words, and
    /// whether it says it failed.
    pub fn tool(
        &self,
        tool: &str,
        arguments: &Value,
    ) -> Result<(Value, String, bool), DeviceRefusal> {
        let native = self.asked(tool, arguments)?;
        let mut answer = String::new();
        std::io::BufReader::new(native)
            .read_line(&mut answer)
            .map_err(|error| read_failed(&error))?;
        parse_answer(&answer)
    }

    /// A connection to the daemon that asked the tool `tool` with `arguments`, its answer to
    /// come.
    fn asked(
        &self,
        tool: &str,
        arguments: &Value,
    ) -> Result<tilo_files::native::socket::Stream, DeviceRefusal> {
        let mut native =
            tilo_files::native::socket::connect(&self.native).map_err(|error| DeviceRefusal {
                code: NOT_RUNNING.to_owned(),
                message: format!(
                    "the device service isn't running on this computer ({}: {error})",
                    self.native.display()
                ),
            })?;
        // A daemon that took this connection but never answers leaves the caller hanging for as
        // long as it is broken: it is told the daemon did not answer, and where to see why.
        native
            .set_read_timeout(Some(ANSWERING_WITHIN))
            .map_err(|error| DeviceRefusal::said(error.to_string()))?;
        let line = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/call",
            "params": {"name": tool, "arguments": arguments,
                "_meta": {"so.tilo.mcp": {"structured": true}}},
        });
        writeln!(native, "{line}").map_err(|error| DeviceRefusal::said(error.to_string()))?;
        Ok(native)
    }
}

/// Why the daemon's answer could not be read: a daemon that took the request and never answered
/// is told as one.
fn read_failed(error: &std::io::Error) -> DeviceRefusal {
    match error.kind() {
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut => DeviceRefusal {
            code: UNANSWERED.to_owned(),
            message: format!(
                "Tilo's daemon took this request and did not answer it within {} seconds",
                ANSWERING_WITHIN.as_secs()
            ),
        },
        _ => DeviceRefusal::said(error.to_string()),
    }
}

/// The daemon's answer line `answer`: its typed answer, its words, and whether it says it failed.
fn parse_answer(answer: &str) -> Result<(Value, String, bool), DeviceRefusal> {
    let answer: Value = serde_json::from_str(answer.trim()).map_err(|_| {
        DeviceRefusal::said(format!(
            "Tilo answered something unreadable: {}",
            answer.trim()
        ))
    })?;
    if let Some(error) = answer.get("error") {
        return Err(DeviceRefusal::said(
            error
                .get("message")
                .and_then(Value::as_str)
                .map_or_else(|| error.to_string(), str::to_owned),
        ));
    }
    let text = answer
        .pointer("/result/content/0/text")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let typed = answer
        .pointer("/result/structuredContent")
        .cloned()
        .unwrap_or_default();
    let failed = answer.pointer("/result/isError") == Some(&Value::Bool(true));
    Ok((typed, text, failed))
}

/// What the daemon's answer `typed`, in words `text`, gives: its result, or its refusal.
fn answered(typed: &Value, text: &str, failed: bool) -> Result<Value, DeviceRefusal> {
    match typed.get("status").and_then(Value::as_str) {
        Some("ok") => Ok(typed.get("result").cloned().unwrap_or_default()),
        // An answer in words alone: what it said, read as the value it names.
        None if !failed => {
            Ok(serde_json::from_str(text).unwrap_or_else(|_| Value::String(text.to_owned())))
        }
        _ => Err(DeviceRefusal {
            code: typed
                .pointer("/error/code")
                .and_then(Value::as_str)
                .unwrap_or(REFUSED)
                .to_owned(),
            message: typed
                .pointer("/error/message")
                .and_then(Value::as_str)
                .filter(|message| !message.is_empty())
                .unwrap_or(text)
                .to_owned(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// On Tilo OS the system unit names its own profile and still listens where every client
    /// finds it, while a daemon a test or a VM host starts over a profile of its own listens apart,
    /// as that account's, and is not the computer's.
    #[test]
    fn the_system_units_profile_is_the_computers_and_any_other_is_apart() {
        let system_profile = Path::new(tilo_machine::DAEMON_SYSTEM_PROFILE);
        assert_eq!(placed(true, Some(system_profile)), Placed::System);
        assert_eq!(placed(true, None), Placed::System);
        let apart = Path::new("/var/tmp/owner/daemon");
        assert_eq!(placed(true, Some(apart)), Placed::Apart(apart));
        assert_eq!(placed(false, Some(apart)), Placed::Apart(apart));
        // Off Tilo OS no profile is the system's, whatever its path.
        assert_eq!(
            placed(false, Some(system_profile)),
            Placed::Apart(system_profile)
        );
        assert_eq!(placed(false, None), Placed::Own);
    }

    /// A sockets' directory whose longest socket path still fits a socket's address is taken; one
    /// a byte longer is refused, naming the path it would have made, never cut short.
    #[test]
    fn a_socket_path_too_long_for_its_address_is_refused_by_name() {
        let longest = NATIVE.len();
        // `/`, the directory's name, `/`, the socket's name.
        let room = SOCKET_PATH_MAX - longest - 2;
        let fitting = PathBuf::from(format!("/{}", "d".repeat(room)));
        assert_eq!(socket_fits(&native_in(&fitting)), Ok(()));
        let over = PathBuf::from(format!("/{}", "d".repeat(room + 1)));
        let refused =
            socket_fits(&native_in(&over)).expect_err("a path past the address is refused");
        assert!(refused.contains(&over.display().to_string()), "{refused}");
        assert!(refused.contains("too long"), "{refused}");
    }

    /// On macOS a person's own service keeps its sockets in the account's temporary directory,
    /// as the system names it, private to the account, and short enough for every socket.
    #[cfg(target_os = "macos")]
    #[test]
    fn a_persons_sockets_on_macos_are_in_their_temporary_directory() {
        use std::os::unix::fs::PermissionsExt as _;
        let login = tempfile::tempdir().unwrap();
        let sockets = own_sockets(login.path()).unwrap();
        assert!(sockets.starts_with("/var/folders"), "{}", sockets.display());
        let mode = std::fs::metadata(&sockets).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o700, "{mode:o}");
        assert_eq!(socket_fits(&native_in(&sockets)), Ok(()));
    }

    /// With no daemon listening, a device action is refused as the service not running: never
    /// answered from anywhere else.
    #[test]
    fn a_device_action_with_no_daemon_says_the_device_service_is_not_running() {
        let sockets = tempfile::tempdir().unwrap();
        let refused = DeviceActions::at(native_in(sockets.path()))
            .ask("info", serde_json::json!({}))
            .expect_err("nothing listens");
        assert!(refused.not_running(), "{refused:?}");
        assert!(
            refused
                .message
                .starts_with("the device service isn't running on this computer"),
            "{refused}"
        );
    }
}
