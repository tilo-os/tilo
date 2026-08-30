//! Shell-side host for the identity ceremony: persistence
//! (`~/.config/qos/identity/primary.json`, `0600`), the first-run
//! trigger, invite-URL parsing, and the in-memory pairing-channel shim.
//!
//! New ceremonies emit schema-1 JSON matching
//! [`ui::widget::identity_flow::IdentityRecord`] in memory, then the runtime
//! lifecycle store atomically imports it as public schema-2 metadata backed by
//! Secret Service or a host-only broker file before the ceremony returns.
//!
//! Intentionally thin: cryptographic logic stays in
//! `threshold-identity`, visual logic stays in `ui`.

use std::fs::OpenOptions;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(test)]
use std::fs;

#[cfg(unix)]
use std::os::unix::fs::{MetadataExt, OpenOptionsExt, PermissionsExt};

use ui::widget::identity_flow::{
    IdentityFlowError, IdentityRecord, IdentityStore, InviteEnvelope, PairingHandshake,
};

/// Writes public metadata to `$XDG_CONFIG_HOME/qos/identity/primary.json`
/// (falling back to `$HOME/.config/...`) and imports its private share into
/// the runtime identity provider before returning.
pub struct DiskIdentityStore {
    path: PathBuf,
}

impl DiskIdentityStore {
    /// Uses the canonical runtime resolver shared by Settings, first boot,
    /// and the launcher, so the boot chain cannot disagree because of a
    /// relative XDG path.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity directory can't be resolved or
    /// created.
    pub fn from_env() -> Result<Self, io::Error> {
        let base = runtime_identity::identity_dir_from_env()?;
        let path = base.join("primary.json");
        Ok(Self { path })
    }

    /// Used by tests to point at a tempdir without `$HOME` shimming.
    pub fn at(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn identity_present(&self) -> bool {
        self.path.exists()
    }
}

impl IdentityStore for DiskIdentityStore {
    fn load(&self) -> Result<Option<IdentityRecord>, IdentityFlowError> {
        let mut file = match OpenOptions::new()
            .read(true)
            .custom_flags(libc::O_NOFOLLOW | libc::O_CLOEXEC)
            .open(&self.path)
        {
            Ok(file) => file,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(IdentityFlowError::Io(error)),
        };
        let metadata = file.metadata().map_err(IdentityFlowError::Io)?;
        if !metadata.file_type().is_file()
            || metadata.uid() != unsafe { libc::geteuid() }
            || metadata.permissions().mode() & 0o077 != 0
            || metadata.len() > 1024 * 1024
        {
            return Err(IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "identity record must be a private user-owned regular file",
            )));
        }
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        (&mut file)
            .take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .map_err(IdentityFlowError::Io)?;
        if bytes.len() > 1024 * 1024 {
            return Err(IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::InvalidData,
                "identity record exceeds the size limit",
            )));
        }
        Ok(Some(IdentityRecord::from_json_bytes(&bytes)?))
    }

    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        let bytes = record.to_json_bytes()?;
        let parent = self.path.parent().ok_or_else(|| {
            IdentityFlowError::Io(io::Error::new(
                io::ErrorKind::InvalidInput,
                "identity path has no parent",
            ))
        })?;
        let id = self
            .path
            .file_stem()
            .and_then(|value| value.to_str())
            .ok_or_else(|| {
                IdentityFlowError::Io(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "identity path has no valid id",
                ))
            })?;
        import_identity_record(parent, id, &bytes).map_err(IdentityFlowError::Io)?;
        Ok(())
    }
}

fn import_identity_record(parent: &Path, id: &str, bytes: &[u8]) -> io::Result<PathBuf> {
    #[cfg(test)]
    {
        let provider = runtime_identity::BrokerFileProvider::new(parent);
        runtime_identity::import_identity_with_provider(parent, id, bytes, &provider)
    }
    #[cfg(not(test))]
    {
        runtime_identity::import_identity(parent, id, bytes)
    }
}

/// A free function, not a method on the store, so the boot path can
/// call it without owning the store first.
pub fn needs_first_run_flow(store: &impl IdentityStore) -> bool {
    match store.load() {
        Ok(None) => true,
        Ok(Some(_)) => false,
        Err(e) => {
            tracing::warn!("identity store load failed; treating as no identity present: {e}");
            true
        }
    }
}

/// Wraps `InviteEnvelope::from_url` plus the signature check so a
/// caller doesn't need to remember both steps.
///
/// # Errors
///
/// Returns an error if `url` doesn't parse as an invite envelope, or if
/// its signature doesn't verify.
pub fn parse_invite_url(url: &str) -> Result<InviteEnvelope, IdentityFlowError> {
    let env = InviteEnvelope::from_url(url)?;
    env.verify_signature()?;
    Ok(env)
}

/// Instantiated once at boot and shared between the host and pairer
/// wirings. In-memory shim for now; an Iroh-backed impl will land later.
pub type SharedPairingChannel = Arc<PairingHandshake>;

