//! The proof that a foreign notification sender really is the installed
//! Flatpak app it claims: the per-process `.flatpak-info` and the systemd
//! cgroup are read and checked against an installed desktop entry, and
//! nothing else about a sender's argv or `app_name` is believed.

use std::fmt::Write as _;
use std::path::Path;

/// The installed Flatpak `pid` demonstrably runs in.
///
/// Plain host processes are deliberately not guessed from their argv or the
/// `Notify.app_name` field: the session's [`crate::Callers`] names the apps
/// it launched; anything else stays unattributed. Flatpak provides two
/// independent pieces of immutable runtime metadata — its per-process
/// `.flatpak-info` and systemd cgroup — which are checked against an
/// installed desktop entry before the id is believed.
#[must_use]
pub fn verified_flatpak_app_id(pid: u32) -> Option<String> {
    let entries = &desktop_entry::cache::cached_scan().entries;
    verified_flatpak_app_id_from(Path::new("/proc"), pid, entries)
}

pub(crate) fn verified_flatpak_app_id_from(
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

fn read_bounded(path: &Path, max_bytes: u64) -> Option<String> {
    let metadata = std::fs::metadata(path).ok()?;
    (metadata.len() <= max_bytes).then(|| std::fs::read_to_string(path).ok())?
}

pub(crate) fn valid_app_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 160
        && id
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'-' | b'_'))
}

fn flatpak_application_id(info_path: &Path) -> Option<String> {
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
            return valid_app_id(id).then(|| id.to_string());
        }
    }
    None
}

#[allow(
    clippy::case_sensitive_file_extension_comparisons,
    reason = "Systemd scope unit names use the literal lowercase `.scope` suffix; accepting a differently cased unit would not identify a Flatpak application scope."
)]
fn flatpak_cgroup_matches(cgroup: &str, app_id: &str) -> bool {
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

fn systemd_escape(value: &str) -> String {
    value.bytes().fold(String::new(), |mut out, byte| {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_') {
            out.push(char::from(byte));
        } else {
            let _ = write!(out, "\\x{byte:02x}");
        }
        out
    })
}

#[cfg(test)]
#[allow(
    clippy::expect_used,
    reason = "a fixture that cannot be written fails the test"
)]
mod tests {
    use super::*;

    fn flatpak_entries(root: &Path, app_id: &str) -> Vec<desktop_entry::DesktopEntry> {
        let apps = root.join("applications");
        std::fs::create_dir_all(&apps).expect("applications");
        std::fs::write(
            apps.join(format!("{app_id}.desktop")),
            format!(
                "[Desktop Entry]\nType=Application\nName=Trusted Chat\nExec=/bin/true\nX-Flatpak={app_id}\n"
            ),
        )
        .expect("desktop entry");
        desktop_entry::scan_dirs(&[apps], &[desktop_entry::scan::CURRENT_DESKTOP.to_string()])
            .entries
    }

    fn write_flatpak_proc(root: &Path, pid: u32, app_id: &str, cgroup_app_id: &str) {
        let proc = root.join(pid.to_string());
        std::fs::create_dir_all(proc.join("root")).expect("proc root");
        std::fs::write(
            proc.join("root/.flatpak-info"),
            format!("[Application]\nname={app_id}\n"),
        )
        .expect("flatpak-info");
        std::fs::write(
            proc.join("cgroup"),
            format!(
                "0::/user.slice/user-1000.slice/user@1000.service/app.slice/app-flatpak-{cgroup_app_id}-123.scope\n"
            ),
        )
        .expect("cgroup");
    }

    #[test]
    fn flatpak_cgroup_matching_uses_the_systemd_escaped_scope() {
        assert_eq!(systemd_escape("org.example.App"), "org.example.App");
        assert_eq!(systemd_escape("org.example App"), "org.example\\x20App");
        assert!(flatpak_cgroup_matches(
            "0::/app.slice/app-flatpak-org.example\\x20App-42.scope",
            "org.example App"
        ));
        assert!(!flatpak_cgroup_matches(
            "0::/app.slice/app-flatpak-org.example\\x20App-42.SCOPE",
            "org.example App"
        ));
    }

    #[test]
    fn flatpak_identity_requires_matching_scope_runtime_and_install() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let app_id = "org.example.Chat";
        let entries = flatpak_entries(tmp.path(), app_id);
        write_flatpak_proc(tmp.path(), 404, app_id, app_id);

        assert_eq!(
            verified_flatpak_app_id_from(tmp.path(), 404, &entries).as_deref(),
            Some(app_id),
            "identity comes from runtime metadata/cgroup, not Notify.app_name"
        );

        write_flatpak_proc(tmp.path(), 405, app_id, "org.example.Spoof");
        assert_eq!(
            verified_flatpak_app_id_from(tmp.path(), 405, &entries),
            None,
            "a mismatched cgroup leaves a foreign sender unattributed"
        );

        write_flatpak_proc(
            tmp.path(),
            406,
            "org.example.Missing",
            "org.example.Missing",
        );
        assert_eq!(
            verified_flatpak_app_id_from(tmp.path(), 406, &entries),
            None,
            "an uninstalled Flatpak id is never accepted"
        );
    }

    #[test]
    fn app_ids_are_bounded_reverse_dns_characters() {
        assert!(valid_app_id("org.example.Chat"));
        assert!(!valid_app_id(""));
        assert!(!valid_app_id("org/example"));
        assert!(!valid_app_id(&"a".repeat(161)));
    }
}
