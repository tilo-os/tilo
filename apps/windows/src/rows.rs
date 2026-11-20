//! The rows Windows Apps publishes for what the compatibility service
//! reports: the setup of a private capsule with every program it put there,
//! a program of the Windows appliance on its own, the install awaiting a
//! choice and the managed appliance.
//!
//! Each row names its entity, and the effects it offers are
//! [`WindowsEffect`](crate::actions::WindowsEffect)'s, bound to it.

use crate::actions::WindowsEffect;

use ui::tilo_resource;

/// One setup's group: every program one installer put in a private capsule, which keep one
/// set of files. Retrying its setup, continuing it in the appliance and deleting its data act
/// on the group, because they act on the capsule every program of it shares.
#[derive(tilo_resource::Row)]
#[row(
    kind = "windows.setup",
    label = "Windows setup",
    effects = WindowsEffect
)]
pub struct Setup;

/// One application of the catalogue.
#[derive(tilo_resource::Row)]
#[row(
    kind = "windows.compat-app",
    label = "Windows app",
    effects = WindowsEffect
)]
pub struct CompatApp;

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

/// The capsule a setup row's id names.
pub(crate) fn capsule_of_row(row: &str) -> Option<&str> {
    row.strip_prefix(&format!("{SETUP_PREFIX}:"))
}

/// The application id an app row's or a setup row's id names.
pub(crate) fn app_of_row(row: &str) -> &str {
    row.strip_prefix(crate::search::APP_PREFIX)
        .or_else(|| capsule_of_row(row))
        .unwrap_or(row)
}

/// The prefix a setup row's id carries before its capsule's id.
pub(crate) const SETUP_PREFIX: &str = "windows.setup";
