//! Telling the person, once, that the last start of the computer failed, or
//! that it started the previous version because the newer one did not start.
//!
//! The boot-failure screen (`/usr/libexec/tilo-boot-failure`) leaves a record
//! of a failed start where it survives the restart; on the next boot
//! `tilo-boot-failure import` turns the newest one into [`NOTICE`]: `id=`,
//! `fallback=`, and the record's `step=`, `unit=`, `log=` lines. A notice is
//! shown once per id: the ids shown are kept in the desktop's own state.

use std::path::{Path, PathBuf};
use std::sync::Mutex;

use tilo_desktop::notifications::{Notification, NotificationSeverity, PERSISTENT_DISMISS};
use tilo_lock::MutexRecover as _;

use crate::shell::i18n::{t, t_args};

/// The notice `tilo-boot-failure import` writes.
const NOTICE: &str = "/var/lib/tilo/boot-failure/notice";

/// The action that shows what failed, on the desktop's own notice only.
pub(crate) const DETAILS_ACTION: &str = "boot-failure-details";

static QUEUE: Mutex<Vec<Notification>> = Mutex::new(Vec::new());
static DETAILS: Mutex<Option<Notification>> = Mutex::new(None);

#[derive(Debug, Default, PartialEq)]
struct Notice {
    id: String,
    fallback: bool,
    step: Option<String>,
    unit: Option<String>,
    log: Vec<String>,
}

fn parse(text: &str) -> Option<Notice> {
    let mut notice = Notice::default();
    for line in text.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        match key {
            "id" => value.clone_into(&mut notice.id),
            "fallback" => notice.fallback = value == "1",
            "step" if !value.is_empty() => notice.step = Some(value.to_owned()),
            "unit" if !value.is_empty() => notice.unit = Some(value.to_owned()),
            "log" => notice.log.push(value.to_owned()),
            _ => {}
        }
    }
    (!notice.id.is_empty()).then_some(notice)
}

fn step_words(step: Option<&str>) -> String {
    match step {
        Some("unlock") => t("boot-failure-step-unlock"),
        Some("mount") => t("boot-failure-step-mount"),
        Some("desktop") => t("boot-failure-step-desktop"),
        _ => t("boot-failure-step-prepare"),
    }
}

/// The notice, and what its Details action shows, when there is a record.
fn notifications(notice: &Notice) -> (Notification, Option<Notification>) {
    let has_record = notice.step.is_some() || notice.unit.is_some();
    let mut shown = if notice.fallback {
        Notification::new(
            t("boot-failure-fallback-title"),
            t("boot-failure-fallback-body"),
        )
    } else {
        Notification::new(
            t("boot-failure-notice-title"),
            step_words(notice.step.as_deref()),
        )
    }
    .with_severity(NotificationSeverity::Warning)
    .with_app_name(t("boot-failure-app-name"))
    .with_dismiss_after(PERSISTENT_DISMISS);
    if !has_record {
        return (shown, None);
    }
    shown.actions = vec![(DETAILS_ACTION.to_owned(), t("boot-failure-details"))];
    let mut body = vec![step_words(notice.step.as_deref())];
    if let Some(unit) = &notice.unit {
        body.push(t_args(
            "boot-failure-details-unit",
            &[("unit", unit.as_str().into())],
        ));
    }
    body.extend(notice.log.iter().cloned());
    body.push(t_args(
        "boot-failure-details-start",
        &[("start", notice.id.as_str().into())],
    ));
    let details = Notification::new(t("boot-failure-details-title"), body.join("\n"))
        .with_severity(NotificationSeverity::Info)
        .with_app_name(t("boot-failure-app-name"))
        .with_dismiss_after(PERSISTENT_DISMISS);
    (shown, Some(details))
}

fn shown_path() -> Option<PathBuf> {
    tilo_paths::env_dir("XDG_STATE_HOME").map(|state| state.join("tilo/boot-failure-shown"))
}

/// Queue the notice of the last failed start if it has not been shown yet.
/// The compositor calls this once, at startup.
pub fn look() {
    let notice_path = std::env::var_os("TILO_BOOT_FAILURE_NOTICE")
        .map_or_else(|| PathBuf::from(NOTICE), PathBuf::from);
    look_at(&notice_path, shown_path().as_deref());
}

