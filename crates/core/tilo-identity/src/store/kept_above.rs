//! An account's store taken up from the identity directory it was kept in before each account
//! kept its own.
//!
//! A computer set up before then keeps its person's records, their selector and the secrets a
//! file keeps for them in the identity directory itself. Each account now reads only its own
//! store below it (`accounts/<uid>`), so a person kept the earlier way is brought into the store of
//! the account that runs, once, before anything reads that store: without it, the computer finds
//! no person and founds a new one, whose workspaces are none of its person's.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, PoisonError};

/// What the store of an account is not brought up with: the accounts' own stores, and the lock
/// of the directory they sit in.
const NOT_CARRIED: [&str; 2] = ["accounts", ".identity.lock"];

/// The account stores this process has looked above for, so a lookup of where an account's store
/// is does not walk the identity directory again: the take-up happens once, when the store is
/// first read, on the thread that reads it, and never on a loop that asks where it is.
static LOOKED_ABOVE: Mutex<Option<Vec<PathBuf>>> = Mutex::new(None);

/// Whether this process has already looked above `account`.
fn looked_above(account: &Path) -> bool {
    LOOKED_ABOVE
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .is_some_and(|looked| looked.iter().any(|seen| seen == account))
}

/// Keep that this process has looked above `account`, whatever it found.
fn keep_looked_above(account: &Path) {
    let mut looked = LOOKED_ABOVE.lock().unwrap_or_else(PoisonError::into_inner);
    looked
        .get_or_insert_with(Vec::new)
        .push(account.to_path_buf());
}

/// Bring the person kept in the identity directory above `account` into `account`, the store of
/// the account this process serves, when `account` keeps no identity and that directory keeps
/// one. Every record, the selector, and each file a record's secret is kept in come along; what
/// is above is left as it was. The store appears whole or not at all: it is made beside and
/// renamed into place.
///
/// Answers whether it brought a person up.
///
/// # Errors
/// What is above cannot be read, or the store cannot be written.
pub fn take_up_person_kept_above(account: &Path) -> io::Result<bool> {
    // The directory above is read at most once per account in this process: a store taken up is
    // the account's from then on, and one with nothing above it stays so for this process's life.
    if looked_above(account) {
        return Ok(false);
    }
    let brought = bring_up_person_kept_above(account)?;
    keep_looked_above(account);
    Ok(brought)
}

fn bring_up_person_kept_above(account: &Path) -> io::Result<bool> {
    let Some(above) = account
        .parent()
        .filter(|accounts| accounts.file_name().is_some_and(|name| name == "accounts"))
        .and_then(Path::parent)
    else {
        return Ok(false);
    };
    if super::resolve_active_identity(account).is_some()
        || super::resolve_active_identity(above).is_none()
    {
        return Ok(false);
    }
    // A store that holds no identity holds nothing to keep: a lock, a secret made for sealing
    // before anything was sealed with it.
    let Some(name) = account.file_name() else {
        return Ok(false);
    };
    let accounts = account.parent().unwrap_or(above);
    tilo_files::native::private::directory(accounts)?;
    let mut making = name.to_os_string();
    making.push(".bringing-up");
    let making = accounts.join(making);
    match fs::remove_dir_all(&making) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(error),
    }
    tilo_files::native::private::directory(&making)?;
    for entry in fs::read_dir(above)? {
        let entry = entry?;
        if NOT_CARRIED
            .iter()
            .any(|skipped| entry.file_name() == std::ffi::OsStr::new(skipped))
        {
            continue;
        }
        copy_kept(&entry.path(), &making.join(entry.file_name()))?;
    }
    if account.exists() {
        let mut empty = name.to_os_string();
        empty.push(".before-bringing-up");
        fs::rename(account, accounts.join(empty))?;
    }
    fs::rename(&making, account)?;
    tilo_files::native::private::open_directory(accounts)?.sync_all()?;
    Ok(true)
}

