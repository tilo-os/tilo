//! Desktop notifications for the opt-in media codecs pack.
//!
//! The pack installs in the background: setup continues while it downloads,
//! and an opt-in made offline installs once the device is online (the
//! helper's `NetworkManager` hook starts it). Whoever asked may have moved on,
//! so the result arrives as a notification. The shell follows the helper's
//! status file ([`os_actions::codec_pack::watch`], inotify, nothing polls) and
//! queues a notice for each transition worth telling; the compositor loop
//! drains the queue into the notification centre.

use std::sync::{Mutex, OnceLock};

use os_actions::codec_pack::{CodecPackStatus, MesaState, PackState};

use crate::shell::i18n::{t, t_args};
use tilo_desktop::notifications::{Notification, NotificationSeverity};

/// One notification to show.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CodecNotice {
    pub title: String,
    pub body: String,
    pub severity: NotificationSeverity,
}

/// What, if anything, the person should be told when the status moves from
/// `before` to `after`.
#[must_use]
pub fn notice_for(before: &CodecPackStatus, after: &CodecPackStatus) -> Option<CodecNotice> {
    let applies = || after.when_it_applies().unwrap_or_default().to_string();
    match (before.state, after.state) {
        (PackState::Installed, PackState::Installed) => {
            // A background refresh found the Mesa build matching the image.
            (before.mesa != MesaState::Enabled && after.mesa == MesaState::Enabled).then(|| {
                CodecNotice {
                    title: t("codec-hardware-ready"),
                    body: applies(),
                    severity: NotificationSeverity::Info,
                }
            })
        }
        (_, PackState::Installed) => Some(CodecNotice {
            title: t("codec-installed"),
            body: if after.mesa == MesaState::Waiting {
                t_args(
                    "codec-installed-mesa-waiting",
                    &[("applies", applies().into())],
                )
            } else {
                applies()
            },
            severity: NotificationSeverity::Info,
        }),
        (PackState::Installing | PackState::WaitingForNetwork, PackState::Failed) => {
            Some(CodecNotice {
                title: t("codec-failed"),
                body: if after.last_error.is_empty() {
                    t("codec-failed-retry")
                } else {
                    t_args(
                        "codec-failed-with-error",
                        &[("error", after.last_error.as_str().into())],
                    )
                },
                severity: NotificationSeverity::Error,
            })
        }
        _ => None,
    }
}

struct Monitor {
    last: CodecPackStatus,
    queue: Vec<CodecNotice>,
}

fn monitor() -> Option<&'static Mutex<Monitor>> {
    static MONITOR: OnceLock<Option<Mutex<Monitor>>> = OnceLock::new();
    MONITOR
        .get_or_init(|| {
            if !os_actions::codec_pack::available() {
                return None;
            }
            let started = os_actions::codec_pack::watch(|| {
                if let Some(monitor) = monitor() {
                    observe(monitor, CodecPackStatus::read());
                }
                true
            });
            if let Err(error) = started {
                tracing::debug!(%error, "codec pack notifications are unavailable");
                return None;
            }
            Some(Mutex::new(Monitor {
                last: CodecPackStatus::read(),
                queue: Vec::new(),
            }))
        })
        .as_ref()
}

fn observe(monitor: &Mutex<Monitor>, status: CodecPackStatus) {
    let Ok(mut monitor) = monitor.lock() else {
        return;
    };
    if let Some(notice) = notice_for(&monitor.last, &status) {
        monitor.queue.push(notice);
    }
    monitor.last = status;
}

/// Notices since the last call, as notifications. Starts following the
/// status file on first use; empty on systems without the pack.
pub fn drain() -> Vec<Notification> {
    let Some(monitor) = monitor() else {
        return Vec::new();
    };
    let Ok(mut monitor) = monitor.lock() else {
        return Vec::new();
    };
    std::mem::take(&mut monitor.queue)
        .into_iter()
        .map(|notice| {
            Notification::new(notice.title, notice.body)
                .with_severity(notice.severity)
                .with_app_name(t("codec-app-name"))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(json: &str) -> CodecPackStatus {
        CodecPackStatus::parse(json)
    }

    #[test]
    fn a_deferred_install_that_finishes_online_is_announced() {
        let notice = notice_for(
            &status(r#"{"state":"waiting-for-network","desired":true}"#),
            &status(r#"{"state":"installed","mesa":"enabled","desired":true}"#),
        )
        .unwrap();
        assert_eq!(notice.title, "Media codecs installed");
        assert!(
            notice.body.contains("sign out and back in"),
            "{}",
            notice.body
        );
        assert_eq!(notice.severity, NotificationSeverity::Info);
    }

    #[test]
    fn software_only_install_says_no_restart_and_that_hardware_follows() {
        let notice = notice_for(
            &status(r#"{"state":"installing"}"#),
            &status(r#"{"state":"installed","mesa":"waiting"}"#),
        )
        .unwrap();
        assert!(notice.body.contains("No restart is needed"));
        assert!(notice.body.contains("Hardware H.264/HEVC follows"));
    }

    #[test]
    fn a_later_matching_mesa_build_is_announced_once() {
        let before = status(r#"{"state":"installed","mesa":"waiting"}"#);
        let after = status(r#"{"state":"installed","mesa":"enabled"}"#);
        assert_eq!(
            notice_for(&before, &after).unwrap().title,
            "Hardware H.264/HEVC is ready"
        );
        assert_eq!(
            notice_for(&after, &after),
            None,
            "routine refreshes are quiet"
        );
    }

    #[test]
    fn failures_point_to_settings_and_progress_is_quiet() {
        let failed = notice_for(
            &status(r#"{"state":"installing"}"#),
            &status(r#"{"state":"failed","last_error":"signature check failed"}"#),
        )
        .unwrap();
        assert_eq!(failed.severity, NotificationSeverity::Error);
        assert!(failed.body.starts_with("signature check failed"));
        assert!(failed.body.contains("Settings, Updates"));
        assert_eq!(
            notice_for(
                &status(r#"{"state":"not-installed"}"#),
                &status(r#"{"state":"installing"}"#)
            ),
            None
        );
        assert_eq!(
            notice_for(
                &status(r#"{"state":"installed"}"#),
                &status(r#"{"state":"not-installed"}"#)
            ),
            None,
            "removing is the owner's own action"
        );
    }

    #[test]
    fn observed_transitions_are_queued_in_order() {
        let monitor = Mutex::new(Monitor {
            last: status(r#"{"state":"not-installed"}"#),
            queue: Vec::new(),
        });
        observe(
            &monitor,
            status(r#"{"state":"waiting-for-network","desired":true}"#),
        );
        observe(&monitor, status(r#"{"state":"installing","desired":true}"#));
        observe(
            &monitor,
            status(r#"{"state":"installed","mesa":"enabled"}"#),
        );
        let queue = &monitor.lock().unwrap().queue;
        assert_eq!(queue.len(), 1);
        assert_eq!(queue[0].title, "Media codecs installed");
    }
}