fn look_at(notice_path: &Path, shown: Option<&Path>) {
    let Some(notice) = std::fs::read_to_string(notice_path)
        .ok()
        .as_deref()
        .and_then(parse)
    else {
        return;
    };
    let Some(shown) = shown else {
        return;
    };
    let seen = std::fs::read_to_string(shown).unwrap_or_default();
    if seen.lines().any(|id| id == notice.id) {
        return;
    }
    // Recorded before it is shown: a desktop that crashes while showing it
    // must not show it again at every start.
    let mut ids: Vec<&str> = seen.lines().filter(|id| !id.is_empty()).collect();
    ids.push(&notice.id);
    let kept: Vec<&str> = ids.iter().rev().take(16).rev().copied().collect();
    let written = shown
        .parent()
        .map_or(Ok(()), std::fs::create_dir_all)
        .and_then(|()| std::fs::write(shown, format!("{}\n", kept.join("\n"))));
    if let Err(error) = written {
        tracing::warn!(%error, "could not note the boot-failure notice as shown");
        return;
    }
    let (shown_notice, details) = notifications(&notice);
    *DETAILS.lock_recover() = details;
    QUEUE.lock_recover().push(shown_notice);
}

/// Notices queued since the last call.
pub fn drain() -> Vec<Notification> {
    std::mem::take(&mut *QUEUE.lock_recover())
}

/// What the Details action of the notice shows.
pub(crate) fn details() -> Option<Notification> {
    DETAILS.lock_recover().clone()
}

#[cfg(test)]
mod tests {
    use super::*;

    const RECORD: &str = "id=4f2a9c1e0b7d\nfallback=0\ntime=1792000000\nstep=mount\n\
                          unit=sysroot.mount\nentry=bootc_fedora-44-2.conf\n\
                          log=mount: /sysroot: wrong fs type\n";

    #[test]
    fn a_record_parses_into_a_notice() {
        let notice = parse(RECORD).expect("notice");
        assert_eq!(notice.id, "4f2a9c1e0b7d");
        assert!(!notice.fallback);
        assert_eq!(notice.step.as_deref(), Some("mount"));
        assert_eq!(notice.unit.as_deref(), Some("sysroot.mount"));
        assert_eq!(notice.log, ["mount: /sysroot: wrong fs type"]);
        assert_eq!(
            parse("fallback=1\n"),
            None,
            "a notice without an id is no notice"
        );
    }

    #[test]
    fn the_notice_names_the_step_and_offers_details() {
        let (shown, details) = notifications(&parse(RECORD).expect("notice"));
        assert_eq!(shown.title, t("boot-failure-notice-title"));
        assert_eq!(shown.body, t("boot-failure-step-mount"));
        assert_eq!(shown.actions[0].0, DETAILS_ACTION);
        let details = details.expect("details");
        assert!(details.body.contains("sysroot.mount"));
        assert!(details.body.contains("wrong fs type"));
        assert!(details.body.contains("4f2a9c1e0b7d"));
    }

    #[test]
    fn a_fallback_without_a_record_says_so_and_has_no_details() {
        let (shown, details) =
            notifications(&parse("id=fallback-ab12\nfallback=1\n").expect("notice"));
        assert_eq!(shown.title, t("boot-failure-fallback-title"));
        assert!(shown.actions.is_empty());
        assert!(details.is_none());
    }

    #[test]
    fn a_notice_is_shown_once() {
        let dir = tempfile::tempdir().expect("tempdir");
        let notice = dir.path().join("notice");
        let shown = dir.path().join("state/tilo/boot-failure-shown");
        std::fs::write(&notice, RECORD).expect("notice");
        let _ = drain();
        look_at(&notice, Some(&shown));
        assert_eq!(drain().len(), 1);
        look_at(&notice, Some(&shown));
        assert!(drain().is_empty(), "the same id again is not shown again");
        std::fs::write(&notice, RECORD.replace("4f2a9c1e0b7d", "77aa00bb11cc")).expect("notice");
        look_at(&notice, Some(&shown));
        assert_eq!(drain().len(), 1, "a new failure is a new notice");
    }
}
