//! Naming the app behind a request the frontend could not name.
//!
//! The frontend names a sandboxed app from its sandbox and a host app from
//! its systemd unit, when a desktop entry of that id is installed. An app
//! started some other way (from a terminal, by a launcher that makes no
//! unit, an entry under another id) reaches the backend with the empty app
//! id. Refusing it would leave every such app without a file chooser or a
//! screenshot, so its process names it instead ([`Requesters::identify`]):
//!
//! 1. its systemd unit (`app-<launcher>-<id>-<n>.scope`,
//!    `app-<launcher>-<id>@<n>.service`), even with no entry installed;
//! 2. the installed desktop entry whose program is the process's executable;
//! 3. the executable itself, by its absolute path.
//!
//! An unsandboxed process may do anything its user may, so naming it gives
//! it no authority: it only lets the person see who asks, and keeps what
//! they decided for it apart from every other app.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::PortalCaller;
use crate::auth::RequesterResolver;

/// How many parents up a process's ancestry a unit is looked for: an app's
/// helper (its `xdg-dbus-proxy`, a script's interpreter) runs in its unit.
const MAX_ANCESTORS: usize = 16;

/// Where processes and installed apps are read from.
#[derive(Clone, Debug)]
pub struct Requesters {
    /// The procfs root, `/proc` on a running system.
    proc_root: PathBuf,
    /// The application directories, in precedence order.
    applications: Vec<PathBuf>,
}

impl Requesters {
    /// This system's processes and installed apps.
    #[must_use]
    pub fn host() -> Self {
        Self {
            proc_root: PathBuf::from("/proc"),
            applications: desktop_entry::scan::application_dirs(),
        }
    }

    /// Processes under `proc_root`, apps in `applications`: what tests
    /// build.
    #[must_use]
    pub const fn at(proc_root: PathBuf, applications: Vec<PathBuf>) -> Self {
        Self {
            proc_root,
            applications,
        }
    }

    /// The app process `pid` belongs to, as [`crate::requester`] names it.
    #[must_use]
    pub fn identify(&self, pid: u32) -> Option<PortalCaller> {
        if let Some(id) = self.unit_app(pid) {
            return Some(PortalCaller::new(id));
        }
        let exe = std::fs::read_link(self.proc_root.join(pid.to_string()).join("exe")).ok()?;
        let exe = exe.canonicalize().unwrap_or(exe);
        if let Some(id) = self.entry_running(&exe) {
            return Some(PortalCaller::new(id));
        }
        let path = exe.to_str()?;
        (path.starts_with('/') && !path.chars().any(char::is_control) && path.len() <= 255)
            .then(|| PortalCaller::new(path))
    }

    /// [`Self::identify`] as the authenticator takes it.
    #[must_use]
    pub fn resolver(self) -> RequesterResolver {
        Arc::new(move |pid| self.identify(pid))
    }

    /// The app id of the systemd app unit `pid` or a parent runs in.
    fn unit_app(&self, mut pid: u32) -> Option<String> {
        for _ in 0..MAX_ANCESTORS {
            let process = self.proc_root.join(pid.to_string());
            let cgroup = std::fs::read_to_string(process.join("cgroup")).ok()?;
            if let Some(id) = cgroup.lines().find_map(app_of_cgroup_line) {
                return Some(id);
            }
            let parent = parent_of(&std::fs::read_to_string(process.join("stat")).ok()?)?;
            if parent <= 1 || parent == pid {
                return None;
            }
            pid = parent;
        }
        None
    }

    /// The id of the installed entry whose program is `exe`.
    fn entry_running(&self, exe: &Path) -> Option<String> {
        let scan = desktop_entry::scan::scan_dirs(&self.applications, &[]);
        scan.entries
            .iter()
            .find(|entry| {
                entry
                    .exec
                    .as_deref()
                    .and_then(|exec| desktop_entry::exec::split_exec(exec).ok())
                    .and_then(|argv| argv.into_iter().next())
                    .and_then(|program| locate(&program))
                    .is_some_and(|program| program == exe)
            })
            .map(|entry| entry.id.clone())
    }
}

/// The program `name` runs, as a canonical path: itself when it is a path,
/// else found on `PATH`.
fn locate(name: &str) -> Option<PathBuf> {
    let found = if name.contains('/') {
        PathBuf::from(name)
    } else {
        std::env::split_paths(&std::env::var_os("PATH")?)
            .map(|dir| dir.join(name))
            .find(|candidate| candidate.is_file())?
    };
    found.canonicalize().ok()
}

/// The parent pid in a `/proc/<pid>/stat` line. The command name may hold
/// spaces and parentheses, so the fields are counted after its last `)`.
fn parent_of(stat: &str) -> Option<u32> {
    let (_, rest) = stat.rsplit_once(')')?;
    rest.split_whitespace().nth(1)?.parse().ok()
}

