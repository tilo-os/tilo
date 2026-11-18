//! The types the window's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource.

use ui::tilo_resource::words::RowType;

pub(crate) struct Retry;

impl RowType for Retry {
    const KIND: &'static str = "files.retry";
    const LABEL: &'static str = "Retry";
}

pub(crate) struct Place;

impl RowType for Place {
    const KIND: &'static str = "files.place";
    const LABEL: &'static str = "Place";
}

pub(crate) struct Template;

impl RowType for Template {
    const KIND: &'static str = "files.template";
    const LABEL: &'static str = "Template";
}

pub(crate) struct NearbyTransfer;

impl RowType for NearbyTransfer {
    const KIND: &'static str = "files.nearby-transfer";
    const LABEL: &'static str = "Nearby transfer";
}

pub(crate) struct RemoteAccount;

impl RowType for RemoteAccount {
    const KIND: &'static str = "files.remote-account";
    const LABEL: &'static str = "Remote account";
}

pub(crate) struct CloudEntry;

impl RowType for CloudEntry {
    const KIND: &'static str = "files.cloud-entry";
    const LABEL: &'static str = "Cloud entry";
}

pub(crate) struct Drive;

impl RowType for Drive {
    const KIND: &'static str = "files.drive";
    const LABEL: &'static str = "Drive";
}

pub(crate) struct Document;

impl RowType for Document {
    const KIND: &'static str = "files.record";
    const LABEL: &'static str = "Document";
}

pub(crate) struct Entry;

impl RowType for Entry {
    const KIND: &'static str = "files.entry";
    const LABEL: &'static str = "File";
}
