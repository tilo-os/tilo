//! How one account row says what it is: the name and icon of its kind, the
//! status chip, the sync line under it, and the menu of everything else to do
//! with the account. The pane decides what an account holds and what may be
//! done with it; this decides how one row puts that.

use accounts::SyncHealth;
use accounts::person::AccountSync;

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

/// The line under an account's status about its syncs, when there is one to say: nothing for an
/// account nothing has synced or tried to yet, whose status already says where it stands.
pub(super) fn sync_detail(
    runtime: Option<&AccountSync>,
    import_only: bool,
    granted_mail: bool,
) -> Option<String> {
    if import_only && runtime.is_none_or(|sync| sync.message.is_empty()) {
        return Some(t("accounts-import-no-sync"));
    }
    let sync =
        runtime.filter(|sync| sync.health != SyncHealth::Never || sync.succeeded_ms.is_some());
    let Some(sync) = sync else {
        return granted_mail.then(|| t("accounts-mail-not-synced"));
    };
    let when = sync
        .succeeded_ms
        .map_or_else(|| t("accounts-sync-never"), relative_sync_time);
    Some(if sync.message.is_empty() {
        t_args("accounts-sync-when", &[("when", when.into())])
    } else {
        t_args(
            "accounts-sync-when-msg",
            &[
                ("when", when.as_str().into()),
                ("message", sync.message.as_str().into()),
            ],
        )
    })
}

pub(super) fn account_status(
    is_sample: bool,
    enabled: bool,
    runtime: Option<&AccountSync>,
) -> (String, StatusTone) {
    let health = runtime.map(|sync| sync.health);
    if is_sample {
        (t("accounts-status-example"), StatusTone::Neutral)
    } else if !enabled {
        (t("accounts-status-disabled"), StatusTone::Neutral)
    } else {
        match health {
            Some(SyncHealth::Syncing) => (t("accounts-status-syncing"), StatusTone::Info),
            Some(SyncHealth::Healthy) => (t("accounts-status-healthy"), StatusTone::Success),
            Some(SyncHealth::Offline) => (t("accounts-status-offline"), StatusTone::Warning),
            Some(SyncHealth::NeedsSignIn) => (t("accounts-status-reauth"), StatusTone::Warning),
            Some(SyncHealth::Error) => (t("accounts-status-error"), StatusTone::Warning),
            // A sign-in proved the account works; its first sync has not reported yet.
            Some(SyncHealth::Never) if runtime.is_some_and(|sync| sync.signed_in_ms.is_some()) => {
                (t("accounts-status-signed-in"), StatusTone::Success)
            }
            Some(SyncHealth::Never) | None => (t("accounts-status-never"), StatusTone::Neutral),
        }
    }
}

fn relative_sync_time(at_ms: u64) -> String {
    let seconds = ui::time::unix_now_ms().saturating_sub(at_ms) / 1000;
    ui::format::relative(i64::try_from(seconds).unwrap_or(i64::MAX))
}
