//! Narrow AccountsService administration for local login accounts.
//!
//! AccountsService is the authority and performs the polkit challenge.  qOS
//! never invokes `useradd` with UI strings, edits `/etc/passwd`, or stores a
//! login password in the Settings document.  Password bytes travel only over
//! an anonymous stdin pipe to `openssl passwd`, then as a salted hash in the
//! D-Bus request; neither value appears in argv or diagnostics.

use std::time::Duration;

use zbus::zvariant::OwnedObjectPath;
use zeroize::{Zeroize, Zeroizing};

use crate::process::run_with_input;

const DEST: &str = "org.freedesktop.Accounts";
const ROOT: &str = "/org/freedesktop/Accounts";
const MANAGER: &str = "org.freedesktop.Accounts";
const USER: &str = "org.freedesktop.Accounts.User";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountRole {
    Standard = 0,
    Administrator = 1,
}

fn runtime() -> Result<tokio::runtime::Runtime, String> {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|e| format!("cannot start account-service client: {e}"))
}

fn validate_username(name: &str) -> Result<&str, String> {
    if name.is_empty()
        || name.len() > 32
        || !name.bytes().enumerate().all(|(i, b)| {
            b.is_ascii_lowercase()
                || b.is_ascii_digit() && i > 0
                || matches!(b, b'_' | b'-') && i > 0
        })
    {
        return Err(
            "user name must start with a lowercase letter and contain only lowercase letters, numbers, '-' or '_'"
                .into(),
        );
    }
    Ok(name)
}

fn validate_real_name(name: &str) -> Result<&str, String> {
    let name = name.trim();
    if name.is_empty()
        || name.chars().count() > 128
        || name.chars().any(|c| c == '\0' || c == '\n' || c == '\r')
    {
        return Err("full name must be one line of 1–128 characters".into());
    }
    Ok(name)
}

async fn manager_proxy(conn: &zbus::Connection) -> Result<zbus::Proxy<'_>, String> {
    zbus::Proxy::new(conn, DEST, ROOT, MANAGER)
        .await
        .map_err(|e| format!("AccountsService is unavailable: {e}"))
}

async fn find_user(conn: &zbus::Connection, username: &str) -> Result<OwnedObjectPath, String> {
    validate_username(username)?;
    let reply = manager_proxy(conn)
        .await?
        .call_method("FindUserByName", &(username))
        .await
        .map_err(account_error)?;
    reply
        .body()
        .deserialize::<OwnedObjectPath>()
        .map_err(|e| format!("AccountsService returned an invalid user path: {e}"))
}

fn account_error(e: zbus::Error) -> String {
    // D-Bus errors can include policy/action names but never values supplied
    // to SetPassword.  Keep the useful authentication failure while avoiding
    // Debug output of the whole message body.
    format!("account change was refused: {e}")
}

fn block<T>(f: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    runtime()?.block_on(f)
}

pub fn create_user(username: &str, full_name: &str, role: AccountRole) -> Result<(), String> {
    let username = validate_username(username)?.to_string();
    let full_name = validate_real_name(full_name)?.to_string();
    block(async move {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach the system bus: {e}"))?;
        manager_proxy(&conn)
            .await?
            .call_method("CreateUser", &(username, full_name, role as i32))
            .await
            .map_err(account_error)?;
        Ok(())
    })
}

pub fn delete_user(username: &str, uid: u64, remove_files: bool) -> Result<(), String> {
    validate_username(username)?;
    if uid < 1000 || uid >= 60000 {
        return Err("refusing to remove a system account".into());
    }
    let current = std::env::var("USER").unwrap_or_default();
    if current == username {
        return Err("sign in as another administrator before removing this account".into());
    }
    block(async move {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach the system bus: {e}"))?;
        manager_proxy(&conn)
            .await?
            .call_method("DeleteUser", &(uid as i64, remove_files))
            .await
            .map_err(account_error)?;
        Ok(())
    })
}

