//! Who is asking: the application identity every secret is attributed to.
//!
//! An identity is never a name the caller supplies. It is derived from what
//! the kernel and the package system say about the calling process, or, for
//! a sandboxed app that reaches the store through the desktop portal, from
//! the compositor's own authenticated record of that app. The forms:
//!
//! | Form | Means |
//! |---|---|
//! | `sandbox:<package id>` | a sandboxed app the portal vouched for |
//! | `flatpak:<app id>` | a Flatpak, by its cgroup scope and `.flatpak-info` |
//! | `binary:<path>` | a root-owned, non-writable executable of the image |
//! | `local-process:<path>` | any other executable the person runs |
//!
//! A `local-process:` caller can use the store, and sees only what it made
//! itself: it cannot read what the system's own programs keep there.
//!
//! # A caller the kernel will not show
//!
//! The executable is read from `/proc/<pid>/exe`. A process that loaded the
//! device key made itself non-dumpable (`tilo_user_identity::persist`), and
//! the kernel refuses that link of a non-dumpable process to every other
//! process of the user (`EACCES`); the compositor is one. Such a caller may
//! prove, on its own bus connection, that it holds the device key, and name
//! its executable (`so.tilo.Secrets1.ProveCaller`,
//! `tilo_seal::secret_service::CallerKey`). The named executable then
//! stands in for the link, for that connection only, and is classified
//! exactly as a link read from `/proc` would be. It is consulted only while
//! the kernel refuses the link: a caller whose link is readable is always
//! named by it.
//!
//! This is no weaker than reading the link. Whoever can make the proof holds
//! the device key, and the device key with the person's salt is what the
//! store's own key derives from: such a process can already open the store's
//! file, whatever program it claims to be. The proof is bound to a fresh
//! challenge and to the prover's unique bus name, which the bus never hands
//! to another connection, so one seen on the bus cannot be replayed. What a
//! same-user process can set (its `comm`, its cgroup, which a delegated user
//! manager lets it join, its environment) is never used.

use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

/// The longest identity accepted or recorded.
const MAX_ID_BYTES: usize = 1024;

/// An application identity, in one of the forms of the module docs.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct AppId(String);

/// How far an identity is trusted with the store's shared contents.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Trust {
    /// Part of the image, a package or a portal-vouched sandbox.
    System,
    /// Anything else the person runs.
    Local,
}

impl AppId {
    /// A sandboxed app the portal vouched for.
    #[must_use]
    pub fn sandbox(package: &str) -> Option<Self> {
        Self::checked(format!("sandbox:{package}"))
    }

    /// An identity read back from the vault, or typed by the person in
    /// Settings.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let known = ["sandbox:", "flatpak:", "binary:", "local-process:"]
            .iter()
            .any(|prefix| text.starts_with(prefix));
        known.then(|| Self::checked(text.to_owned())).flatten()
    }

    fn checked(text: String) -> Option<Self> {
        (text.len() <= MAX_ID_BYTES && !text.chars().any(char::is_control)).then_some(Self(text))
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }

    #[must_use]
    pub fn trust(&self) -> Trust {
        if self.0.starts_with("local-process:") {
            Trust::Local
        } else {
            Trust::System
        }
    }

    /// A short name for lists, derived from the identity and never trusted
    /// for anything else.
    #[must_use]
    pub fn display_name(&self) -> String {
        let body = self
            .0
            .split_once(':')
            .map_or(self.0.as_str(), |(_, rest)| rest);
        let leaf = body.rsplit('/').next().unwrap_or(body);
        if self.trust() == Trust::Local {
            format!("{leaf} (not part of the system)")
        } else {
            leaf.to_owned()
        }
    }
}

impl fmt::Display for AppId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

/// Why a process could not be given an identity.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum UnknownCaller {
    #[error("the caller's process cannot be examined")]
    Unreadable,
    #[error("the caller runs as another user")]
    OtherUser,
    #[error("the caller is a proxy for a process that cannot be identified")]
    Proxied,
    #[error("the caller's executable was replaced while it ran")]
    Replaced,
}

