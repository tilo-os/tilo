//! Who the app runs as and on what: the login's account and host names, and
//! whether a desktop session is there to show capture and call UI.
//!
//! Apps use these only as hints (a display name to start from, a seed for a
//! deterministic fallback id); none of them is an identity to trust. The web
//! has none of them.

/// The login name of the account running this process: `$USER`, else
/// `$USERNAME` (Windows), when non-empty. `None` in a page.
#[must_use]
pub fn user_name() -> Option<String> {
    imp::user_name()
}

/// This machine's host name: `$HOSTNAME`, else the first line of
/// `/etc/hostname`, when non-empty. `None` in a page.
#[must_use]
pub fn host_name() -> Option<String> {
    imp::host_name()
}

/// Whether the process runs inside an interactive desktop session with a
/// session bus or a Wayland display, which live capture, calls and screen
/// sharing need. Headless runs (CI, `--shots`) and pages have neither.
#[must_use]
pub fn has_desktop_session() -> bool {
    imp::has_desktop_session()
}

#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod imp {
    pub fn non_empty(value: Option<String>) -> Option<String> {
        value
            .map(|value| value.trim().to_owned())
            .filter(|value| !value.is_empty())
    }

    fn var(name: &str) -> Option<String> {
        crate::var(name).and_then(|value| value.into_string().ok())
    }

    pub fn user_name() -> Option<String> {
        non_empty(var("USER")).or_else(|| non_empty(var("USERNAME")))
    }

    pub fn host_name() -> Option<String> {
        non_empty(var("HOSTNAME")).or_else(|| {
            let text = std::fs::read_to_string("/etc/hostname").ok()?;
            non_empty(text.lines().next().map(str::to_owned))
        })
    }

    pub fn has_desktop_session() -> bool {
        session_bus_present() || var("WAYLAND_DISPLAY").is_some()
    }

    /// A session bus address that names a socket which exists. The test
    /// environment points the address at one that never does.
    fn session_bus_present() -> bool {
        let Some(address) = var("DBUS_SESSION_BUS_ADDRESS") else {
            return false;
        };
        address
            .split(';')
            .any(|entry| match entry.strip_prefix("unix:path=") {
                Some(rest) => {
                    let path = rest.split(',').next().unwrap_or_default();
                    std::path::Path::new(path).exists()
                }
                None => !entry.is_empty(),
            })
    }
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
mod imp {
    pub const fn user_name() -> Option<String> {
        None
    }

    pub const fn host_name() -> Option<String> {
        None
    }

    pub const fn has_desktop_session() -> bool {
        false
    }
}

#[cfg(test)]
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
mod tests {
    use super::imp::non_empty;
    use super::*;

    #[test]
    fn blank_values_name_nothing() {
        assert_eq!(non_empty(Some("  ".into())), None);
        assert_eq!(non_empty(None), None);
        assert_eq!(non_empty(Some(" ada \n".into())), Some("ada".into()));
    }

    #[test]
    fn a_scoped_profile_keeps_the_session_answers_consistent() {
        // The names come from the process environment either way; they only
        // have to be non-blank when present.
        let _scope = crate::scope_profile(std::env::temp_dir());
        assert!(user_name().is_none_or(|name| !name.is_empty()));
        assert!(host_name().is_none_or(|name| !name.is_empty()));
    }
}