/// Copy `from` to `to` as it is kept: a directory with everything in it, a file with its bytes
/// and its access. A link is not followed, and nothing here keeps one.
fn copy_kept(from: &Path, to: &Path) -> io::Result<()> {
    let kind = fs::symlink_metadata(from)?;
    if kind.is_dir() {
        fs::create_dir(to)?;
        fs::set_permissions(to, kind.permissions())?;
        for entry in fs::read_dir(from)? {
            let entry = entry?;
            copy_kept(&entry.path(), &to.join(entry.file_name()))?;
        }
        return Ok(());
    }
    if kind.is_file() {
        fs::copy(from, to)?;
        fs::File::open(to)?.sync_all()?;
    }
    Ok(())
}

#[cfg(test)]
#[cfg(unix)]
mod tests {
    #![allow(
        clippy::unwrap_used,
        reason = "a failed step in a test is the assertion"
    )]

    use super::take_up_person_kept_above;
    use crate::store::{
        import_identity, load_from_path, load_metadata_from_path, resolve_active_identity,
        set_active_identity,
    };
    use ed25519_dalek::SigningKey;

    fn record(secret: [u8; 32], name: &str) -> Vec<u8> {
        let key = SigningKey::from_bytes(&secret);
        serde_json::to_vec(&serde_json::json!({
            "display_name": name,
            "avatar_color": [10, 20, 30],
            "threshold_identity": {
                "single_device": true,
                "group_public_key": key.verifying_key().to_bytes(),
                "threshold": 1_u16,
                "total_shares": 1_u16,
                "share_index": 1_u16,
                "secret_bytes": secret.to_vec(),
            },
            "created_at": "2026-08-30T00:00:00Z",
            "schema_version": 1,
        }))
        .unwrap()
    }

    #[test]
    fn a_person_kept_before_each_account_kept_its_own_is_its_accounts_and_signs() {
        let root = tempfile::tempdir().unwrap();
        let above = root.path();
        // An earlier person beside the one the selector names: the store keeps both, and the
        // selector still names the same one.
        import_identity(above, "primary", &record([7; 32], "Earlier")).unwrap();
        import_identity(above, "person-ada", &record([41; 32], "Ada")).unwrap();
        set_active_identity(above, "person-ada").unwrap();
        let account = above.join("accounts").join("1000");

        assert!(take_up_person_kept_above(&account).unwrap());
        let active = resolve_active_identity(&account).unwrap();
        assert!(active.starts_with(&account));
        assert_eq!(
            load_metadata_from_path(&active).unwrap().group_public_key,
            SigningKey::from_bytes(&[41; 32]).verifying_key().to_bytes()
        );
        assert_eq!(
            load_from_path(active.clone())
                .unwrap()
                .sign(b"still")
                .unwrap()
                .len(),
            64
        );
        assert!(account.join("primary.json").is_file());
        assert!(
            resolve_active_identity(above).is_some(),
            "what is above is left as it was"
        );

        // Once it keeps one, it is the account's: nothing is brought up over it.
        set_active_identity(above, "primary").unwrap();
        assert!(!take_up_person_kept_above(&account).unwrap());
        assert_eq!(resolve_active_identity(&account), Some(active));
    }

    #[test]
    fn an_account_with_nothing_above_it_or_no_person_there_is_left_alone() {
        let root = tempfile::tempdir().unwrap();
        let account = root.path().join("accounts").join("1000");
        assert!(!take_up_person_kept_above(&account).unwrap());
        assert!(!account.exists());
        // A directory not under `accounts` is no account's store.
        import_identity(root.path(), "person-ada", &record([41; 32], "Ada")).unwrap();
        assert!(!take_up_person_kept_above(&root.path().join("1000")).unwrap());
    }

    #[test]
    fn an_account_holding_no_identity_yet_takes_it_up_and_sets_its_own_beside() {
        let root = tempfile::tempdir().unwrap();
        let above = root.path();
        import_identity(above, "person-ada", &record([41; 32], "Ada")).unwrap();
        set_active_identity(above, "person-ada").unwrap();
        let account = above.join("accounts").join("1000");
        std::fs::create_dir_all(&account).unwrap();
        std::fs::write(account.join("seal.key"), [3; 32]).unwrap();

        assert!(take_up_person_kept_above(&account).unwrap());
        assert!(resolve_active_identity(&account).is_some());
        assert!(
            above
                .join("accounts/1000.before-bringing-up/seal.key")
                .is_file()
        );
    }
}
