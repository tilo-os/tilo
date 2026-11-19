//! The types the window's rows are items of: a row says what it is — its
//! type's noun and the app's name — so search and an agent name it as they
//! name a resource.

use ui::tilo_resource;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.retry", label = "Retry")]
pub(crate) struct Retry;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.place", label = "Place")]
pub(crate) struct Place;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.template", label = "Template")]
pub(crate) struct Template;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.nearby-transfer", label = "Nearby transfer")]
pub(crate) struct NearbyTransfer;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.remote-account", label = "Remote account")]
pub(crate) struct RemoteAccount;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.cloud-entry", label = "Cloud entry")]
pub(crate) struct CloudEntry;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.drive", label = "Drive")]
pub(crate) struct Drive;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.record", label = "Document")]
pub(crate) struct Document;

#[derive(ui::tilo_resource::Row)]
#[row(kind = "files.entry", label = "File")]
pub(crate) struct Entry;
