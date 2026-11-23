//! The `users` resource: what [`tilo_system_resources::users`] says it is, published with the
//! actions of [`UsersAction`] and marked unavailable where this computer cannot change its
//! accounts.
//!
//! The reading comes from the system's account files (`/etc/passwd`, `/etc/group`) and, for
//! automatic login, from `AccountsService`; changing an account goes through `AccountsService`
//! and polkit, which is [`crate::device::System`]'s side.

use std::collections::HashSet;

use tilo_actions::ResourceSpec;
use tilo_resource::Effect as _;
use tilo_service::{offer, unavailable};
use tilo_system_resources::users::{LocalAccount, Users, UsersAction, USERS};

use crate::i18n::name;

/// The `users` resource, as a declared type.
#[derive(tilo_resource::Row)]
#[row(kind = "so.tilo.system.users", label = "Users")]
pub struct UsersReading;

/// The `users` resource reading `users`.
#[must_use]
pub fn resource(users: &Users) -> ResourceSpec {
    let mut resource = offer::<UsersAction>(tilo_resource::typed_resource::<UsersReading>(
        USERS,
        name("so.tilo.system.users", "Users"),
    ));
    if !users.serving {
        for action in ["add", "set-admin", "set-automatic-login", "remove"] {
            unavailable(
                &mut resource,
                action,
                "no_account_service",
                "This computer cannot change its accounts",
            );
        }
    }
    resource
}

/// The accounts the machine keeps, with automatic login read from `AccountsService`. A machine
/// whose account files cannot be read answers why.
///
/// # Errors
/// The account files are unreadable.
pub fn accounts() -> Result<Vec<LocalAccount>, String> {
    let current = std::env::var("USER").ok();
    let admins = std::fs::read_to_string("/etc/group")
        .map(|contents| admins_from_group(&contents))
        .map_err(|error| format!("/etc/group: {error}"))?;
    let mut accounts = std::fs::read_to_string("/etc/passwd")
        .map(|contents| read_passwd(&contents, current.as_deref(), &admins))
        .map_err(|error| format!("/etc/passwd: {error}"))?;
    for account in &mut accounts {
        match os_actions::users::automatic_login(&account.username) {
            Ok(enabled) => account.automatic_login = enabled,
            Err(error) => account.automatic_login_error = Some(error),
        }
    }
    Ok(accounts)
}

/// The numeric id of `username`, from the account files.
///
/// # Errors
/// The account is not there.
pub fn uid(username: &str) -> Result<u64, String> {
    accounts()?
        .into_iter()
        .find(|account| account.username == username)
        .map(|account| account.uid)
        .ok_or_else(|| format!("{username} is not an account on this computer"))
}

/// The accounts `/etc/group` says administer this device: the members of `wheel` or `sudo`.
#[must_use]
pub fn admins_from_group(contents: &str) -> HashSet<String> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 4 || !matches!(fields.first(), Some(&("wheel" | "sudo"))) {
                return None;
            }
            fields.get(3).copied()
        })
        .flat_map(|members| members.split(','))
        .map(str::trim)
        .filter(|member| !member.is_empty())
        .map(str::to_string)
        .collect()
}

/// The login accounts `/etc/passwd` lists: those with a person's id and a shell that signs in.
fn read_passwd(
    contents: &str,
    current_user: Option<&str>,
    admins: &HashSet<String>,
) -> Vec<LocalAccount> {
    contents
        .lines()
        .filter_map(|line| {
            let fields: Vec<&str> = line.split(':').collect();
            if fields.len() < 7 {
                return None;
            }
            let username = *fields.first()?;
            let uid = fields.get(2)?.parse::<u64>().ok()?;
            let gecos = *fields.get(4)?;
            let shell = *fields.get(6)?;
            if !(1000..60000).contains(&uid)
                || shell.ends_with("nologin")
                || shell.ends_with("false")
            {
                return None;
            }
            let full = gecos.split(',').next().unwrap_or("").trim();
            Some(LocalAccount {
                username: username.to_string(),
                full_name: if full.is_empty() {
                    username.to_string()
                } else {
                    full.to_string()
                },
                uid,
                admin: admins.contains(username),
                current: current_user == Some(username),
                automatic_login: false,
                automatic_login_error: None,
            })
        })
        .collect()
}
