//! Which app a process belongs to, and whether the person may end it.
//!
//! Every app runs in a cgroup of its own, so its processes are found by the
//! cgroup path in `/proc/<pid>/cgroup`, never by a name a program can pick:
//!
//! - a Tilo launch the launcher placed itself:
//!   `…/app.slice/app-tilo-<escaped id>-<pid>.scope`;
//! - a launch in the session's delegated app tree:
//!   `…/app.slice/tilo-apps-<n>.scope/app-<escaped id>-<k>`;
//! - another desktop's application, by the systemd convention
//!   (<https://systemd.io/DESKTOP_ENVIRONMENTS>):
//!   `app[-<launcher>]-<application id>[-<random>].scope` or
//!   `app[-<launcher>]-<application id>[@<random>].service`.
//!
//! Anything else — system services, the login session that holds the
//! compositor, the person's own background services (audio, the bus) — is
//! not an app. The person may end only processes that are theirs **and**
//! live inside their own `app.slice`: never a system service, never the
//! session that draws their screen.

#[must_use]
pub fn app_of_cgroup(path: &str) -> Option<String> {
    let mut components = path.split('/').filter(|part| !part.is_empty()).peekable();
    while let Some(part) = components.next() {
        if let Some(rest) = part.strip_prefix("app-tilo-")
            && let Some(rest) = rest.strip_suffix(".scope")
        {
            return tilo_leaf(rest);
        }
        if part.starts_with("tilo-apps-") && part.strip_suffix(".scope").is_some() {
            return components
                .peek()
                .and_then(|leaf| leaf.strip_prefix("app-"))
                .and_then(tilo_leaf);
        }
        if let Some(app) = desktop_unit(part) {
            return Some(app);
        }
    }
    None
}

/// `<escaped id>-<number>`, the id spelled as in a unit name.
fn tilo_leaf(leaf: &str) -> Option<String> {
    let (escaped, number) = leaf.rsplit_once('-')?;
    (!escaped.is_empty() && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| unescape(escaped))
}

/// An application unit by the systemd convention.
fn desktop_unit(part: &str) -> Option<String> {
    let rest = part.strip_prefix("app-")?;
    let (stem, scope) = if let Some(stem) = rest.strip_suffix(".scope") {
        (stem, true)
    } else {
        (rest.strip_suffix(".service")?, false)
    };
    // `@<random>` ends a service's name; a scope ends in `-<random>`.
    let stem = stem.split_once('@').map_or(stem, |(stem, _)| stem);
    let mut fields: Vec<&str> = stem.split('-').filter(|f| !f.is_empty()).collect();
    if scope && fields.len() > 1 {
        fields.pop();
    }
    // With two fields left the first names the launcher that started it.
    let id = match fields.as_slice() {
        [id] | [_, id] => *id,
        _ => return None,
    };
    let id = unescape(id);
    (!id.is_empty()).then_some(id)
}

