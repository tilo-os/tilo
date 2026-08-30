//! Shell-side host for the identity ceremony: persistence
//! (`~/.config/qos/identity/primary.json`, `0600`), the first-run
//! trigger, invite-URL parsing, and the in-memory pairing-channel shim.
//!
//! The on-disk format is plaintext-JSON matching
//! [`ui::widget::identity_flow::IdentityRecord`] — pending work wraps
//! the blob with libsecret or keyutils; for now the file mode is the
//! only mitigation.
//!
//! Intentionally thin: cryptographic logic stays in
//! `threshold-identity`, visual logic stays in `ui`.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use ui::widget::identity_flow::{
    IdentityFlowError, IdentityRecord, IdentityStore, InviteEnvelope, PairingHandshake,
};

/// Writes to `$XDG_CONFIG_HOME/qos/identity/primary.json` (falling
/// back to `$HOME/.config/...`), `0600` mode.
///
/// TODO: the blob is currently plaintext JSON. Replace with a
/// libsecret-encrypted envelope so a hostile process reading
/// `/proc/<pid>/maps` of the compositor can't trivially exfiltrate the
/// secret share.
pub struct DiskIdentityStore {
    path: PathBuf,
}

impl DiskIdentityStore {
    /// Resolves the config dir the same way `firstboot::run` does, so
    /// the boot chain is self-consistent.
    ///
    /// # Errors
    ///
    /// Returns an error if the identity directory can't be resolved or
    /// created.
    pub fn from_env() -> Result<Self, io::Error> {
        let base = resolve_identity_dir()?;
        fs::create_dir_all(&base)?;
        // The contents will be 0600, but a world-listable directory
        // still leaks "identity exists" / atime.
        #[cfg(unix)]
        {
            let mut perm = fs::metadata(&base)?.permissions();
            perm.set_mode(0o700);
            let _ = fs::set_permissions(&base, perm);
        }
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
        if !self.path.exists() {
            return Ok(None);
        }
        let bytes = fs::read(&self.path).map_err(IdentityFlowError::Io)?;
        Ok(Some(IdentityRecord::from_json_bytes(&bytes)?))
    }

    fn save(&self, record: &IdentityRecord) -> Result<(), IdentityFlowError> {
        let bytes = record.to_json_bytes()?;
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(IdentityFlowError::Io)?;
        }
        // 0600 lands on the temp file, so the record is never readable by
        // anyone else at its final path.
        atomic_file::write_with_mode(&self.path, &bytes, 0o600).map_err(IdentityFlowError::Io)?;
        Ok(())
    }
}

fn resolve_identity_dir() -> Result<PathBuf, io::Error> {
    if let Some(x) = std::env::var_os("XDG_CONFIG_HOME")
        && !x.is_empty()
    {
        return Ok(PathBuf::from(x).join("qos").join("identity"));
    }
    if let Some(h) = std::env::var_os("HOME")
        && !h.is_empty()
    {
        return Ok(PathBuf::from(h)
            .join(".config")
            .join("qos")
            .join("identity"));
    }
    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "neither XDG_CONFIG_HOME nor HOME set",
    ))
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
        assert_eq!(back, rec);
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
