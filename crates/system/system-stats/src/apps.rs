//! Which app a process belongs to, and whether the person may end it.
//!
//! Anything else — system services, the login session that holds the
//! compositor, the person's own background services (audio, the bus) — is
//! not an app. The person may end only processes that are theirs **and**
//! live inside their own `app.slice`: never a system service, never the
//! session that draws their screen.

#[must_use]
pub fn app_of_cgroup(path: &str) -> Option<String> {
    for part in path.split('/').filter(|part| !part.is_empty()) {
        if let Some(rest) = part.strip_prefix("app-tilo-")
            && let Some(rest) = rest.strip_suffix(".scope")
        {
            return tilo_leaf(rest);
        }
        if let Some(app) = desktop_unit(part) {
            return Some(app);
        }
    }
    None
}

fn tilo_leaf(leaf: &str) -> Option<String> {
    let (escaped, number) = leaf.rsplit_once('-')?;
    (!escaped.is_empty() && !number.is_empty() && number.bytes().all(|b| b.is_ascii_digit()))
        .then(|| unescape(escaped))
}

fn desktop_unit(part: &str) -> Option<String> {
    let rest = part.strip_prefix("app-")?;
    let (stem, scope) = if let Some(stem) = rest.strip_suffix(".scope") {
        (stem, true)
    } else {
        (rest.strip_suffix(".service")?, false)
    };
    let stem = stem.split_once('@').map_or(stem, |(stem, _)| stem);
    let mut fields: Vec<&str> = stem.split('-').filter(|f| !f.is_empty()).collect();
    if scope && fields.len() > 1 {
        fields.pop();
    }
    let id = match fields.as_slice() {
        [id] | [_, id] => *id,
        _ => return None,
    };
    let id = unescape(id);
    (!id.is_empty()).then_some(id)
}

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

#[must_use]
pub fn unified_path(cgroup_file: &str) -> Option<&str> {
    cgroup_file
        .lines()
        .find_map(|line| line.strip_prefix("0::"))
        .map(str::trim)
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EndPolicy {
    pub uid: u32,
    /// The cgroup every one of their apps lives under
    /// (`/user.slice/user-<uid>.slice/user@<uid>.service/app.slice`).
    pub app_root: String,
    /// The service's own process, which is never a target.
    pub own_pid: u32,
}

impl EndPolicy {
    #[must_use]
    pub fn for_user(uid: u32, own_pid: u32) -> Self {
        Self {
            uid,
            app_root: format!("/user.slice/user-{uid}.slice/user@{uid}.service/app.slice"),
            own_pid,
        }
    }

    #[must_use]
    pub fn current() -> Self {
        #[cfg(unix)]
        let uid = rustix::process::geteuid().as_raw();
        #[cfg(not(unix))]
        let uid = 0;
        Self::for_user(uid, std::process::id())
    }

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
