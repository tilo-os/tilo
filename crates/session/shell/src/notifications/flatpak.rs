//! The proof that a foreign notification sender really is the installed Flatpak
//! app it claims: the per-process `.flatpak-info` and the systemd cgroup are
//! read and checked against an installed desktop entry, and nothing else about
//! a sender's argv or `app_name` is believed.

use std::path::Path;

/// Return a policy identity for a foreign desktop notification sender only
/// when the process is demonstrably inside an installed Flatpak sandbox.
///
/// Plain RPM/host processes are deliberately not guessed from their argv or
/// the `Notify.app_name` field: when Tilo launched one, the trusted launcher
/// ancestry above supplies its desktop id; otherwise it remains global-policy
/// only. Flatpak provides two independent pieces of immutable runtime
/// metadata — its per-process `.flatpak-info` and systemd cgroup — which are
/// checked against an installed desktop entry before a per-app rule is used.
#[cfg(target_os = "linux")]
pub(super) fn verified_foreign_notification_identity(pid: u32) -> Option<String> {
    verified_flatpak_app_id(pid)
}

/// The installed Flatpak `pid` demonstrably runs in, by the proof described
/// at [`verified_foreign_notification_identity`]. The tray uses it to tell
/// which app keeps an icon there.
#[cfg(target_os = "linux")]
pub(crate) fn verified_flatpak_app_id(pid: u32) -> Option<String> {
    let entries = &desktop_entry::cache::cached_scan().entries;
    verified_foreign_notification_identity_from(Path::new("/proc"), pid, entries)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn verified_foreign_notification_identity_from(
    proc_root: &Path,
    pid: u32,
    entries: &[desktop_entry::DesktopEntry],
) -> Option<String> {
    let proc = proc_root.join(pid.to_string());
    let app_id = flatpak_application_id(&proc.join("root/.flatpak-info"))?;
    let cgroup = read_bounded(&proc.join("cgroup"), 16 * 1024)?;
    if !flatpak_cgroup_matches(&cgroup, &app_id) {
        return None;
    }
    entries
        .iter()
        .any(|entry| {
            matches!(
                &entry.source,
                desktop_entry::EntrySource::Flatpak { app_id: installed } if installed == &app_id
            )
        })
        .then_some(app_id)
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn read_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    (metadata.len() <= max_bytes).then(|| std::fs::read_to_string(path).ok())?
}

pub(super) fn valid_notification_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn flatpak_application_id(info_path: &Path) -> Option<String> {
    let info = read_bounded(info_path, 16 * 1024)?;
    let mut application_section = false;
    for line in info.lines() {
        let line = line.trim();
        if line.starts_with('[') && line.ends_with(']') {
            application_section = line == "[Application]";
            continue;
        }
        if application_section && let Some(id) = line.strip_prefix("name=") {
            let id = id.trim();
            return valid_notification_app_id(id).then(|| id.to_string());
        }
    }
    None
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Systemd scope unit names use the literal lowercase `.scope` suffix; accepting a differently cased unit would not identify a Flatpak application scope."
)]
#[cfg(any(test, target_os = "linux"))]
pub(super) fn flatpak_cgroup_matches(cgroup: &str, app_id: &str) -> bool {
    let raw_prefix = format!("app-flatpak-{app_id}");
    let escaped_prefix = format!("app-flatpak-{}", systemd_escape(app_id));
    cgroup.lines().any(|line| {
        let Some((_, path)) = line.rsplit_once(':') else {
            return false;
        };
        path.split('/').any(|component| {
            [raw_prefix.as_str(), escaped_prefix.as_str()]
                .iter()
                .any(|prefix| {
                    component.strip_prefix(prefix).is_some_and(|suffix| {
                        suffix == ".scope"
                            || (suffix.starts_with('-') && suffix.ends_with(".scope"))
                    })
                })
        })
    })
}

#[cfg(any(test, target_os = "linux"))]
pub(super) fn systemd_escape(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_') {
            out.push(char::from(byte));
        } else {
            out.push_str("\\x");
            out.push_str(&hex::encode([byte]));
        }
        out
    })
}