async fn call_user(
    username: &str,
    method: &str,
    body: &(impl serde::ser::Serialize + zbus::zvariant::DynamicType),
) -> Result<(), String> {
    let conn = zbus::Connection::system()
        .await
        .map_err(|e| format!("cannot reach the system bus: {e}"))?;
    let path = find_user(&conn, username).await?;
    let proxy = zbus::Proxy::new(&conn, DEST, path, USER)
        .await
        .map_err(|e| format!("cannot open account record: {e}"))?;
    proxy
        .call_method(method, body)
        .await
        .map_err(account_error)?;
    Ok(())
}

pub fn set_role(username: &str, role: AccountRole) -> Result<(), String> {
    validate_username(username)?;
    if std::env::var("USER").ok().as_deref() == Some(username) && role == AccountRole::Standard {
        return Err("another administrator must remove your administrator role".into());
    }
    block(call_user(username, "SetAccountType", &(role as i32)))
}

pub fn set_automatic_login(username: &str, enabled: bool) -> Result<(), String> {
    validate_username(username)?;
    block(call_user(username, "SetAutomaticLogin", &enabled))
}

pub fn automatic_login(username: &str) -> Result<bool, String> {
    validate_username(username)?;
    block(async move {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach the system bus: {e}"))?;
        let path = find_user(&conn, username).await?;
        let proxy = zbus::Proxy::new(&conn, DEST, path, USER)
            .await
            .map_err(|e| format!("cannot open account record: {e}"))?;
        proxy
            .get_property::<bool>("AutomaticLogin")
            .await
            .map_err(account_error)
    })
}

pub fn set_real_name(username: &str, real_name: &str) -> Result<(), String> {
    validate_username(username)?;
    let real_name = validate_real_name(real_name)?.to_string();
    block(call_user(username, "SetRealName", &real_name))
}

pub fn set_avatar(username: &str, path: &std::path::Path) -> Result<(), String> {
    validate_username(username)?;
    let meta =
        std::fs::symlink_metadata(path).map_err(|e| format!("cannot read avatar image: {e}"))?;
    if !meta.file_type().is_file() || meta.len() > 5 * 1024 * 1024 {
        return Err("avatar must be a regular image file no larger than 5 MB".into());
    }
    let path = path
        .canonicalize()
        .map_err(|e| format!("cannot resolve avatar image: {e}"))?;
    let value = path.to_string_lossy().to_string();
    block(call_user(username, "SetIconFile", &value))
}

pub fn set_password(username: &str, password: &mut [u8]) -> Result<(), String> {
    validate_username(username)?;
    if password.len() < 10 || password.len() > 1024 || password.contains(&b'\n') {
        password.zeroize();
        return Err("password must contain 10–1024 characters and no newline".into());
    }
    let mut input = Zeroizing::new(password.to_vec());
    input.push(b'\n');
    password.zeroize();
    let hash = run_with_input(
        "openssl",
        &["passwd", "-6", "-stdin"],
        &input,
        Duration::from_secs(5),
    )?;
    let hash = Zeroizing::new(hash.trim().to_string());
    if !hash.starts_with("$6$") {
        return Err("password hashing service returned an unexpected result".into());
    }
    block(call_user(username, "SetPassword", &(hash.as_str(), "")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn validates_account_names_before_privilege_boundary() {
        assert!(validate_username("ada").is_ok());
        assert!(validate_username("ada_2").is_ok());
        assert!(validate_username("Ada").is_err());
        assert!(validate_username("../root").is_err());
        assert!(validate_real_name("Ada Lovelace").is_ok());
        assert!(validate_real_name("Ada\nroot").is_err());
    }

    #[test]
    fn password_validation_always_scrubs_the_caller_buffer() {
        let mut short = b"bad".to_vec();
        assert!(set_password("ada", &mut short).is_err());
        assert!(short.iter().all(|b| *b == 0));
    }
}
