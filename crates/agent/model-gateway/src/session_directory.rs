//! Optional standalone host-session adapter. The trusted host writes expiring
//! grants into a private directory; browsers receive only a random bearer.
//! Filenames are SHA-256(bearer), never provider credentials. Removing or
//! changing a grant revokes active requests within the service polling period.
use crate::http_service::{Caller, Session, SessionAuthorizer};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs,
    io::Read,
    os::unix::fs::{MetadataExt, OpenOptionsExt},
    path::{Path, PathBuf},
    sync::Arc,
};

pub struct SessionDirectory {
    directory: PathBuf,
}
impl SessionDirectory {
    /// # Errors
    /// Rejects missing, non-private or foreign-owned session directories.
    pub fn new(directory: PathBuf) -> Result<Self, String> {
        if !private_directory(&directory) {
            return Err("sessions require an owner-only directory".into());
        }
        Ok(Self { directory })
    }
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Grant {
    caller: GrantCaller,
    #[serde(default)]
    manage_credentials: bool,
    expires_unix_ms: u64,
}
#[derive(Deserialize)]
#[serde(rename_all = "snake_case")]
enum GrantCaller {
    Agent,
    Chat,
    Photos,
    Settings,
}
struct DirectorySession {
    directory: PathBuf,
    path: PathBuf,
    snapshot: Vec<u8>,
    grant: Grant,
}
impl Session for DirectorySession {
    fn caller(&self) -> Caller {
        match self.grant.caller {
            GrantCaller::Agent => Caller::Agent,
            GrantCaller::Chat => Caller::Chat,
            GrantCaller::Photos => Caller::Photos,
            GrantCaller::Settings => Caller::Settings,
        }
    }
    fn may_manage_credentials(&self) -> bool {
        self.grant.manage_credentials
    }
    fn active(&self) -> bool {
        self.grant.expires_unix_ms > tilo_clock::unix_now_ms()
            && private_directory(&self.directory)
            && read_private(&self.path).is_some_and(|bytes| bytes == self.snapshot)
    }
}
impl SessionAuthorizer for SessionDirectory {
    fn authorize(&self, headers: &hyper::HeaderMap) -> Option<Arc<dyn Session>> {
        // A closed bearer format prevents path injection and keeps parsing
        // bounded. The host must generate 32 random bytes, encoded as hex.
        if headers.get_all(hyper::header::AUTHORIZATION).iter().count() != 1 {
            return None;
        }
        let token = headers
            .get(hyper::header::AUTHORIZATION)?
            .to_str()
            .ok()?
            .strip_prefix("Bearer ")?;
        if token.len() != 64 || !token.bytes().all(|b| b.is_ascii_hexdigit()) {
            return None;
        }
        if !private_directory(&self.directory) {
            return None;
        }
        let name = session_name(token);
        let path = self.directory.join(name);
        let snapshot = read_private(&path)?;
        let grant = serde_json::from_slice::<Grant>(&snapshot).ok()?;
        let session = DirectorySession {
            directory: self.directory.clone(),
            path,
            snapshot,
            grant,
        };
        session
            .active()
            .then(|| Arc::new(session) as Arc<dyn Session>)
    }
}
fn session_name(token: &str) -> String {
    use std::fmt::Write as _;
    Sha256::digest(token.as_bytes())
        .iter()
        .fold(String::with_capacity(64), |mut name, byte| {
            let _ = write!(name, "{byte:02x}");
            name
        })
}
fn owner_only(metadata: &fs::Metadata) -> bool {
    // SAFETY: geteuid has no pointer arguments or side effects.
    metadata.uid() == rustix::process::geteuid().as_raw() && metadata.mode().trailing_zeros() >= 6
}
fn private_directory(path: &Path) -> bool {
    fs::symlink_metadata(path).is_ok_and(|m| m.is_dir() && owner_only(&m))
}
fn read_private(path: &Path) -> Option<Vec<u8>> {
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW | libc::O_NONBLOCK)
        .open(path)
        .ok()?;
    let metadata = file.metadata().ok()?;
    if !metadata.is_file() || !owner_only(&metadata) || metadata.len() > 4096 {
        return None;
    }
    let mut bytes = Vec::new();
    file.take(4097).read_to_end(&mut bytes).ok()?;
    (bytes.len() <= 4096).then_some(bytes)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::os::unix::fs::PermissionsExt;
    #[test]
    fn trusted_chat_grant_preserves_narrow_identity() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let token = "c".repeat(64);
        let path = dir.path().join(session_name(&token));
        fs::write(
            &path,
            format!(
                r#"{{"caller":"chat","manage_credentials":true,"expires_unix_ms":{}}}"#,
                tilo_clock::unix_now_ms().saturating_add(60_000)
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let session = SessionDirectory::new(dir.path().to_owned())
            .unwrap()
            .authorize(&headers)
            .unwrap();
        assert!(matches!(session.caller(), Caller::Chat));
        assert!(!crate::caller::permitted(
            session.as_ref(),
            &crate::Request::SecretStatus {
                profile_id: "profile".into()
            }
        ));
    }

    #[test]
    fn grants_are_private_expiring_and_revocable() {
        let dir = tempfile::tempdir().unwrap();
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o700)).unwrap();
        let authorizer = SessionDirectory::new(dir.path().to_owned()).unwrap();
        let token = "a".repeat(64);
        let path = dir.path().join(session_name(&token));
        fs::write(
            &path,
            format!(
                r#"{{"caller":"settings","manage_credentials":true,"expires_unix_ms":{}}}"#,
                tilo_clock::unix_now_ms().saturating_add(60_000)
            ),
        )
        .unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let mut headers = hyper::HeaderMap::new();
        headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let session = authorizer.authorize(&headers).unwrap();
        assert!(session.active());
        assert!(session.may_manage_credentials());
        fs::remove_file(&path).unwrap();
        assert!(!session.active());
        assert!(authorizer.authorize(&headers).is_none());
        fs::write(&path, r#"{"caller":"agent","expires_unix_ms":0}"#).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        assert!(authorizer.authorize(&headers).is_none());
        fs::set_permissions(dir.path(), fs::Permissions::from_mode(0o755)).unwrap();
        assert!(SessionDirectory::new(dir.path().to_owned()).is_err());
    }
}