/// Programs that run other people's code. Their executable says nothing
/// about the program, so they are never `binary:` identities.
const INTERPRETERS: &[&str] = &[
    "python", "perl", "ruby", "node", "nodejs", "bun", "deno", "java", "lua", "luajit", "php",
    "bash", "sh", "dash", "zsh", "fish", "ksh", "tclsh", "wish", "guile", "racket",
];

fn is_interpreter(leaf: &str) -> bool {
    let stem = leaf.trim_end_matches(|c: char| c.is_ascii_digit() || c == '.');
    INTERPRETERS.contains(&stem)
}

/// The identity of process `pid`, as seen under `proc_root` (`/proc`).
///
/// `expected_uid` is the person the store belongs to; a process of another
/// user is refused outright.
///
/// # Errors
///
/// [`UnknownCaller`] when the process cannot be examined or is a proxy.
pub fn identify_process(
    proc_root: &Path,
    pid: u32,
    expected_uid: u32,
) -> Result<AppId, UnknownCaller> {
    identify_proven_process(proc_root, pid, expected_uid, None)
}

/// [`identify_process`] for a caller that may have proved it holds the
/// device key: `proven` is the executable it named then, which stands in
/// for `/proc/<pid>/exe` only while the kernel refuses that link (see the
/// module docs).
///
/// # Errors
///
/// As [`identify_process`].
pub fn identify_proven_process(
    proc_root: &Path,
    pid: u32,
    expected_uid: u32,
    proven: Option<&Path>,
) -> Result<AppId, UnknownCaller> {
    let process = proc_root.join(pid.to_string());
    if process_uid(&process).ok_or(UnknownCaller::Unreadable)? != expected_uid {
        return Err(UnknownCaller::OtherUser);
    }
    let executable = executable(std::fs::read_link(process.join("exe")), proven)?;
    classify(&process, &executable)
}

/// The identity of `process` running `executable`, whether the kernel
/// showed the executable or the caller proved it.
fn classify(process: &Path, executable: &Path) -> Result<AppId, UnknownCaller> {
    if executable.to_string_lossy().ends_with(" (deleted)") {
        return Err(UnknownCaller::Replaced);
    }
    let leaf = executable
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .ok_or(UnknownCaller::Unreadable)?;
    if leaf == "xdg-dbus-proxy" {
        // A filtered proxy forwards for a sandbox; its own identity says
        // nothing about the app behind it, and apps reach secrets through
        // the portal instead.
        return Err(UnknownCaller::Proxied);
    }
    if let Some(app) = flatpak_app(process) {
        return AppId::parse(&format!("flatpak:{app}")).ok_or(UnknownCaller::Unreadable);
    }
    let kind = if is_interpreter(&leaf) || !immutable_system_binary(executable) {
        "local-process"
    } else {
        "binary"
    };
    AppId::parse(&format!("{kind}:{}", executable.display())).ok_or(UnknownCaller::Unreadable)
}

/// The caller's executable: the link the kernel shows, or, only when it
/// refuses to show it (`EACCES`, a non-dumpable process), what the caller
/// proved. A link that is gone (the process exited) is never replaced by a
/// proof.
fn executable(
    link: std::io::Result<PathBuf>,
    proven: Option<&Path>,
) -> Result<PathBuf, UnknownCaller> {
    match link {
        Ok(path) => Ok(path),
        Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied => proven
            .map(Path::to_path_buf)
            .ok_or(UnknownCaller::Unreadable),
        Err(_) => Err(UnknownCaller::Unreadable),
    }
}

fn process_uid(process: &Path) -> Option<u32> {
    std::fs::read_to_string(process.join("status"))
        .ok()?
        .lines()
        .find_map(|line| {
            line.strip_prefix("Uid:")?
                .split_ascii_whitespace()
                .next()?
                .parse()
                .ok()
        })
}

/// A Flatpak's app id: named both by its systemd scope and by the
/// `.flatpak-info` the runtime places at the root of its mount namespace.
/// Either alone can be forged by the app, so both must agree.
fn flatpak_app(process: &Path) -> Option<String> {
    let cgroup = std::fs::read_to_string(process.join("cgroup")).ok()?;
    let from_scope = cgroup.lines().find_map(|line| {
        let scope = line.rsplit('/').next()?;
        let rest = scope.strip_prefix("app-flatpak-")?.strip_suffix(".scope")?;
        let (id, _instance) = rest.rsplit_once('-')?;
        Some(id.to_owned())
    })?;
    let info = std::fs::read_to_string(process.join("root/.flatpak-info")).ok()?;
    let declared = info
        .lines()
        .skip_while(|line| line.trim() != "[Application]")
        .skip(1)
        .take_while(|line| !line.starts_with('['))
        .find_map(|line| line.strip_prefix("name=").map(str::trim))?;
    (declared == from_scope).then_some(from_scope)
}