/// Undo a unit name's `\xNN` escapes.
#[must_use]
pub fn unescape(escaped: &str) -> String {
    let bytes = escaped.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut index = 0_usize;
    while let Some(&byte) = bytes.get(index) {
        if byte == b'\\'
            && bytes.get(index.saturating_add(1)) == Some(&b'x')
            && let Some(hex) = bytes.get(index.saturating_add(2)..index.saturating_add(4))
            && let Some(value) = std::str::from_utf8(hex)
                .ok()
                .and_then(|hex| u8::from_str_radix(hex, 16).ok())
        {
            out.push(value);
            index = index.saturating_add(4);
        } else {
            out.push(byte);
            index = index.saturating_add(1);
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// The unified hierarchy's path in a `/proc/<pid>/cgroup` file.
#[must_use]
pub fn unified_path(cgroup_file: &str) -> Option<&str> {
    cgroup_file
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
}

/// Who may end what: the person's uid, and the root of their apps.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndPolicy {
    /// The person the service works for.
    pub uid: u32,
    /// The cgroup every one of their apps lives under
    /// (`/user.slice/user-<uid>.slice/user@<uid>.service/app.slice`).
    pub app_root: String,
    /// The service's own process, which is never a target.
    pub own_pid: u32,
}

impl EndPolicy {
    /// The policy for `uid`, with the standard app root.
    #[must_use]
    pub fn for_user(uid: u32, own_pid: u32) -> Self {
        Self {
            uid,
            app_root: format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"),
            own_pid,
        }
    }

    /// The policy for the person running this process.
    #[must_use]
    pub fn current() -> Self {
        #[cfg(unix)]
        let uid = rustix::process::geteuid().as_raw();
        #[cfg(not(unix))]
        let uid = 0;
        Self::for_user(uid, std::process::id())
    }

    /// Whether a process of `uid` in cgroup `cgroup` may be ended.
    ///
    /// # Errors
    /// Why it may not.
    pub fn check(
        &self,
        pid: u32,
        uid: u32,
        cgroup: Option<&str>,
        kernel_thread: bool,
    ) -> Result<(), crate::protocol::Refusal> {
        use crate::protocol::Refusal;
        if uid != self.uid {
            return Err(Refusal::NotYours);
        }
        if kernel_thread || pid <= 1 || pid == self.own_pid {
            return Err(Refusal::NotAnApp);
        }
        let inside = cgroup.is_some_and(|path| {
            path.strip_prefix(self.app_root.as_str())
                .is_some_and(|rest| rest.is_empty() || rest.starts_with('/'))
        });
        if inside {
            Ok(())
        } else {
            Err(Refusal::NotAnApp)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::Refusal;

    #[test]
    fn apps_are_found_by_their_cgroup_and_nothing_else_is_an_app() {
        let base = "/user.slice/user-1000.slice/user@1000.service/app.slice";
        for (path, app) in [
            (
                format!("{base}/app-tilo-so.tilo.notes-4242.scope"),
                Some("so.tilo.notes"),
            ),
            (
                format!("{base}/app-tilo-so.tilo.my\\x2dapp-7.scope/sub"),
                Some("so.tilo.my-app"),
            ),
            (
                format!("{base}/tilo-apps-12.scope/app-so.tilo.files-3"),
                Some("so.tilo.files"),
            ),
            (format!("{base}/tilo-apps-12.scope/spawner"), None),
            (
                format!("{base}/app-gnome-org.mozilla.firefox-5512.scope"),
                Some("org.mozilla.firefox"),
            ),
            (
                format!("{base}/app-flatpak-com.spotify.Client@1a2b.service"),
                Some("com.spotify.Client"),
            ),
            (
                format!("{base}/app-org.gnome.Terminal.service"),
                Some("org.gnome.Terminal"),
            ),
            (
                "/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service"
                    .to_owned(),
                None,
            ),
            (
                "/user.slice/user-1000.slice/session-3.scope".to_owned(),
                None,
            ),
            ("/system.slice/sshd.service".to_owned(), None),
            ("/init.scope".to_owned(), None),
        ] {
            assert_eq!(app_of_cgroup(&path).as_deref(), app, "{path}");
        }
        assert_eq!(
            unified_path("1:name=systemd:/x\n0::/user.slice/a.scope\n"),
            Some("/user.slice/a.scope")
        );
    }

    #[test]
    fn only_the_persons_own_apps_may_be_ended() {
        let policy = EndPolicy::for_user(1000, 77);
        let app = format!("{}/app-tilo-so.tilo.notes-9.scope", policy.app_root);
        assert_eq!(policy.check(500, 1000, Some(&app), false), Ok(()));
        assert_eq!(
            policy.check(500, 0, Some(&app), false),
            Err(Refusal::NotYours),
            "another user's process"
        );
        assert_eq!(
            policy.check(500, 1001, Some(&app), false),
            Err(Refusal::NotYours)
        );
        for path in [
            "/system.slice/sshd.service",
            "/user.slice/user-1000.slice/session-3.scope",
            "/user.slice/user-1000.slice/user@1000.service/session.slice/pipewire.service",
            "/user.slice/user-1000.slice/user@1000.service/app.slice2/x.scope",
        ] {
            assert_eq!(
                policy.check(500, 1000, Some(path), false),
                Err(Refusal::NotAnApp),
                "{path}"
            );
        }
        assert_eq!(policy.check(500, 1000, None, false), Err(Refusal::NotAnApp));
        assert_eq!(
            policy.check(77, 1000, Some(&app), false),
            Err(Refusal::NotAnApp),
            "never the service itself"
        );
        assert_eq!(
            policy.check(1, 1000, Some(&app), false),
            Err(Refusal::NotAnApp)
        );
        assert_eq!(
            policy.check(500, 1000, Some(&app), true),
            Err(Refusal::NotAnApp)
        );
    }
}
