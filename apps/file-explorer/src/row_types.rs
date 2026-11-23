//! The types the window's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource, and it offers the effects its type declares on it.

use ui::tilo_resource::{self, Effect as _};

/// A failed folder's row, offering one of the window's own effects: trying
/// the folder again, or opening the folder it is in.
pub(crate) struct Retry;

impl ui::tilo_resource::words::RowType for Retry {
    const KIND: &'static str = "files.retry";
    const LABEL: &'static str = "Retry";

    fn effects_with(
        _id: &tilo_resource::Id,
        bindings: &tilo_resource::RowBindings,
    ) -> Vec<ui::tilo_actions::ActionSpec> {
        [
            ("refresh", crate::effects::FilesEffect::Refresh),
            ("up", crate::effects::FilesEffect::Up),
        ]
        .into_iter()
        .filter(|(id, _)| bindings.arguments(id).is_some())
        .map(|(_, effect)| effect.bound())
        .collect()
    }
}

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.place", label = "Place", effects = crate::effects::FilesEffect)]
pub(crate) struct Place;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.template", label = "Template", effects = crate::effects::FilesEffect)]
pub(crate) struct Template;

#[derive(ui::tilo_resource::Row)]
#[row(
    kind = "files.nearby-transfer",
    label = "Nearby transfer",
    effects = crate::effects::FilesEffect
)]
pub(crate) struct NearbyTransfer;

#[derive(ui::tilo_resource::Row)]
#[row(
    kind = "files.remote-account",
    label = "Remote account",
    effects = crate::effects::FilesEffect
)]
pub(crate) struct RemoteAccount;

#[derive(ui::tilo_resource::Row)]
#[row(
    kind = "files.cloud-entry",
    label = "Cloud entry",
    effects = crate::effects::FilesEffect
)]
pub(crate) struct CloudEntry;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.drive", label = "Drive", effects = crate::effects::FilesEffect)]
pub(crate) struct Drive;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.record", label = "Document", effects = crate::effects::FilesEffect)]
pub(crate) struct Document;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.entry", label = "File", effects = crate::effects::FilesEffect)]
pub(crate) struct Entry;
