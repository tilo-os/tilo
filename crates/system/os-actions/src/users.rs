//! Narrow `AccountsService` administration for local login accounts.
//!
//! `AccountsService` is the authority and performs the polkit challenge.  Tilo
//! never invokes `useradd` with UI strings, edits `/etc/passwd`, or stores a
//! login password in the Settings document.  Password bytes travel only over
//! a narrow in-process Rust SHA-crypt implementation, then as a salted hash in
//! the D-Bus request; neither value appears in argv or diagnostics.

use zbus::zvariant::OwnedObjectPath;
use zeroize::Zeroizing;

const DEST: &str = "org.freedesktop.Accounts";
const ROOT: &str = "/org/freedesktop/Accounts";
const MANAGER: &str = "org.freedesktop.Accounts";
const USER: &str = "org.freedesktop.Accounts.User";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AccountRole {
    Standard = 0,
    Administrator = 1,
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
        .map_err(|e| account_error(&e))?;
    reply
        .body()
        .deserialize::<OwnedObjectPath>()
        .map_err(|e| format!("AccountsService returned an invalid user path: {e}"))
}

fn account_error(e: &zbus::Error) -> String {
    // D-Bus errors can include policy/action names but never values supplied
    // to SetPassword.  Keep the useful authentication failure while avoiding
    // Debug output of the whole message body.
    format!("account change was refused: {e}")
}

fn block<T>(f: impl std::future::Future<Output = Result<T, String>>) -> Result<T, String> {
    crate::block_on(f)
}

/// Create a local login account through `AccountsService`.
///
/// # Errors
///
/// Returns an error message if `username` or `full_name` fails validation, the system bus is
/// unreachable, or `AccountsService` refuses the request (e.g. the polkit challenge fails).
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
            .map_err(|e| account_error(&e))?;
        Ok(())
    })
}

/// Delete the local login account `username` through `AccountsService`.
///
/// # Errors
///
/// Returns an error message if `username` fails validation, `uid` is outside the ordinary user
/// range `1000..60000`, `username` is the caller's own account, `uid` does not fit in an
/// `i64`, the system bus is unreachable, or `AccountsService` refuses the request.
pub fn delete_user(username: &str, uid: u64, remove_files: bool) -> Result<(), String> {
    validate_username(username)?;
    if !(1000..60000).contains(&uid) {
        return Err("refusing to remove a system account".into());
    }
    let current = std::env::var("USER").unwrap_or_default();
    if current == username {
        return Err("sign in as another administrator before removing this account".into());
    }
    let uid = i64::try_from(uid).map_err(|_| "user id is out of range".to_string())?;
    block(async move {
        let conn = zbus::Connection::system()
            .await
            .map_err(|e| format!("cannot reach the system bus: {e}"))?;
        manager_proxy(&conn)
            .await?
            .call_method("DeleteUser", &(uid, remove_files))
            .await
            .map_err(|e| account_error(&e))?;
        Ok(())
    })
}

// The returned future borrows `zbus::Proxy` internals that are not `Send`; that is fine here
// because `call_user` is only ever driven to completion synchronously by its
// caller via `block()`, never spawned onto another thread.
#[allow(
    clippy::future_not_send,
    reason = "only ever polled synchronously by its caller through block()"
)]
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
        .map_err(|e| account_error(&e))?;
    Ok(())
}

/// Change `username`'s account role (standard vs. administrator).
///
/// # Errors
///
/// Returns an error message if `username` fails validation, `username` is the caller's own
/// account and `role` would demote it, or `AccountsService` refuses the request.
pub fn set_role(username: &str, role: AccountRole) -> Result<(), String> {
    validate_username(username)?;
    if std::env::var("USER").ok().as_deref() == Some(username) && role == AccountRole::Standard {
        return Err("another administrator must remove your administrator role".into());
    }
    block(call_user(username, "SetAccountType", &(role as i32)))
}

/// Enable or disable passwordless automatic login for `username`.
///
/// # Errors
///
/// Returns an error message if `username` fails validation or `AccountsService` refuses the
/// request.
pub fn set_automatic_login(username: &str, enabled: bool) -> Result<(), String> {
    validate_username(username)?;
    block(call_user(username, "SetAutomaticLogin", &enabled))
}

/// Read whether `username` is set up for automatic login.
///
/// # Errors
///
/// Returns an error message if `username` fails validation, the system bus is unreachable, or
/// `AccountsService` refuses the request.
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
            .map_err(|e| account_error(&e))
    })
}

/// Change `username`'s display ("full") name.
///
/// # Errors
///
/// Returns an error message if `username` or `real_name` fails validation, or
/// `AccountsService` refuses the request.
pub fn set_real_name(username: &str, real_name: &str) -> Result<(), String> {
    validate_username(username)?;
    let real_name = validate_real_name(real_name)?.to_string();
    block(call_user(username, "SetRealName", &real_name))
}

/// Set `username`'s account icon to the image at `path`.
///
/// # Errors
///
/// Returns an error message if `username` fails validation, `path` cannot be read, is not a
/// regular file, is larger than 5 MB, cannot be canonicalised, or `AccountsService` refuses the
/// request.
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

/// Set `username`'s login password. `password` is zeroized in place before returning,
/// regardless of outcome.
///
/// # Errors
///
/// Returns an error message if `username` fails validation, `password` is outside 10–1024
/// bytes or contains a newline, hashing fails, or `AccountsService` refuses the request.
pub fn set_password(username: &str, password: &mut [u8]) -> Result<(), String> {
    validate_username(username)?;
    let hash = Zeroizing::new(tilo_crypto::hash_account_password(password)?);
    block(call_user(username, "SetPassword", &(hash.as_str(), "")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn account_input_is_validated_and_scrubbed_before_the_privilege_boundary() {
        // Phase `validates_account_names_before_privilege_boundary`.
        {
            assert!(validate_username("ada").is_ok());
            assert!(validate_username("ada_2").is_ok());
            assert!(validate_username("Ada").is_err());
            assert!(validate_username("../root").is_err());
            assert!(validate_real_name("Ada Lovelace").is_ok());
            assert!(validate_real_name("Ada\nroot").is_err());
        }
        // Phase `password_validation_always_scrubs_the_caller_buffer`.
        {
            let mut short = b"bad".to_vec();
            assert!(set_password("ada", &mut short).is_err());
            assert!(short.iter().all(|b| *b == 0));
        }
    }
}
