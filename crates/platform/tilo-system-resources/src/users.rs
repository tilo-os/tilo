//! The `users` resource: the local login accounts this computer keeps, with the actions that
//! create, change and remove them.
//!
//! A Unix login account owns a home and runs a session; the identities a person signs in with
//! are the identity service's, not this reading's. What changes the accounts is the session's
//! system service, through `AccountsService` and polkit.

use serde::{Deserialize, Serialize};

/// The resource's id.
pub const USERS: &str = "users";

/// A local login account.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct LocalAccount {
    /// The login name.
    pub username: String,
    /// The person's full name, or the login name when none is set.
    pub full_name: String,
    /// The account's numeric id.
    pub uid: u64,
    /// Whether the account administers this device: it is in `wheel`.
    pub admin: bool,
    /// Whether this is the account the session runs as.
    pub current: bool,
    /// Whether the account signs in without its password when the device starts.
    #[serde(default)]
    pub automatic_login: bool,
    /// Why automatic login could not be read for this account, when it could not.
    #[serde(default)]
    pub automatic_login_error: Option<String>,
}

/// What this computer's accounts read as now.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Users {
    /// Whether this computer's account service answers: where it does not, no account changes.
    #[serde(default)]
    pub serving: bool,
    pub accounts: Vec<LocalAccount>,
    /// Why the accounts could not be read, until they can.
    #[serde(default)]
    pub read_error: Option<String>,
    /// Actions taken and not landed yet, by the action's id.
    #[serde(default)]
    pub pending: Vec<String>,
    /// Why the last action did not happen, until the next one.
    #[serde(default)]
    pub problem: Option<String>,
}

/// The `users` resource's actions.
#[derive(Clone, Debug, PartialEq, tilo_resource::Effect)]
#[effect(operation = "so.tilo.system.users")]
pub enum UsersAction {
    /// Create a login account with the person's name and a password.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "plus",
        keywords = "add account, new user, benutzer hinzufügen"
    )]
    Add {
        #[param("The login name")] username: String,
        #[param("The person's full name")] full_name: String,
        #[param(secret, "The password the account signs in with")] password: String,
    },
    /// Make an account an administrator, or a standard account.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "check",
        keywords = "administrator, admin rights, administratorrechte"
    )]
    SetAdmin {
        #[param("The account's login name")] user: String,
        #[param("Whether the account administers this device")] admin: bool,
    },
    /// Sign an account in without its password when the device starts.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        icon = "check",
        keywords = "automatic login, sign in automatically, automatische anmeldung"
    )]
    SetAutomaticLogin {
        #[param("The account's login name")] user: String,
        #[param("Whether it signs in by itself")] on: bool,
    },
    /// Remove an account, keeping its home folder or removing it too.
    #[effect(kind = "outside")]
    #[effect(
        own_device,
        destructive,
        icon = "trash",
        keywords = "remove account, delete user, benutzer entfernen"
    )]
    Remove {
        #[param("The account's login name")] user: String,
        #[param("Whether the account's home folder stays")] keep_files: bool,
    },
}