/// The app id in one `/proc/<pid>/cgroup` line naming a systemd app unit:
/// `app[-<launcher>]-<id>[@<instance>].service` or
/// `app[-<launcher>]-<id>-<instance>.scope`, the id escaped as systemd
/// escapes unit names. The launcher part is told from the id because an id
/// is reverse-DNS (it has dots) and a launcher name has none.
fn app_of_cgroup_line(line: &str) -> Option<String> {
    let path = line.splitn(3, ':').nth(2)?;
    let unit = path.rsplit('/').find(|part| part.starts_with("app-"))?;
    let rest = unit.strip_prefix("app-")?;
    let name = if let Some(service) = rest.strip_suffix(".service") {
        service.split_once('@').map_or(service, |(name, _)| name)
    } else {
        let scope = rest.strip_suffix(".scope")?;
        scope.rsplit_once('-')?.0
    };
    let id = match name.split_once('-') {
        Some((launcher, id)) if !launcher.contains('.') && id.contains('.') => id,
        _ => name,
    };
    let id = unescape_unit(id)?;
    (!id.is_empty() && id.contains('.') && !id.chars().any(char::is_control) && id.len() <= 255)
        .then_some(id)
}

/// A unit name part with systemd's `\xNN` escapes undone.
fn unescape_unit(escaped: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(escaped.len());
    let mut rest = escaped.as_bytes();
    while let Some((&first, tail)) = rest.split_first() {
        if first == b'\\' {
            let hex = tail.strip_prefix(b"x")?.get(..2)?;
            bytes.push(u8::from_str_radix(std::str::from_utf8(hex).ok()?, 16).ok()?);
            rest = tail.get(3..)?;
        } else {
            bytes.push(first);
            rest = tail;
        }
    }
    String::from_utf8(bytes).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_units_name_their_app() {
        let id = |line: &str| app_of_cgroup_line(line);
        assert_eq!(
            id("0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-gnome-org.gnome.Nautilus-4242.scope")
                .as_deref(),
            Some("org.gnome.Nautilus")
        );
        assert_eq!(
            id("0::/user.slice/user@1000.service/app.slice/app-org.example.Editor@a1b2.service")
                .as_deref(),
            Some("org.example.Editor")
        );
        assert_eq!(
            id("0::/user.slice/user@1000.service/app.slice/app-flatpak-org.example.Big\\x2dApp-77.scope")
                .as_deref(),
            Some("org.example.Big-App")
        );
        assert_eq!(
            id("0::/user.slice/user@1000.service/session.slice/org.gnome.Shell@wayland.service"),
            None,
            "a session service is no app"
        );
        assert_eq!(
            id("0::/user.slice/user@1000.service/app.slice/app-terminal-42.scope"),
            None,
            "an id without a dot names no app"
        );
    }

    #[test]
    fn the_parent_is_read_past_a_command_with_parentheses() {
        assert_eq!(parent_of("42 (a) b (c)) S 7 42 42 0"), Some(7));
        assert_eq!(parent_of("garbage"), None);
    }

    fn process(root: &Path, pid: u32, parent: u32, cgroup: &str, exe: Option<&Path>) {
        let dir = root.join(pid.to_string());
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("cgroup"), cgroup).unwrap();
        std::fs::write(dir.join("stat"), format!("{pid} (p) S {parent} 0 0")).unwrap();
        if let Some(exe) = exe {
            std::os::unix::fs::symlink(exe, dir.join("exe")).unwrap();
        }
    }

    #[test]
    fn an_unnamed_requester_is_named_by_unit_entry_or_executable() {
        let dir = tempfile::tempdir().unwrap();
        let proc_root = dir.path().join("proc");
        let apps = dir.path().join("applications");
        let bin = dir.path().join("bin");
        std::fs::create_dir_all(&apps).unwrap();
        std::fs::create_dir_all(&bin).unwrap();
        let editor = bin.join("editor");
        let tool = bin.join("tool");
        std::fs::write(&editor, "").unwrap();
        std::fs::write(&tool, "").unwrap();
        std::fs::write(
            apps.join("org.example.Editor.desktop"),
            format!(
                "[Desktop Entry]\nType=Application\nName=Editor\nExec={} %F\n",
                editor.display()
            ),
        )
        .unwrap();
        let plain = "0::/user.slice/user@1000.service/app.slice/vte-spawn-1.scope";
        // A helper whose parent runs in an app unit.
        process(
            &proc_root,
            10,
            1,
            "0::/user.slice/user@1000.service/app.slice/app-gnome-org.example.Mail-10.scope",
            None,
        );
        process(&proc_root, 11, 10, plain, None);
        process(&proc_root, 20, 1, plain, Some(&editor));
        process(&proc_root, 30, 1, plain, Some(&tool));
        let requesters = Requesters::at(proc_root, vec![apps]);
        let named = |pid| requesters.identify(pid).map(|caller| caller.app_id);
        assert_eq!(named(11).as_deref(), Some("org.example.Mail"));
        assert_eq!(named(20).as_deref(), Some("org.example.Editor"));
        assert_eq!(
            named(30),
            Some(tool.canonicalize().unwrap().display().to_string())
        );
        assert_eq!(named(99), None, "a process that is gone");
    }
}