/// Owned by root, not writable by group or others, and so not something the
/// person's own processes can swap for another program.
fn immutable_system_binary(path: &Path) -> bool {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};
    path.is_absolute()
        && std::fs::metadata(path).is_ok_and(|metadata| {
            metadata.is_file() && metadata.uid() == 0 && metadata.permissions().mode() & 0o022 == 0
        })
        && path.starts_with("/usr")
}

/// The identity a trusted caller of the management interface must have.
#[must_use]
pub fn trusted_managers() -> Vec<AppId> {
    ["/usr/bin/shell", "/usr/bin/tilo-secret-service"]
        .iter()
        .filter_map(|path| AppId::parse(&format!("binary:{path}")))
        .collect()
}

/// The `/proc` the service reads, overridable only in tests.
#[must_use]
pub fn proc_root() -> PathBuf {
    PathBuf::from("/proc")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn fake_process(root: &Path, pid: u32, uid: u32, exe: &Path, cgroup: &str, info: Option<&str>) {
        let dir = root.join(pid.to_string());
        fs::create_dir_all(dir.join("root")).unwrap();
        fs::write(
            dir.join("status"),
            format!("Name:\tx\nUid:\t{uid}\t{uid}\t{uid}\t{uid}\n"),
        )
        .unwrap();
        std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
        fs::write(dir.join("cgroup"), cgroup).unwrap();
        if let Some(info) = info {
            fs::write(dir.join("root/.flatpak-info"), info).unwrap();
        }
    }

    #[test]
    fn a_program_the_person_installed_is_a_local_process() {
        let tree = tempfile::tempdir().unwrap();
        let exe = tree.path().join("home/chat");
        fs::create_dir_all(exe.parent().unwrap()).unwrap();
        fs::write(&exe, b"").unwrap();
        fake_process(tree.path(), 7, 1000, &exe, "0::/\n", None);
        let id = identify_process(tree.path(), 7, 1000).unwrap();
        assert_eq!(id.trust(), Trust::Local);
        assert!(id.as_str().starts_with("local-process:"));
    }

    #[test]
    fn a_process_of_another_user_is_refused() {
        let tree = tempfile::tempdir().unwrap();
        fake_process(
            tree.path(),
            7,
            1001,
            Path::new("/usr/bin/true"),
            "0::/\n",
            None,
        );
        assert_eq!(
            identify_process(tree.path(), 7, 1000),
            Err(UnknownCaller::OtherUser)
        );
        assert_eq!(
            identify_process(tree.path(), 8, 1000),
            Err(UnknownCaller::Unreadable)
        );
    }

    #[test]
    fn an_image_executable_is_a_binary_identity() {
        let tree = tempfile::tempdir().unwrap();
        fake_process(
            tree.path(),
            7,
            1000,
            Path::new("/usr/bin/true"),
            "0::/\n",
            None,
        );
        assert_eq!(
            identify_process(tree.path(), 7, 1000).unwrap().as_str(),
            "binary:/usr/bin/true"
        );
    }

    #[test]
    fn a_dbus_proxy_cannot_stand_in_for_an_app() {
        let tree = tempfile::tempdir().unwrap();
        fake_process(
            tree.path(),
            7,
            1000,
            Path::new("/usr/bin/xdg-dbus-proxy"),
            "0::/\n",
            None,
        );
        assert_eq!(
            identify_process(tree.path(), 7, 1000),
            Err(UnknownCaller::Proxied)
        );
    }

    #[test]
    fn a_flatpak_needs_its_scope_and_its_info_to_agree() {
        let tree = tempfile::tempdir().unwrap();
        let cgroup = "0::/user.slice/app.slice/app-flatpak-org.example.Chat-4242.scope\n";
        let info = "[Application]\nname=org.example.Chat\n";
        fake_process(
            tree.path(),
            1,
            1000,
            Path::new("/app/bin/chat"),
            cgroup,
            Some(info),
        );
        assert_eq!(
            identify_process(tree.path(), 1, 1000).unwrap().as_str(),
            "flatpak:org.example.Chat"
        );
        // An app that writes another name into its own info file.
        let forged = "[Application]\nname=org.example.Bank\n";
        fake_process(
            tree.path(),
            2,
            1000,
            Path::new("/app/bin/chat"),
            cgroup,
            Some(forged),
        );
        assert!(
            identify_process(tree.path(), 2, 1000)
                .unwrap()
                .as_str()
                .starts_with("local-process:")
        );
    }

    #[test]
    fn a_proof_stands_in_only_for_a_link_the_kernel_refuses() {
        let refused = || Err(std::io::Error::from(std::io::ErrorKind::PermissionDenied));
        let gone = || Err(std::io::Error::from(std::io::ErrorKind::NotFound));
        let shell = Path::new("/usr/bin/shell");
        // Non-dumpable and proven: the proven executable.
        assert_eq!(executable(refused(), Some(shell)), Ok(shell.to_path_buf()));
        // Non-dumpable and unproven: refused, as before.
        assert_eq!(executable(refused(), None), Err(UnknownCaller::Unreadable));
        // An exited process is never named by an earlier proof.
        assert_eq!(
            executable(gone(), Some(shell)),
            Err(UnknownCaller::Unreadable)
        );
        // A readable link always wins over what the caller claimed.
        assert_eq!(
            executable(Ok(PathBuf::from("/home/me/chat")), Some(shell)),
            Ok(PathBuf::from("/home/me/chat"))
        );
    }

    #[test]
    fn a_proven_executable_is_classified_like_a_link() {
        let tree = tempfile::tempdir().unwrap();
        fake_process(
            tree.path(),
            7,
            1000,
            Path::new("/usr/bin/true"),
            "0::/\n",
            None,
        );
        let process = tree.path().join("7");
        assert_eq!(
            classify(&process, Path::new("/usr/bin/true"))
                .unwrap()
                .as_str(),
            "binary:/usr/bin/true"
        );
        let home = tree.path().join("home/chat");
        fs::create_dir_all(home.parent().unwrap()).unwrap();
        fs::write(&home, b"").unwrap();
        assert!(
            classify(&process, &home)
                .unwrap()
                .as_str()
                .starts_with("local-process:")
        );
        assert_eq!(
            classify(&process, Path::new("/usr/bin/shell (deleted)")),
            Err(UnknownCaller::Replaced)
        );
        assert_eq!(
            classify(&process, Path::new("/usr/bin/xdg-dbus-proxy")),
            Err(UnknownCaller::Proxied)
        );
        // A caller of another user is refused before any proof is looked at.
        fake_process(
            tree.path(),
            8,
            1001,
            Path::new("/usr/bin/true"),
            "0::/\n",
            None,
        );
        assert_eq!(
            identify_proven_process(tree.path(), 8, 1000, Some(Path::new("/usr/bin/true"))),
            Err(UnknownCaller::OtherUser)
        );
        // A readable link names the caller, whatever it proved.
        assert_eq!(
            identify_proven_process(tree.path(), 7, 1000, Some(&home))
                .unwrap()
                .as_str(),
            "binary:/usr/bin/true"
        );
    }

    #[test]
    fn interpreters_never_identify_a_program() {
        assert!(is_interpreter("python3.14"));
        assert!(is_interpreter("bash"));
        assert!(!is_interpreter("chromium"));
    }

    #[test]
    fn identities_parse_only_in_the_known_forms() {
        assert!(AppId::parse("binary:/usr/bin/shell").is_some());
        assert!(AppId::parse("sandbox:so.tilo.notes").is_some());
        assert!(AppId::parse("so.tilo.notes").is_none());
        assert!(AppId::parse("binary:/usr/bin/x\nbinary:/usr/bin/shell").is_none());
        assert_eq!(
            AppId::parse("binary:/usr/lib64/chromium/chromium")
                .unwrap()
                .display_name(),
            "chromium"
        );
    }
}
