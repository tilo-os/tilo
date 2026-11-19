//! What the rows a Files window publishes say their effects do now: the words
//! a file is opened, read, kept or sent by, the apps that open it, the devices
//! nearby, and a transfer's phase with the destinations it can be saved to.

use std::path::Path;

use ui::framework::EffectStatus;
use ui::tilo_actions::Availability;

use super::{
    FilesState, RETRY_ROW, nearby_action_locator, remote_folder_availability, template_resource_id,
    templates,
};
use crate::effects::FilesEffect;
use crate::i18n::{arg, t, t_args};

/// What the retry row's one action says now: trying the folder again, or
/// opening the folder it is in.
pub(super) fn retry_row_status(may_pass: bool) -> Vec<EffectStatus> {
    let (effect, label) = if may_pass {
        (FilesEffect::REFRESH, t("files-try-again"))
    } else {
        (FilesEffect::UP, t("files-open-enclosing"))
    };
    vec![EffectStatus::labelled(effect, label).on(RETRY_ROW)]
}

/// What a template's row says its one action does: make a file of that
/// template, named for it.
pub(super) fn template_row_status(template: &Path) -> Vec<EffectStatus> {
    let template_id = template_resource_id(&template.to_string_lossy());
    vec![
        EffectStatus::labelled(
            FilesEffect::NEW_FROM_TEMPLATE,
            templates::menu_label(template),
        )
        .on(template_id),
    ]
}

impl FilesState {
    /// What the row `id` says its effects do now, where its state says more
    /// than their declaration: the words a file is opened or read by, the
    /// apps that open it, the devices nearby, and a transfer's phase.
    pub(crate) fn row_status(&self, id: &str) -> Vec<EffectStatus> {
        if let Some(entry) = self.entry_by_resource_id(id) {
            let context = self.row_context();
            let facts = self.entry_row_facts(&context, entry);
            return super::entry_rows::entry_row_status(&context, &facts, entry);
        }
        if id == RETRY_ROW {
            return self
                .folder_failure()
                .map(|(failure, _)| retry_row_status(failure.may_pass()))
                .unwrap_or_default();
        }
        if id.starts_with("files.template:") {
            return self
                .available_templates()
                .iter()
                .find(|template| template_resource_id(&template.to_string_lossy()) == id)
                .map(|template| template_row_status(template))
                .unwrap_or_default();
        }
        if id.starts_with("nearby-transfer:") {
            return self
                .nearby_send
                .transfers()
                .iter()
                .find(|transfer| {
                    format!(
                        "nearby-transfer:{}",
                        nearby_action_locator("transfer", transfer.handle.as_bytes())
                    ) == id
                })
                .map(|transfer| self.nearby_transfer_status(transfer))
                .unwrap_or_default();
        }
        if id.starts_with("files.remote-account:") {
            return self
                .remote_account_snapshot()
                .into_iter()
                .find(|account| crate::remote_accounts::resource_id(&account.id) == id)
                .map(|account| remote_account_status(&account))
                .unwrap_or_default();
        }
        Vec::new()
    }

    /// Whether `transfer` waits on the person to take or decline it here.
    pub(super) fn nearby_deciding(transfer: &ui::collaboration::NearbyTransferStatus) -> bool {
        use ui::collaboration::{NearbyTransferDirection, NearbyTransferPhase};

        transfer.direction == NearbyTransferDirection::Receiving
            && matches!(
                transfer.phase,
                NearbyTransferPhase::AwaitingConsent | NearbyTransferPhase::AwaitingDestination
            )
    }

    /// What a transfer's row says its actions do: the status it is in, the
    /// destinations it can be saved to, and retrying, cancelling or
    /// declining it.
    pub(crate) fn nearby_transfer_status(
        &self,
        transfer: &ui::collaboration::NearbyTransferStatus,
    ) -> Vec<EffectStatus> {
        use ui::collaboration::NearbyTransferPhase;

        let resource_id = format!(
            "nearby-transfer:{}",
            nearby_action_locator("transfer", transfer.handle.as_bytes())
        );
        let id = resource_id.as_str();
        let mut statuses = vec![
            EffectStatus::labelled(FilesEffect::STATUS, t("files-nearby-status"))
                .with_description(t("files-nearby-status-detail"))
                .on(id),
        ];
        if Self::nearby_deciding(transfer) {
            let choices: Vec<(String, String)> = self
                .nearby_send
                .destinations()
                .iter()
                .map(|destination| {
                    (
                        hex::encode(destination.handle.as_bytes()),
                        t_args(
                            "files-nearby-save-to-destination",
                            &[arg("destination", destination.label.clone())],
                        ),
                    )
                })
                .collect();
            statuses.push(
                EffectStatus::labelled(
                    FilesEffect::ACCEPT_NEARBY,
                    t("files-effects-accept-nearby"),
                )
                .with_labelled_param_choices("destination", choices)
                .on(id),
            );
        }
        if transfer.phase == NearbyTransferPhase::Failed {
            statuses
                .push(EffectStatus::labelled(FilesEffect::RETRY, t("files-nearby-retry")).on(id));
        }
        if matches!(
            transfer.phase,
            NearbyTransferPhase::Queued | NearbyTransferPhase::Transferring
        ) {
            statuses.push(
                EffectStatus::labelled(FilesEffect::CANCEL, t("files-effects-cancel-nearby"))
                    .on(id),
            );
        }
        statuses
    }
}

/// What an account's row says its actions do: opening its files, its status
/// and how to sign in again — and whether it can be opened at all.
fn remote_account_status(account: &crate::remote_accounts::RemoteAccount) -> Vec<EffectStatus> {
    let id = crate::remote_accounts::resource_id(&account.id);
    let id = id.as_str();
    let open = match remote_folder_availability(account) {
        Availability::Available => EffectStatus::labelled(
            FilesEffect::OPEN_ACCOUNT,
            crate::i18n::CATALOG.t("files-shortcuts-open"),
        ),
        Availability::Unavailable { code, reason } => {
            EffectStatus::unavailable(FilesEffect::OPEN_ACCOUNT, code, reason)
        }
    };
    vec![
        open.on(id),
        EffectStatus::labelled(
            FilesEffect::DETAILS,
            crate::i18n::CATALOG.t("files-account-status"),
        )
        .on(id),
        EffectStatus::labelled(
            FilesEffect::REAUTHENTICATE,
            crate::i18n::CATALOG.t("files-account-reauthenticate"),
        )
        .on(id),
    ]
}
