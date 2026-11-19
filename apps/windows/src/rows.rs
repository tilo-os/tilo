//! The rows Windows Apps publishes for what the compatibility service
//! reports: every catalogue application, the private data one of them keeps,
//! the install awaiting a choice and the managed appliance.
//!
//! Each row names its entity, and the effects it offers are
//! [`WindowsEffect`](crate::actions::WindowsEffect)'s, bound to it.

use crate::actions::WindowsEffect;

use ui::tilo_resource;

/// One application of the catalogue.
#[derive(tilo_resource::Row)]
#[row(
    kind = "windows.compat-app",
    label = "Windows app",
    effects = WindowsEffect
)]
pub struct CompatApp;

/// The private data one application keeps, which repair and removal act on.
#[derive(tilo_resource::Row)]
#[row(
    kind = "windows.prefix",
    label = "Private data",
    effects = WindowsEffect
)]
pub struct PrivateData;

/// An install the service waits for a choice on.
#[derive(tilo_resource::Row)]
#[row(
    kind = "windows.pending-install",
    label = "Pending install",
    effects = WindowsEffect
)]
pub struct PendingInstall;

/// The managed Windows appliance.
#[derive(tilo_resource::Row)]
#[row(
    kind = "windows.appliance",
    label = "Windows appliance",
    effects = WindowsEffect
)]
pub struct Appliance;

/// The application id an app row's or a private-data row's id names.
pub(crate) fn app_of_row(row: &str) -> &str {
    row.strip_prefix(crate::search::APP_PREFIX)
        .or_else(|| row.strip_prefix(&format!("{PRIVATE_DATA_PREFIX}:")))
        .unwrap_or(row)
}

/// The prefix a private-data row's id carries before its application's.
pub(crate) const PRIVATE_DATA_PREFIX: &str = "windows.prefix";
