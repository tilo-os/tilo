//! How one account row says what it is: the name and icon of its kind, the
//! status chip, the sync line under it, and the menu of everything else to do
//! with the account.
//!
//! The pane decides what an account holds and what may be done with it; this
//! decides how one row puts that.

use accounts::{AccountSyncHealth, AccountSyncStatus};

use super::AccountsEffect;
use crate::accounts::AccountKind;
use crate::i18n::{t, t_args};
use provider_settings_ui::widgets::StatusTone;

pub(super) fn kind_shown(kind: AccountKind) -> String {
    t(match kind {
        AccountKind::Mail => "accounts-kind-mail",
        AccountKind::CalDav => "accounts-kind-caldav",
        AccountKind::WebDav => "accounts-kind-webdav",
        AccountKind::Nextcloud => "accounts-kind-nextcloud",
        AccountKind::GoogleDrive => "accounts-kind-google",
        AccountKind::OneDrive => "accounts-kind-onedrive",
        AccountKind::Dropbox => "accounts-kind-dropbox",
        AccountKind::ICloudDrive => "accounts-kind-icloud",
    })
}

/// One entry of an account's menu: what it says and the row effect it runs.
pub(super) struct AccountEntry {
    pub(super) label: String,
    pub(super) effect: AccountsEffect,
    pub(super) destructive: bool,
}

impl AccountEntry {
    pub(super) fn new(label: String, effect: AccountsEffect) -> Self {
        Self {
            label,
            effect,
            destructive: false,
        }
    }

    pub(super) fn destructive(label: String, effect: AccountsEffect) -> Self {
        Self {
            label,
            effect,
            destructive: true,
        }
    }
}

/// Sync now, and pause or resume, for a mounted file account.
pub(super) fn sync_entries(id: &str, paused: bool) -> [AccountEntry; 2] {
    let toggle = if paused {
        AccountEntry::new(
            t("accounts-resume-sync"),
            AccountsEffect::ResumeSync {
                account: id.to_owned(),
            },
        )
    } else {
        AccountEntry::new(
            t("accounts-pause-sync"),
            AccountsEffect::PauseSync {
                account: id.to_owned(),
            },
        )
    };
    [
        AccountEntry::new(
            t("accounts-sync-now"),
            AccountsEffect::SyncNow {
                account: id.to_owned(),
            },
        ),
        toggle,
    ]
}

/// The icon of an account's kind.
pub(super) fn kind_icon(kind: AccountKind) -> ui::icons::Icon {
    use ui::icons::Icon;
    match kind {
        AccountKind::Mail => Icon::Mail,
        AccountKind::CalDav => Icon::Calendar,
        AccountKind::WebDav => Icon::HardDrive,
        AccountKind::Nextcloud
        | AccountKind::GoogleDrive
        | AccountKind::OneDrive
        | AccountKind::Dropbox
        | AccountKind::ICloudDrive => Icon::Cloud,
    }
}

pub(super) fn sync_detail(
    runtime: Option<&AccountSyncStatus>,
    import_only: bool,
    granted_mail: bool,
) -> String {
    if import_only && runtime.is_none_or(|status| status.message.is_none()) {
        return t("accounts-import-no-sync");
    }
    let Some(status) = runtime else {
        return t(if granted_mail {
            "accounts-mail-not-synced"
        } else {
            "accounts-sync-none"
        });
    };
    let when = status
        .last_success_unix
        .map_or_else(|| t("accounts-sync-never"), relative_sync_time);
    match status.message.as_deref() {
        Some(message) => t_args(
            "accounts-sync-when-msg",
            &[("when", when.as_str().into()), ("message", message.into())],
        ),
        None => t_args("accounts-sync-when", &[("when", when.into())]),
    }
}

pub(super) fn account_status(
    is_sample: bool,
    enabled: bool,
    health: Option<AccountSyncHealth>,
) -> (String, StatusTone) {
    if is_sample {
        (t("accounts-status-example"), StatusTone::Neutral)
    } else if !enabled {
        (t("accounts-status-disabled"), StatusTone::Neutral)
    } else {
        match health {
            Some(AccountSyncHealth::Syncing) => (t("accounts-status-syncing"), StatusTone::Info),
            Some(AccountSyncHealth::Healthy) => (t("accounts-status-healthy"), StatusTone::Success),
            Some(AccountSyncHealth::Offline) => (t("accounts-status-offline"), StatusTone::Warning),
            Some(AccountSyncHealth::NeedsAuthentication) => {
                (t("accounts-status-reauth"), StatusTone::Warning)
            }
            Some(AccountSyncHealth::Error) => (t("accounts-status-error"), StatusTone::Warning),
            Some(AccountSyncHealth::Never) | None => {
                (t("accounts-status-never"), StatusTone::Neutral)
            }
        }
    }
}

fn relative_sync_time(unix: i64) -> String {
    ui::format::relative(ui::time::unix_now_i64().saturating_sub(unix))
}