pub fn new_pairing_channel() -> SharedPairingChannel {
    Arc::new(PairingHandshake::new())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;
    use std::time::SystemTime;
    use threshold_identity::ThresholdIdentity;
    use ui::widget::identity_flow::MemoryIdentityStore;

    struct TmpDir(PathBuf);
    impl TmpDir {
        fn new(name: &str) -> Self {
            let base = std::env::temp_dir();
            let pid = std::process::id();
            let nonce = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |d| d.as_nanos());
            let path = base.join(format!("qos-identity-prov-{name}-{pid}-{nonce}"));
            fs::create_dir_all(&path).expect("tempdir");
            Self(path)
        }
    }
    impl Drop for TmpDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn fixture_record() -> IdentityRecord {
        let mut rng = OsRng;
        let id = ThresholdIdentity::bootstrap_single_device(&mut rng).unwrap();
        IdentityRecord::from_threshold_identity(
            &id,
            "alice",
            [248, 113, 113],
            SystemTime::UNIX_EPOCH
                .checked_add(std::time::Duration::from_secs(100))
                .unwrap_or(SystemTime::UNIX_EPOCH),
        )
        .unwrap()
    }

    #[test]
    fn save_and_load_round_trip_on_disk() {
        let tmp = TmpDir::new("rt");
        let store = DiskIdentityStore::at(tmp.0.join("primary.json"));
        assert!(store.load().unwrap().is_none());
        let rec = fixture_record();
        store.save(&rec).unwrap();
        assert!(store.identity_present());
        let back = store.load().unwrap().unwrap();
        assert_eq!(back.display_name, rec.display_name);
        assert_eq!(back.public_key(), rec.public_key());
        assert_eq!(back.schema_version, 2);
        assert!(back.threshold_identity.secret_bytes.is_empty());
        assert!(back.secret_ref.is_some());
    }

    #[test]
    fn save_sets_0600_mode_on_unix() {
        let tmp = TmpDir::new("mode");
        let store = DiskIdentityStore::at(tmp.0.join("primary.json"));
        store.save(&fixture_record()).unwrap();
        #[cfg(unix)]
        {
            let meta = fs::metadata(store.path()).unwrap();
            let mode = meta.permissions().mode() & 0o777;
            assert_eq!(mode, 0o600, "primary.json must be 0600, got {mode:o}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_an_identity_path_symlink() {
        use std::os::unix::fs::symlink;

        let tmp = TmpDir::new("symlink");
        let target = tmp.0.join("elsewhere.json");
        let bytes = fixture_record().to_json_bytes().unwrap();
        fs::write(&target, bytes).unwrap();
        fs::set_permissions(&target, fs::Permissions::from_mode(0o600)).unwrap();
        let path = tmp.0.join("primary.json");
        symlink(&target, &path).unwrap();

        let store = DiskIdentityStore::at(path);
        assert!(store.load().is_err());
    }

    #[test]
    fn first_run_flow_triggers_only_on_missing_identity() {
        let store = MemoryIdentityStore::new();
        assert!(needs_first_run_flow(&store));
        store.save(&fixture_record()).unwrap();
        assert!(!needs_first_run_flow(&store));
    }

    #[test]
    fn parse_invite_url_validates_signature() {
        use collab_types::{ChannelId, Role, generate_keypair};
        use ui::widget::identity_flow::mint_invite;
        let (sk, pk) = generate_keypair();
        let env = mint_invite(
            ChannelId::new(),
            "engineering",
            pk,
            &sk,
            "Alice",
            Role::Editor,
            SystemTime::now() + std::time::Duration::from_hours(1),
        )
        .unwrap();
        let url = env.to_url().unwrap();
        let back = parse_invite_url(&url).expect("valid URL must parse");
        assert_eq!(back.channel_name, "engineering");
    }

    #[test]
    fn parse_invite_url_rejects_garbage() {
        let err = parse_invite_url("https://example.com").unwrap_err();
        assert!(matches!(err, IdentityFlowError::DecodeFailed(_)));
    }

    #[test]
    fn load_on_missing_file_returns_none() {
        let tmp = TmpDir::new("missing");
        let store = DiskIdentityStore::at(tmp.0.join("does-not-exist.json"));
        assert!(store.load().unwrap().is_none());
        assert!(!store.identity_present());
    }

    #[test]
    fn new_pairing_channel_round_trips_a_message() {
        use threshold_identity::EnrolmentRound2;
        let ch = new_pairing_channel();
        let nonce = [0x55u8; 16];
        let mac = [0x11u8; 64];
        let reply = EnrolmentRound2 {
            from_share_index: 2,
            to_share_index: 1,
            package_bytes: vec![1, 2, 3],
        };
        ch.publish_pairer_reply(&nonce, reply.clone(), mac);
        let (back, tag) = ch.take_pairer_reply(&nonce).expect("must have reply");
        assert_eq!(back.from_share_index, reply.from_share_index);
        assert_eq!(tag, mac);
    }
}
