//! Retained notifications as the notification centre shows them: grouped by
//! verified app, newest first, with the words a screen reader hears for each.

use std::collections::{BTreeSet, HashMap};

use ui::time::Instant;

use super::model::{Notification, NotificationSeverity};
use crate::i18n::{t, t_args};

/// Identity of the group a retained notification belongs to. Only the
/// launcher-attributed app id (and, for a notification mirrored from
/// another device, the caption naming that device) is used: a
/// sender-controlled name never decides grouping. The desktop's own
/// notifications have no verified app and stay ungrouped.
pub fn group_key(notification: &Notification) -> Option<String> {
    let app_id = notification.app_id.as_deref()?;
    Some(match notification.app_name.as_deref() {
        Some(caption) => format!("{app_id}\u{1f}{caption}"),
        None => app_id.to_string(),
    })
}

/// Label painted and announced for a group: the verified display name.
pub fn group_label(notification: &Notification) -> String {
    notification
        .app_name
        .clone()
        .or_else(|| notification.app_id.clone())
        .unwrap_or_else(|| t("notification-group-system"))
}

/// One row of the notification centre, newest first.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryRow {
    /// Header for two or more notifications from the same verified app.
    Group {
        key: String,
        label: String,
        count: usize,
        expanded: bool,
        /// Index into the oldest-first history of the group's newest entry.
        newest: usize,
    },
    /// One retained notification; `index` is into the oldest-first history.
    Item { index: usize, grouped: bool },
}

impl HistoryRow {
    /// Stable identity of the row across re-derivations, used to keep the
    /// keyboard selection on the same row when history changes under it.
    pub fn identity(&self, history: &[Notification]) -> Option<RowIdentity> {
        match self {
            Self::Group { key, .. } => Some(RowIdentity::Group(key.clone())),
            Self::Item { index, .. } => history.get(*index).map(|n| RowIdentity::Item(n.id)),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum RowIdentity {
    Group(String),
    Item(u64),
}

/// Derive the centre's rows from the oldest-first history. Groups sort by
/// their newest member, so a fresh notification lifts its whole app to the
/// top; members follow their header newest first. An app with a single
/// retained notification shows that notification as a plain row.
pub fn history_rows(history: &[Notification], collapsed: &BTreeSet<String>) -> Vec<HistoryRow> {
    let mut members: HashMap<String, Vec<usize>> = HashMap::new();
    for (index, notification) in history.iter().enumerate() {
        if let Some(key) = group_key(notification) {
            members.entry(key).or_default().push(index);
        }
    }
    let mut rows = Vec::new();
    let mut emitted: BTreeSet<String> = BTreeSet::new();
    for (index, notification) in history.iter().enumerate().rev() {
        let Some(key) = group_key(notification) else {
            rows.push(HistoryRow::Item {
                index,
                grouped: false,
            });
            continue;
        };
        if emitted.contains(&key) {
            continue;
        }
        let group = members.get(&key).map_or(&[][..], Vec::as_slice);
        if group.len() < 2 {
            rows.push(HistoryRow::Item {
                index,
                grouped: false,
            });
            continue;
        }
        let expanded = !collapsed.contains(&key);
        rows.push(HistoryRow::Group {
            key: key.clone(),
            label: group_label(notification),
            count: group.len(),
            expanded,
            newest: index,
        });
        if expanded {
            rows.extend(group.iter().rev().map(|&member| HistoryRow::Item {
                index: member,
                grouped: true,
            }));
        }
        emitted.insert(key);
    }
    rows
}

/// What activating a retained notification asks the caller to do.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HistoryActivation {
    /// The default action was sent to the originating app or device.
    Invoked,
    /// The sender is gone (the entry was restored after a restart, or the
    /// host has no route back to it); open the verified app instead.
    OpenApp { app_id: String, label: String },
    /// Nothing can act on this entry; it stays in history.
    Unavailable,
    /// No retained notification carries that id.
    Missing,
}

/// How long ago `notification` arrived, as a short relative time.
pub fn relative_time(notification: &Notification, now: Instant) -> String {
    let seconds = now
        .saturating_duration_since(notification.created_at)
        .as_secs();
    ui::format::relative(i64::try_from(seconds).unwrap_or(i64::MAX))
}

/// The retained notification's source line: its verified app, or the
/// part of the system that asks. `None` when nothing names one: a generic
/// "System" would only repeat that the desktop showed it.
pub fn source_label(notification: &Notification) -> Option<String> {
    notification
        .app_name
        .as_deref()
        .or(notification.app_id.as_deref())
        .map(str::to_string)
}

fn severity_label(severity: NotificationSeverity) -> String {
    match severity {
        NotificationSeverity::Info => t("history-severity-info"),
        NotificationSeverity::Warning => t("history-severity-warning"),
        NotificationSeverity::Error => t("history-severity-error"),
    }
}

/// Screen-reader description for one retained notification row, from the
/// same source and age the painted row shows.
pub fn accessible_description(notification: &Notification, now: Instant) -> String {
    let severity = severity_label(notification.severity);
    let time = relative_time(notification, now);
    let mut args = vec![("severity", severity.into()), ("time", time.into())];
    let mut key = String::from("history-notification-description");
    if let Some(source) = source_label(notification) {
        args.push(("source", source.into()));
        key.push_str("-sourced");
    }
    if !notification.body.is_empty() {
        args.push(("body", notification.body.as_str().into()));
        key.push_str("-body");
    }
    t_args(&key, &args)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_app(app: &str, title: &str) -> Notification {
        let mut notification = Notification::new(title, "");
        notification.app_id = Some(app.to_string());
        notification.app_name = Some(app.to_uppercase());
        notification
    }

    #[test]
    fn apps_with_several_notifications_group_under_their_newest() {
        let history = vec![
            from_app("mail", "m1"),
            Notification::new("system", ""),
            from_app("chat", "c1"),
            from_app("mail", "m2"),
            from_app("calendar", "only"),
        ];
        let rows = history_rows(&history, &BTreeSet::new());
        assert_eq!(
            rows,
            vec![
                HistoryRow::Item {
                    index: 4,
                    grouped: false
                },
                HistoryRow::Group {
                    key: "mail\u{1f}MAIL".into(),
                    label: "MAIL".into(),
                    count: 2,
                    expanded: true,
                    newest: 3,
                },
                HistoryRow::Item {
                    index: 3,
                    grouped: true
                },
                HistoryRow::Item {
                    index: 0,
                    grouped: true
                },
                HistoryRow::Item {
                    index: 2,
                    grouped: false
                },
                HistoryRow::Item {
                    index: 1,
                    grouped: false
                },
            ]
        );
    }

    #[test]
    fn a_collapsed_group_keeps_only_its_header() {
        let history = vec![from_app("mail", "m1"), from_app("mail", "m2")];
        let collapsed = BTreeSet::from(["mail\u{1f}MAIL".to_string()]);
        let rows = history_rows(&history, &collapsed);
        assert!(matches!(
            rows.as_slice(),
            [HistoryRow::Group {
                expanded: false,
                count: 2,
                ..
            }]
        ));
    }

    #[test]
    fn a_mirrored_app_groups_apart_from_the_same_local_app() {
        let local = from_app("mail", "here");
        let mut remote = from_app("mail", "there");
        remote.app_name = Some("MAIL · From Phone".into());
        assert_ne!(group_key(&local), group_key(&remote));
    }
}
