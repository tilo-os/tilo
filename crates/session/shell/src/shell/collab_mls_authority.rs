//! Broker-owned transaction boundary for MLS membership and workspace roles.
//!
//! No app-facing RPC accepts these values. The workspace authority constructs
//! typed commands containing only random handles registered by [`MlsVaultProvider`].

use crate::shell::state::WorkspaceAuthority as _;
use getrandom::{SysRng, rand_core::UnwrapErr};
use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use channel_crypto::{ChannelGroup, GovernanceJournal, KeyPackage, VaultKey};
use collab_types::{
    CapOverride, ChannelId, Ed25519PublicKey, MlsGovernanceAction, MlsGovernanceContext,
    MlsGovernanceFrameV1, Principal, Subject, Verb,
};
use ed25519_dalek::SigningKey;
use runtime_collab::{
    MlsAuthorityAction, MlsAuthorityTransaction, MlsKeyPackageHandle, MlsSnapshotReceipt,
    MlsVaultHandle, MlsWorkspaceRole,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use thiserror::Error;
use zeroize::Zeroizing;

use super::collab_core::BrokerChannels;
use super::state::WorkspaceTreeHandle;

#[derive(Debug, Error)]
pub enum MlsAuthorityError {
    #[error("MLS vault provider unavailable: {0}")]
    VaultUnavailable(String),
    #[error("MLS authority transaction is stale or unauthorized: {0}")]
    Unauthorized(String),
    #[error("MLS authority persistence failed: {0}")]
    Persistence(String),
    #[error("MLS operation failed: {0}")]
    Crypto(String),
}

/// Secret-bearing state returned only by a registered provider.
pub struct ProvisionedMlsState {
    pub group: ChannelGroup,
    pub governance_signer: SigningKey,
}

/// Platform-vault capability boundary. Implementations validate opaque handles
/// against their own registry; callers never provide keys or paths.
pub trait MlsVaultProvider: Send + Sync {
    /// # Errors
    ///
    /// Returns an error when the opaque group handle cannot be provisioned.
    fn checkout(&self, handle: MlsVaultHandle) -> Result<ProvisionedMlsState, MlsAuthorityError>;
    /// # Errors
    ///
    /// Returns an error when the provider cannot durably store the group.
    fn persist(
        &self,
        handle: MlsVaultHandle,
        group: &ChannelGroup,
        governance_signer: &SigningKey,
    ) -> Result<(), MlsAuthorityError>;
    /// # Errors
    ///
    /// Returns an error when the opaque key-package handle is unavailable.
    fn key_package(&self, handle: MlsKeyPackageHandle) -> Result<KeyPackage, MlsAuthorityError>;
    /// # Errors
    ///
    /// Returns an error when the receipt cannot be resolved to a digest.
    fn snapshot_digest(
        &self,
        receipt: MlsSnapshotReceipt,
        channel: ChannelId,
    ) -> Result<[u8; 32], MlsAuthorityError>;

    /// Test/fault-injection seam after the outbox is durable. Production
    /// providers leave the default implementation unchanged.
    ///
    /// # Errors
    ///
    /// Returns a provider-injected durability failure.
    fn after_governance_durable(&self) -> Result<(), MlsAuthorityError> {
        Ok(())
    }
}

/// Explicit fail-closed provider for hosts where the session Secret Service
/// cannot be opened. It never creates an owner-only key file as a fallback.
pub struct UnavailableMlsVault;

impl MlsVaultProvider for UnavailableMlsVault {
    fn checkout(&self, _: MlsVaultHandle) -> Result<ProvisionedMlsState, MlsAuthorityError> {
        Err(MlsAuthorityError::VaultUnavailable(
            "Secret Service MLS provider was not installed by the launcher".into(),
        ))
    }

    fn persist(
        &self,
        _: MlsVaultHandle,
        _: &ChannelGroup,
        _: &SigningKey,
    ) -> Result<(), MlsAuthorityError> {
        Err(MlsAuthorityError::VaultUnavailable(
            "Secret Service MLS provider was not installed by the launcher".into(),
        ))
    }

    fn key_package(&self, _: MlsKeyPackageHandle) -> Result<KeyPackage, MlsAuthorityError> {
        Err(MlsAuthorityError::VaultUnavailable(
            "Secret Service MLS provider was not installed by the launcher".into(),
        ))
    }

    fn snapshot_digest(
        &self,
        _: MlsSnapshotReceipt,
        _: ChannelId,
    ) -> Result<[u8; 32], MlsAuthorityError> {
        Err(MlsAuthorityError::VaultUnavailable(
            "durable snapshot receipt provider is unavailable".into(),
        ))
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct SecretServiceGroupRecord {
    state_file: String,
    signer_locator: String,
}

#[derive(Clone, Serialize, Deserialize)]
struct SnapshotRecord {
    channel: ChannelId,
    digest: [u8; 32],
    envelope_file: String,
}

#[derive(Default, Serialize, Deserialize)]
struct SecretServiceRegistry {
    groups: BTreeMap<MlsVaultHandle, SecretServiceGroupRecord>,
    key_packages: BTreeMap<MlsKeyPackageHandle, String>,
    snapshots: BTreeMap<MlsSnapshotReceipt, SnapshotRecord>,
    latest_snapshot: BTreeMap<[u8; 16], MlsSnapshotReceipt>,
}

/// Production provider backed by the system secret store (the freedesktop
/// Secret Service on Linux, the Keychain on macOS, or the sealed file store
/// a headless machine names with `TILO_SECRETS_DIR`). Only opaque registry
/// identifiers and ciphertext are written to disk.
pub struct SecretServiceMlsVault {
    root: PathBuf,
    master_key: Zeroizing<[u8; 32]>,
    registry: Mutex<SecretServiceRegistry>,
}

impl SecretServiceMlsVault {
    /// Open at the session-owned state root selected by the launcher. No home
    /// directory fallback is accepted because that would weaken key custody.
    ///
    /// # Errors
    ///
    /// Returns an error when session state or vault material cannot be opened.
    pub fn open_for_session() -> Result<Self, MlsAuthorityError> {
        let state = xdg_paths::env_dir("XDG_STATE_HOME").ok_or_else(|| {
            MlsAuthorityError::VaultUnavailable(
                "launcher did not provide XDG_STATE_HOME for MLS state".into(),
            )
        })?;
        Self::open(state.join("tilo/collaboration/mls"))
    }

    /// Broker-owned WAL location paired with this provider. It is derived
    /// from the launcher's state root and is never supplied by an app/RPC.
    #[cfg(not(test))]
    pub(crate) fn authority_root(&self) -> PathBuf {
        self.root.join("authority")
    }

    /// Where this device keeps its sealed channel group and member state.
    #[cfg(not(test))]
    pub(crate) fn channel_state_root(&self) -> PathBuf {
        self.root.join("channels")
    }

    /// Key sealing this device's channel state, derived from the Secret
    /// Service master key under its own domain so it seals nothing else.
    #[cfg(not(test))]
    pub(crate) fn channel_state_key(&self) -> VaultKey {
        let mut hash = Sha256::new();
        hash.update(b"tilo.channel-state-key.v1");
        hash.update(self.master_key.as_slice());
        VaultKey::new(hash.finalize().into())
    }

    /// Open the provider, creating its master key in the secret store if this is
    /// the first use. There is intentionally no file-key fallback.
    ///
    /// # Errors
    ///
    /// Returns unavailable if the secret store is absent/locked, or persistence
    /// errors for an invalid registry.
    pub fn open(root: PathBuf) -> Result<Self, MlsAuthorityError> {
        fs::create_dir_all(&root).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
        let locator = format!(
            "mls-master-{}",
            hex::encode(Sha256::digest(root.as_os_str().as_encoded_bytes()))
        );
        let registry_path = root.join("registry.postcard");
        let master_key = match secret_service_load(&locator) {
            Ok(bytes) => bytes,
            Err(error) if registry_path.exists() => return Err(error),
            Err(_) => {
                let mut bytes = Zeroizing::new([0_u8; 32]);
                rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), bytes.as_mut());
                secret_service_store(&locator, bytes.as_ref())?;
                let verified = secret_service_load(&locator)?;
                if verified.as_slice() != bytes.as_slice() {
                    return Err(MlsAuthorityError::VaultUnavailable(
                        "the secret store did not return the newly stored MLS key".into(),
                    ));
                }
                verified
            }
        };
        let registry = if registry_path.exists() {
            postcard::from_bytes(&fs::read(registry_path).map_err(io_error)?)
                .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?
        } else {
            SecretServiceRegistry::default()
        };
        Ok(Self {
            root,
            master_key,
            registry: Mutex::new(registry),
        })
    }

    /// Register a freshly provisioned endpoint group and governance signer,
    /// returning the only value later transactions may present.
    ///
    /// # Errors
    ///
    /// Returns an error when the group or signer cannot be stored securely.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the registry guard remains held through durable registry persistence so a concurrent registration cannot overwrite this new record"
    )]
    pub fn register_group(
        &self,
        group: &ChannelGroup,
        signer: &SigningKey,
    ) -> Result<MlsVaultHandle, MlsAuthorityError> {
        let handle = MlsVaultHandle::from_issued_bytes(random_capability());
        let token = hex::encode(handle.as_bytes());
        let state_file = format!("group-{token}.mls");
        let signer_locator = format!("mls-governance-{token}");
        secret_service_store(&signer_locator, &signer.to_bytes())?;
        group
            .save_sealed(
                &self.root.join(&state_file),
                &VaultKey::new(*self.master_key),
            )
            .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("MLS registry lock poisoned".into()))?;
        registry.groups.insert(
            handle,
            SecretServiceGroupRecord {
                state_file,
                signer_locator,
            },
        );
        self.persist_registry(&registry)?;
        Ok(handle)
    }

    /// Durably register a public MLS key package behind an unguessable handle.
    /// It is consumed only by the typed authority coordinator.
    ///
    /// # Errors
    ///
    /// Returns an error when the key package or registry cannot be persisted.
    #[allow(
        clippy::significant_drop_tightening,
        clippy::needless_pass_by_value,
        reason = "key-package registration atomically persists the encoded package and its private registry mapping"
    )]
    pub fn register_key_package(
        &self,
        package: KeyPackage,
    ) -> Result<MlsKeyPackageHandle, MlsAuthorityError> {
        let handle = MlsKeyPackageHandle::from_issued_bytes(random_capability());
        let package_file = format!("key-package-{}.mls", hex::encode(handle.as_bytes()));
        let bytes = channel_crypto::encode_key_package(&package)
            .map_err(|error| MlsAuthorityError::Crypto(error.to_string()))?;
        atomic_write(&self.root.join(&package_file), &bytes)?;
        let mut registry = self.registry.lock().map_err(|_| {
            MlsAuthorityError::Persistence("key-package registry lock poisoned".into())
        })?;
        registry.key_packages.insert(handle, package_file);
        self.persist_registry(&registry)?;
        Ok(handle)
    }

    /// Durably store a signed encrypted compaction envelope before issuing its
    /// opaque receipt. Wrong-channel, plaintext, unsigned and non-compaction
    /// envelopes are refused even if this is called outside broker ingress.
    ///
    /// # Errors
    ///
    /// Returns an error when the envelope is unauthorized or receipt state
    /// cannot be persisted.
    #[allow(
        clippy::significant_drop_tightening,
        reason = "receipt insertion and registry persistence remain serialized to prevent a persisted envelope without its opaque receipt"
    )]
    pub fn mint_snapshot_receipt(
        &self,
        channel: ChannelId,
        envelope: &collab_types::SignedOp,
    ) -> Result<MlsSnapshotReceipt, MlsAuthorityError> {
        if envelope.op_class != collab_types::OpClass::Compaction
            || envelope
                .encryption
                .as_ref()
                .is_none_or(|metadata| metadata.channel != channel)
            || collab_types::verify_op(envelope).is_err()
        {
            return Err(MlsAuthorityError::Unauthorized(
                "snapshot receipt requires a signed, route-bound encrypted compaction".into(),
            ));
        }
        let bytes = postcard::to_allocvec(envelope)
            .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let receipt = MlsSnapshotReceipt::from_issued_bytes(random_capability());
        let envelope_file = format!("snapshot-{}.op", hex::encode(receipt.as_bytes()));
        atomic_write(&self.root.join(&envelope_file), &bytes)?;
        let mut registry = self
            .registry
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("MLS registry lock poisoned".into()))?;
        registry.snapshots.insert(
            receipt,
            SnapshotRecord {
                channel,
                digest,
                envelope_file,
            },
        );
        registry
            .latest_snapshot
            .insert(*channel.0.as_bytes(), receipt);
        self.persist_registry(&registry)?;
        Ok(receipt)
    }

    /// Latest receipt minted after the document broker durably stored an
    /// encrypted compaction snapshot for `channel`.
    pub fn latest_snapshot_receipt(&self, channel: ChannelId) -> Option<MlsSnapshotReceipt> {
        self.registry
            .lock()
            .ok()?
            .latest_snapshot
            .get(channel.0.as_bytes())
            .copied()
    }

    fn persist_registry(&self, registry: &SecretServiceRegistry) -> Result<(), MlsAuthorityError> {
        let bytes = postcard::to_allocvec(registry)
            .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?;
        atomic_write(&self.root.join("registry.postcard"), &bytes)
    }
}

impl MlsVaultProvider for SecretServiceMlsVault {
    #[allow(
        clippy::significant_drop_tightening,
        reason = "the registry record borrows the guard while its sealed group and signer are loaded as one coherent provisioning snapshot"
    )]
    fn checkout(&self, handle: MlsVaultHandle) -> Result<ProvisionedMlsState, MlsAuthorityError> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("MLS registry lock poisoned".into()))?;
        let record = registry.groups.get(&handle).ok_or_else(|| {
            MlsAuthorityError::VaultUnavailable("unregistered vault capability".into())
        })?;
        let signer_bytes = secret_service_load(&record.signer_locator)?;
        let signer_array: [u8; 32] = signer_bytes.as_slice().try_into().map_err(|_| {
            MlsAuthorityError::VaultUnavailable("stored MLS signer has invalid length".into())
        })?;
        let group = ChannelGroup::load_sealed(
            &self.root.join(&record.state_file),
            &VaultKey::new(*self.master_key),
        )
        .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?;
        Ok(ProvisionedMlsState {
            group,
            governance_signer: SigningKey::from_bytes(&signer_array),
        })
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the selected vault record must remain stable while its sealed state is written"
    )]
    fn persist(
        &self,
        handle: MlsVaultHandle,
        group: &ChannelGroup,
        _: &SigningKey,
    ) -> Result<(), MlsAuthorityError> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("MLS registry lock poisoned".into()))?;
        let record = registry.groups.get(&handle).ok_or_else(|| {
            MlsAuthorityError::VaultUnavailable("unregistered vault capability".into())
        })?;
        group
            .save_sealed(
                &self.root.join(&record.state_file),
                &VaultKey::new(*self.master_key),
            )
            .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the registry mapping remains stable while the referenced package is loaded"
    )]
    fn key_package(&self, handle: MlsKeyPackageHandle) -> Result<KeyPackage, MlsAuthorityError> {
        let registry = self.registry.lock().map_err(|_| {
            MlsAuthorityError::Persistence("key-package registry lock poisoned".into())
        })?;
        let package_file = registry.key_packages.get(&handle).ok_or_else(|| {
            MlsAuthorityError::VaultUnavailable("unregistered key-package capability".into())
        })?;
        let bytes = fs::read(self.root.join(package_file)).map_err(io_error)?;
        channel_crypto::decode_key_package(&bytes)
            .map_err(|error| MlsAuthorityError::Crypto(error.to_string()))
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "receipt-to-channel validation and sealed envelope loading observe one registry snapshot"
    )]
    fn snapshot_digest(
        &self,
        receipt: MlsSnapshotReceipt,
        channel: ChannelId,
    ) -> Result<[u8; 32], MlsAuthorityError> {
        let registry = self
            .registry
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("MLS registry lock poisoned".into()))?;
        let record = registry
            .snapshots
            .get(&receipt)
            .filter(|record| record.channel == channel)
            .ok_or_else(|| {
                MlsAuthorityError::VaultUnavailable(
                    "unregistered or wrong-channel snapshot receipt".into(),
                )
            })?;
        let bytes = fs::read(self.root.join(&record.envelope_file)).map_err(io_error)?;
        let digest: [u8; 32] = Sha256::digest(&bytes).into();
        let envelope: collab_types::SignedOp = postcard::from_bytes(&bytes)
            .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?;
        let envelope_is_valid = envelope.op_class == collab_types::OpClass::Compaction
            && envelope
                .encryption
                .is_some_and(|metadata| metadata.channel == channel)
            && collab_types::verify_op(&envelope).is_ok();
        if digest != record.digest || !envelope_is_valid {
            return Err(MlsAuthorityError::Persistence(
                "durable snapshot envelope failed digest/signature/route validation".into(),
            ));
        }
        Ok(digest)
    }
}

fn random_capability() -> [u8; 32] {
    let mut bytes = [0_u8; 32];
    rand_core::Rng::fill_bytes(&mut UnwrapErr(SysRng), &mut bytes);
    bytes
}

/// The secret-store namespace holding the MLS master and signer keys.
const MLS_SECRETS_APP: &str = "so.tilo.collaboration.mls";

/// The system secret store: the Secret Service on Linux, the login Keychain
/// on macOS.
fn mls_secrets() -> tilo_secrets::SecretStore {
    tilo_secrets::SecretStore::for_app(MLS_SECRETS_APP)
}

fn secret_service_store(locator: &str, secret: &[u8]) -> Result<(), MlsAuthorityError> {
    let options = tilo_secrets::PutOptions {
        label: Some("Tilo MLS key".into()),
    };
    mls_secrets()
        .put_blocking(locator, secret, options)
        .map_err(|error| {
            MlsAuthorityError::VaultUnavailable(format!("MLS key storage refused: {error}"))
        })
}

fn secret_service_load(locator: &str) -> Result<Zeroizing<[u8; 32]>, MlsAuthorityError> {
    let stored = mls_secrets().get_blocking(locator).map_err(|error| {
        MlsAuthorityError::VaultUnavailable(format!("MLS key lookup unavailable: {error}"))
    })?;
    let bytes = if let Some(secret) = stored {
        Zeroizing::new(secret.expose().to_vec())
    } else {
        let legacy = legacy_secret_tool_load(locator)?
            .ok_or_else(|| MlsAuthorityError::VaultUnavailable("MLS key unavailable".into()))?;
        secret_service_store(locator, &legacy)?;
        legacy
    };
    let key: [u8; 32] = bytes.as_slice().try_into().map_err(|_| {
        MlsAuthorityError::VaultUnavailable("stored MLS key has invalid length".into())
    })?;
    Ok(Zeroizing::new(key))
}

/// A key stored by earlier builds through `secret-tool` under their own
/// attributes; `None` when there is none (or no `secret-tool`). The caller
/// moves it into the system store.
fn legacy_secret_tool_load(locator: &str) -> Result<Option<Zeroizing<Vec<u8>>>, MlsAuthorityError> {
    let Ok(output) = bounded_command::run(
        "secret-tool",
        &["lookup", "tilo-purpose", "mls", "tilo-mls-key", locator],
        None,
        64 * 1024,
        Duration::from_secs(10),
    ) else {
        return Ok(None);
    };
    if !output.status.success() {
        return Ok(None);
    }
    let stdout = Zeroizing::new(output.stdout);
    let decoded = Zeroizing::new(
        hex::decode(String::from_utf8_lossy(stdout.as_slice()).trim()).map_err(|_| {
            MlsAuthorityError::VaultUnavailable("Secret Service returned malformed MLS key".into())
        })?,
    );
    Ok(Some(decoded))
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct ChannelLedger {
    revision: u64,
    members: BTreeMap<[u8; 32], MlsWorkspaceRole>,
}

#[derive(Clone, Default, Serialize, Deserialize)]
struct AuthorityLedger {
    pending: Option<MlsAuthorityTransaction>,
    channels: BTreeMap<[u8; 16], ChannelLedger>,
    completed: BTreeMap<[u8; 32], (MlsAuthorityTransaction, [u8; 32])>,
}

/// Serial coordinator. One instance owns one broker state directory and must
/// be called under the workspace authority's mutation serialization point.
pub struct MlsAuthorityCoordinator<P: MlsVaultProvider> {
    provider: Arc<P>,
    root: PathBuf,
    ledger: AuthorityLedger,
    workspaces: WorkspaceTreeHandle,
    broker: Arc<Mutex<BrokerChannels>>,
}

impl<P: MlsVaultProvider> MlsAuthorityCoordinator<P> {
    /// Open a broker-owned authority ledger. `root` comes from shell startup,
    /// never from an RPC argument.
    ///
    /// # Errors
    ///
    /// Returns an error when the authority ledger cannot be created or read.
    pub fn open(
        root: PathBuf,
        provider: Arc<P>,
        workspaces: WorkspaceTreeHandle,
        broker: Arc<Mutex<BrokerChannels>>,
    ) -> Result<Self, MlsAuthorityError> {
        fs::create_dir_all(&root).map_err(io_error)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
        }
        let path = root.join("authority.postcard");
        let ledger = if path.exists() {
            postcard::from_bytes(&fs::read(path).map_err(io_error)?)
                .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?
        } else {
            AuthorityLedger::default()
        };
        Ok(Self {
            provider,
            root,
            ledger,
            workspaces,
            broker,
        })
    }

    /// Apply one authority-created command, or return the prior result for an
    /// identical idempotency key. A pending write-ahead record is recovered
    /// before a new command is considered.
    ///
    /// # Errors
    ///
    /// Returns an error when authority, persistence, or transaction invariants
    /// reject the request.
    #[allow(
        clippy::needless_pass_by_value,
        reason = "the transaction is cloned into the durable idempotency ledger only after the current command is admitted"
    )]
    pub fn apply(&mut self, tx: MlsAuthorityTransaction) -> Result<[u8; 32], MlsAuthorityError> {
        if let Some((completed_tx, digest)) = self.ledger.completed.get(&tx.idempotency_key) {
            if completed_tx != &tx {
                return Err(MlsAuthorityError::Unauthorized(
                    "idempotency key was reused for a different transaction".into(),
                ));
            }
            return Ok(*digest);
        }
        if let Some(pending) = self.ledger.pending.clone() {
            if pending != tx {
                return Err(MlsAuthorityError::Persistence(
                    "a different MLS transaction requires recovery, or reused its key".into(),
                ));
            }
            if let Some(digest) = self.recover_committed(&pending)? {
                return Ok(digest);
            }
        } else {
            self.ledger.pending = Some(tx.clone());
            self.persist_ledger()?;
        }
        let outcome = self.apply_fresh(tx.clone());
        if outcome.is_err() {
            let channel = ChannelId(uuid::Uuid::from_bytes(tx.channel));
            let frame_is_durable = GovernanceJournal::open(self.journal_path(channel))
                .ok()
                .and_then(|journal| {
                    journal
                        .frame_for_transaction(tx.idempotency_key)
                        .map(|_| ())
                })
                .is_some();
            if !frame_is_durable {
                self.ledger.pending = None;
                self.persist_ledger()?;
            }
        }
        outcome
    }

    #[allow(
        clippy::too_many_lines,
        clippy::needless_pass_by_value,
        reason = "a fresh MLS transaction keeps cryptographic mutation, governance projection, and write-ahead completion in one auditable atomic path"
    )]
    fn apply_fresh(&mut self, tx: MlsAuthorityTransaction) -> Result<[u8; 32], MlsAuthorityError> {
        let channel = ChannelId(uuid::Uuid::from_bytes(tx.channel));
        let mut provisioned = self.provider.checkout(tx.vault)?;
        if provisioned.group.channel_id() != channel {
            return Err(MlsAuthorityError::Unauthorized(
                "vault handle names another channel".into(),
            ));
        }
        let signer =
            Ed25519PublicKey::from_bytes(provisioned.governance_signer.verifying_key().to_bytes());
        self.ensure_workspace_admin(&tx, signer)?;
        let mut channel_ledger = self
            .ledger
            .channels
            .get(&tx.channel)
            .cloned()
            .unwrap_or_default();
        if tx.expected_membership_revision != channel_ledger.revision.saturating_add(1) {
            return Err(MlsAuthorityError::Unauthorized(
                "membership revision is not the next revision".into(),
            ));
        }
        if channel_ledger.members.is_empty() {
            channel_ledger
                .members
                .insert(*signer.as_bytes(), MlsWorkspaceRole::Admin);
        }
        if channel_ledger.members.get(signer.as_bytes()) != Some(&MlsWorkspaceRole::Admin) {
            return Err(MlsAuthorityError::Unauthorized(
                "signer is not an authoritative administrator".into(),
            ));
        }
        let current = key_set(&channel_ledger.members);
        let mut desired = current.clone();
        match &tx.action {
            MlsAuthorityAction::Admit { recipient, .. } => {
                desired.insert(Ed25519PublicKey::from_bytes(*recipient));
            }
            MlsAuthorityAction::Remove { recipient, .. } => {
                desired.remove(&Ed25519PublicKey::from_bytes(*recipient));
            }
            MlsAuthorityAction::EnableEncryption { .. } => {}
        }
        let governors = channel_ledger
            .members
            .iter()
            .filter_map(|(key, role)| {
                (*role == MlsWorkspaceRole::Admin).then_some(Ed25519PublicKey::from_bytes(*key))
            })
            .collect();
        let mut journal = GovernanceJournal::open(self.journal_path(channel))
            .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?;
        let head = journal.head_digest();
        let mut committed_digest = None;

        match tx.action.clone() {
            MlsAuthorityAction::Admit {
                recipient,
                role,
                key_package,
            } => {
                let package = self.provider.key_package(key_package)?;
                let provider = Arc::clone(&self.provider);
                let vault = tx.vault;
                let signer_key = &provisioned.governance_signer;
                provisioned
                    .group
                    .add_member_transactional(package, |group, addition| {
                        let epoch = addition.epoch;
                        provider
                            .persist(vault, group, signer_key)
                            .map_err(authority_to_crypto)?;
                        let frame = MlsGovernanceFrameV1::signed(
                            channel,
                            tx.idempotency_key,
                            tx.expected_membership_revision,
                            head,
                            epoch,
                            MlsGovernanceAction::AddWithKeyHistory {
                                recipient: Ed25519PublicKey::from_bytes(recipient),
                                welcome: addition.welcome.clone(),
                                existing_member_commit: addition.commit.clone(),
                                key_history: addition.key_history.clone(),
                            },
                            signer_key,
                        )
                        .map_err(|e| channel_crypto::Error::Serialization(e.to_string()))?;
                        let context = MlsGovernanceContext {
                            channel,
                            membership_revision: tx.expected_membership_revision,
                            previous_frame_digest: head,
                            expected_epoch: epoch,
                            governors: &governors,
                            desired_members: &desired,
                            current_mls_members: &current,
                            durable_snapshot_ack: None,
                        };
                        committed_digest = Some(journal.enqueue(frame, &context)?);
                        provider
                            .after_governance_durable()
                            .map_err(authority_to_crypto)
                    })
                    .map_err(|error| MlsAuthorityError::Crypto(error.to_string()))?;
                channel_ledger.members.insert(recipient, role);
            }
            MlsAuthorityAction::Remove {
                recipient,
                leaf_index,
            } => {
                let provider = Arc::clone(&self.provider);
                let vault = tx.vault;
                let signer_key = &provisioned.governance_signer;
                provisioned
                    .group
                    .remove_member_transactional(leaf_index, |group, commit, epoch| {
                        provider
                            .persist(vault, group, signer_key)
                            .map_err(authority_to_crypto)?;
                        let frame = MlsGovernanceFrameV1::signed(
                            channel,
                            tx.idempotency_key,
                            tx.expected_membership_revision,
                            head,
                            epoch,
                            MlsGovernanceAction::Remove {
                                removed: Ed25519PublicKey::from_bytes(recipient),
                                commit: commit.to_vec(),
                            },
                            signer_key,
                        )
                        .map_err(|e| channel_crypto::Error::Serialization(e.to_string()))?;
                        let context = MlsGovernanceContext {
                            channel,
                            membership_revision: tx.expected_membership_revision,
                            previous_frame_digest: head,
                            expected_epoch: epoch,
                            governors: &governors,
                            desired_members: &desired,
                            current_mls_members: &current,
                            durable_snapshot_ack: None,
                        };
                        committed_digest = Some(journal.enqueue(frame, &context)?);
                        provider
                            .after_governance_durable()
                            .map_err(authority_to_crypto)
                    })
                    .map_err(|error| MlsAuthorityError::Crypto(error.to_string()))?;
                channel_ledger.members.remove(&recipient);
            }
            MlsAuthorityAction::EnableEncryption { snapshot } => {
                let snapshot_digest = self.provider.snapshot_digest(snapshot, channel)?;
                let epoch = provisioned.group.epoch();
                let frame = MlsGovernanceFrameV1::signed(
                    channel,
                    tx.idempotency_key,
                    tx.expected_membership_revision,
                    head,
                    epoch,
                    MlsGovernanceAction::EnableEncryption {
                        encrypted_snapshot_digest: snapshot_digest,
                    },
                    &provisioned.governance_signer,
                )
                .map_err(|e| MlsAuthorityError::Crypto(e.to_string()))?;
                let context = MlsGovernanceContext {
                    channel,
                    membership_revision: tx.expected_membership_revision,
                    previous_frame_digest: head,
                    expected_epoch: epoch,
                    governors: &governors,
                    desired_members: &desired,
                    current_mls_members: &current,
                    durable_snapshot_ack: Some(snapshot_digest),
                };
                committed_digest = Some(
                    journal
                        .enqueue(frame, &context)
                        .map_err(|e| MlsAuthorityError::Persistence(e.to_string()))?,
                );
            }
        }
        self.provider
            .persist(tx.vault, &provisioned.group, &provisioned.governance_signer)?;
        self.apply_workspace_role(&tx, signer, false)?;
        channel_ledger.revision = tx.expected_membership_revision;
        self.ledger.channels.insert(tx.channel, channel_ledger);
        let digest = committed_digest.ok_or_else(|| {
            MlsAuthorityError::Persistence("governance frame was not committed".into())
        })?;
        self.broker
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("broker state lock poisoned".into()))?
            .register_governance_journal(channel, journal);
        self.ledger
            .completed
            .insert(tx.idempotency_key, (tx.clone(), digest));
        self.ledger.pending = None;
        self.persist_ledger()?;
        Ok(digest)
    }

    fn recover_committed(
        &mut self,
        tx: &MlsAuthorityTransaction,
    ) -> Result<Option<[u8; 32]>, MlsAuthorityError> {
        let channel = ChannelId(uuid::Uuid::from_bytes(tx.channel));
        let journal = GovernanceJournal::open(self.journal_path(channel))
            .map_err(|e| MlsAuthorityError::Persistence(e.to_string()))?;
        let Some(frame) = journal.frame_for_transaction(tx.idempotency_key) else {
            return Ok(None);
        };
        let digest = frame
            .digest()
            .map_err(|e| MlsAuthorityError::Persistence(e.to_string()))?;
        let mut provisioned = self.provider.checkout(tx.vault)?;
        if provisioned.group.epoch() < frame.epoch {
            if !provisioned
                .group
                .merge_pending_transaction()
                .map_err(|e| MlsAuthorityError::Crypto(e.to_string()))?
            {
                return Err(MlsAuthorityError::Persistence(
                    "journal advanced but vault has no pending commit".into(),
                ));
            }
            self.provider
                .persist(tx.vault, &provisioned.group, &provisioned.governance_signer)?;
        }
        let signer =
            Ed25519PublicKey::from_bytes(provisioned.governance_signer.verifying_key().to_bytes());
        self.apply_workspace_role(tx, signer, true)?;
        let ledger = self.ledger.channels.entry(tx.channel).or_default();
        ledger
            .members
            .entry(*signer.as_bytes())
            .or_insert(MlsWorkspaceRole::Admin);
        match tx.action {
            MlsAuthorityAction::Admit {
                recipient, role, ..
            } => {
                ledger.members.insert(recipient, role);
            }
            MlsAuthorityAction::Remove { recipient, .. } => {
                ledger.members.remove(&recipient);
            }
            MlsAuthorityAction::EnableEncryption { .. } => {}
        }
        ledger.revision = tx.expected_membership_revision;
        self.broker
            .lock()
            .map_err(|_| MlsAuthorityError::Persistence("broker state lock poisoned".into()))?
            .register_governance_journal(channel, journal);
        self.ledger
            .completed
            .insert(tx.idempotency_key, (tx.clone(), digest));
        self.ledger.pending = None;
        self.persist_ledger()?;
        Ok(Some(digest))
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "workspace authorization and its capability mutation must share one write-lock acquisition"
    )]
    fn apply_workspace_role(
        &self,
        tx: &MlsAuthorityTransaction,
        signer: Ed25519PublicKey,
        recovery: bool,
    ) -> Result<(), MlsAuthorityError> {
        let signer_user = hex::encode(signer.as_bytes());
        let mut tree = self.workspaces.write();
        let workspace = tree
            .desktop
            .workspaces
            .iter_mut()
            .find(|workspace| workspace.room.to_string() == tx.workspace)
            .ok_or_else(|| MlsAuthorityError::Unauthorized("workspace is unavailable".into()))?;
        if !workspace.may_administer(&signer_user) {
            return Err(MlsAuthorityError::Unauthorized(
                "governance signer is not a live workspace administrator".into(),
            ));
        }
        match tx.action {
            MlsAuthorityAction::Admit {
                recipient, role, ..
            } => {
                let user = hex::encode(recipient);
                let cap = CapOverride::membership(
                    Subject::Principal(Principal::Person(user)),
                    workspace.room.to_string(),
                    role_verb(role),
                )
                .with_id(member_capability_id(tx.channel, recipient));
                workspace.grant_as(&signer_user, cap).ok_or_else(|| {
                    MlsAuthorityError::Unauthorized("workspace rejected membership grant".into())
                })?;
            }
            MlsAuthorityAction::Remove { recipient, .. } => {
                let id = member_capability_id(tx.channel, recipient);
                if !workspace.revoke_as(&signer_user, &id) && !recovery {
                    return Err(MlsAuthorityError::Unauthorized(
                        "workspace membership capability was absent".into(),
                    ));
                }
            }
            MlsAuthorityAction::EnableEncryption { .. } => {}
        }
        Ok(())
    }

    #[allow(
        clippy::significant_drop_tightening,
        reason = "the administrator check reads one coherent workspace authority snapshot"
    )]
    fn ensure_workspace_admin(
        &self,
        tx: &MlsAuthorityTransaction,
        signer: Ed25519PublicKey,
    ) -> Result<(), MlsAuthorityError> {
        let signer_user = hex::encode(signer.as_bytes());
        let tree = self.workspaces.read();
        let workspace = tree
            .desktop
            .workspaces
            .iter()
            .find(|workspace| workspace.room.to_string() == tx.workspace)
            .ok_or_else(|| MlsAuthorityError::Unauthorized("workspace is unavailable".into()))?;
        if !workspace.may_administer(&signer_user) {
            return Err(MlsAuthorityError::Unauthorized(
                "governance signer is not a live workspace administrator".into(),
            ));
        }
        Ok(())
    }

    fn journal_path(&self, channel: ChannelId) -> PathBuf {
        self.root.join(format!("{}.governance", channel.0))
    }
    fn persist_ledger(&self) -> Result<(), MlsAuthorityError> {
        atomic_write(
            &self.root.join("authority.postcard"),
            &postcard::to_allocvec(&self.ledger)
                .map_err(|e| MlsAuthorityError::Persistence(e.to_string()))?,
        )
    }
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "the error is converted at ownership transfer into the crypto boundary"
)]
fn authority_to_crypto(error: MlsAuthorityError) -> channel_crypto::Error {
    channel_crypto::Error::Governance(error.to_string())
}
fn key_set(members: &BTreeMap<[u8; 32], MlsWorkspaceRole>) -> BTreeSet<Ed25519PublicKey> {
    members
        .keys()
        .copied()
        .map(Ed25519PublicKey::from_bytes)
        .collect()
}
fn role_verb(role: MlsWorkspaceRole) -> Verb {
    match role {
        MlsWorkspaceRole::Admin => Verb::Admin,
        MlsWorkspaceRole::Editor => Verb::Write,
        MlsWorkspaceRole::Commenter => Verb::Comment,
        MlsWorkspaceRole::Viewer => Verb::Read,
    }
}
fn member_capability_id(channel: [u8; 16], member: [u8; 32]) -> String {
    format!(
        "mls-{}-{}",
        uuid::Uuid::from_bytes(channel),
        hex::encode(member)
    )
}
#[allow(
    clippy::needless_pass_by_value,
    reason = "I/O errors are consumed by map_err at the persistence boundary"
)]
fn io_error(error: std::io::Error) -> MlsAuthorityError {
    MlsAuthorityError::Persistence(error.to_string())
}
fn atomic_write(path: &Path, bytes: &[u8]) -> Result<(), MlsAuthorityError> {
    tilo_files::native::atomic::write_with_mode(path, bytes, 0o600).map_err(io_error)
}

#[allow(
    unused_imports,
    reason = "The native WorkspaceExt methods are selected by host-specific workspace code and test configurations."
)]
use crate::shell::state::WorkspaceExt as _;

impl tilo_action_broker::collab_core::SnapshotReceiptVault for SecretServiceMlsVault {
    fn mint_snapshot_receipt(
        &self,
        channel: ChannelId,
        envelope: &collab_types::SignedOp,
    ) -> Result<(), String> {
        SecretServiceMlsVault::mint_snapshot_receipt(self, channel, envelope)
            .map(|_| ())
            .map_err(|error| error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use crate::shell::state::FreshId as _;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, Ordering};

    use channel_crypto::MemberKey;
    use collab_types::generate_keypair;

    use super::*;
    use crate::shell::state::{Workspace, WorkspaceId, WorkspaceTree};

    struct HermeticVault {
        state_path: PathBuf,
        state_handle: MlsVaultHandle,
        signer: SigningKey,
        packages: Mutex<HashMap<MlsKeyPackageHandle, KeyPackage>>,
        snapshots: HashMap<MlsSnapshotReceipt, (ChannelId, [u8; 32])>,
        fail_after_journal_once: AtomicBool,
    }

    impl MlsVaultProvider for HermeticVault {
        fn checkout(
            &self,
            handle: MlsVaultHandle,
        ) -> Result<ProvisionedMlsState, MlsAuthorityError> {
            if handle != self.state_handle {
                return Err(MlsAuthorityError::VaultUnavailable(
                    "unregistered vault capability".into(),
                ));
            }
            Ok(ProvisionedMlsState {
                group: ChannelGroup::load_sealed(&self.state_path, &VaultKey::new([9; 32]))
                    .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))?,
                governance_signer: self.signer.clone(),
            })
        }

        fn persist(
            &self,
            handle: MlsVaultHandle,
            group: &ChannelGroup,
            _: &SigningKey,
        ) -> Result<(), MlsAuthorityError> {
            if handle != self.state_handle {
                return Err(MlsAuthorityError::VaultUnavailable(
                    "unregistered vault capability".into(),
                ));
            }
            group
                .save_sealed(&self.state_path, &VaultKey::new([9; 32]))
                .map_err(|error| MlsAuthorityError::Persistence(error.to_string()))
        }

        fn key_package(
            &self,
            handle: MlsKeyPackageHandle,
        ) -> Result<KeyPackage, MlsAuthorityError> {
            self.packages
                .lock()
                .map_err(|_| MlsAuthorityError::Persistence("package lock poisoned".into()))?
                .get(&handle)
                .cloned()
                .ok_or_else(|| {
                    MlsAuthorityError::VaultUnavailable(
                        "unregistered key-package capability".into(),
                    )
                })
        }

        fn snapshot_digest(
            &self,
            receipt: MlsSnapshotReceipt,
            channel: ChannelId,
        ) -> Result<[u8; 32], MlsAuthorityError> {
            self.snapshots
                .get(&receipt)
                .filter(|(bound_channel, _)| *bound_channel == channel)
                .map(|(_, digest)| *digest)
                .ok_or_else(|| {
                    MlsAuthorityError::VaultUnavailable(
                        "unregistered or wrong-channel snapshot receipt".into(),
                    )
                })
        }

        fn after_governance_durable(&self) -> Result<(), MlsAuthorityError> {
            if self.fail_after_journal_once.swap(false, Ordering::SeqCst) {
                return Err(MlsAuthorityError::Persistence(
                    "simulated crash after durable journal".into(),
                ));
            }
            Ok(())
        }
    }

    #[allow(
        clippy::too_many_lines,
        clippy::significant_drop_tightening,
        reason = "the recovery test intentionally preserves its complete durable-transaction scenario"
    )]
    #[test]
    fn authority_recovers_partial_commit_and_is_idempotent() {
        let dir = tempfile::tempdir().unwrap();
        let channel = ChannelId::new();
        let state_path = dir.path().join("vault/group.mls");
        let (signer, signer_pk) = generate_keypair();
        let owner = MemberKey::generate("owner").unwrap();
        let group = ChannelGroup::create(channel, owner).unwrap();
        group
            .save_sealed(&state_path, &VaultKey::new([9; 32]))
            .unwrap();
        let bob = MemberKey::generate("bob").unwrap();
        let package = bob.fresh_key_package().unwrap();
        let state_handle = MlsVaultHandle::from_issued_bytes([1; 32]);
        let package_handle = MlsKeyPackageHandle::from_issued_bytes([2; 32]);
        let snapshot_receipt = MlsSnapshotReceipt::from_issued_bytes([3; 32]);
        let provider = Arc::new(HermeticVault {
            state_path,
            state_handle,
            signer,
            packages: Mutex::new(HashMap::from([(package_handle, package)])),
            snapshots: HashMap::from([(snapshot_receipt, (channel, [44; 32]))]),
            fail_after_journal_once: AtomicBool::new(true),
        });

        let mut workspace = Workspace::new(WorkspaceId::new(), "MLS test");
        workspace.set_owner(hex::encode(signer_pk.as_bytes()));
        let workspace_id = workspace.room.to_string();
        let mut tree = WorkspaceTree::default();
        tree.desktop.workspaces.push(workspace);
        let workspaces = tree.shared();
        let broker = Arc::new(Mutex::new(BrokerChannels::default()));
        let root = dir.path().join("authority");
        let tx = MlsAuthorityTransaction {
            idempotency_key: [7; 32],
            channel: *channel.0.as_bytes(),
            workspace: workspace_id,
            expected_membership_revision: 1,
            vault: state_handle,
            action: MlsAuthorityAction::Admit {
                recipient: [5; 32],
                role: MlsWorkspaceRole::Editor,
                key_package: package_handle,
            },
        };

        let mut coordinator = MlsAuthorityCoordinator::open(
            root.clone(),
            Arc::clone(&provider),
            Arc::clone(&workspaces),
            Arc::clone(&broker),
        )
        .unwrap();
        assert!(coordinator.apply(tx.clone()).is_err());
        drop(coordinator);

        let mut coordinator = MlsAuthorityCoordinator::open(
            root,
            provider,
            Arc::clone(&workspaces),
            Arc::clone(&broker),
        )
        .unwrap();
        let recovered = coordinator.apply(tx.clone()).unwrap();
        assert_eq!(coordinator.apply(tx.clone()).unwrap(), recovered);
        let workspace_name = tx.workspace.clone();
        let mut conflicting = tx;
        conflicting.expected_membership_revision = 2;
        assert!(coordinator.apply(conflicting).is_err());
        assert!(broker.lock().unwrap().governance.contains_key(&channel));
        assert_eq!(
            workspaces.read().desktop.workspaces[0].role_of(&hex::encode([5; 32])),
            Some(crate::shell::state::CapRole::Editor)
        );

        let removal = MlsAuthorityTransaction {
            idempotency_key: [8; 32],
            channel: *channel.0.as_bytes(),
            workspace: workspace_name,
            expected_membership_revision: 2,
            vault: state_handle,
            action: MlsAuthorityAction::Remove {
                recipient: [5; 32],
                leaf_index: 1,
            },
        };
        coordinator.apply(removal).unwrap();
        assert_eq!(
            workspaces.read().desktop.workspaces[0].role_of(&hex::encode([5; 32])),
            None
        );
        let pending = broker
            .lock()
            .unwrap()
            .governance
            .get(&channel)
            .unwrap()
            .pending_for(Ed25519PublicKey::from_bytes([5; 32]));
        assert!(
            pending
                .iter()
                .any(|delivery| delivery.kind == channel_crypto::DeliveryKind::Commit)
        );

        let enable = MlsAuthorityTransaction {
            idempotency_key: [9; 32],
            channel: *channel.0.as_bytes(),
            workspace: workspaces.read().desktop.workspaces[0].room.to_string(),
            expected_membership_revision: 3,
            vault: state_handle,
            action: MlsAuthorityAction::EnableEncryption {
                snapshot: snapshot_receipt,
            },
        };
        coordinator.apply(enable).unwrap();
        let broker = broker.lock().unwrap();
        let journal = broker.governance.get(&channel).unwrap();
        assert!(journal.encryption_required());
        assert!(!journal.accepts_document_encryption(None, channel));
    }

    #[test]
    fn mls_receipts_are_durable_route_bound_and_unknown_handles_rejected() {
        fn unavailable_provider_rejects_arbitrary_handles() {
            let error = UnavailableMlsVault
                .checkout(MlsVaultHandle::from_issued_bytes([99; 32]))
                .err()
                .unwrap();
            assert!(matches!(error, MlsAuthorityError::VaultUnavailable(_)));
        }

        fn production_receipt_store_is_durable_route_bound_and_tamper_evident() {
            let dir = tempfile::tempdir().unwrap();
            let provider = SecretServiceMlsVault {
                root: dir.path().to_path_buf(),
                master_key: Zeroizing::new([6; 32]),
                registry: Mutex::new(SecretServiceRegistry::default()),
            };
            let joining_member = MemberKey::generate(b"joining-device").unwrap();
            let package = joining_member.fresh_key_package().unwrap();
            let expected_package = channel_crypto::encode_key_package(&package).unwrap();
            let package_handle = provider.register_key_package(package).unwrap();
            let restarted_registry: SecretServiceRegistry =
                postcard::from_bytes(&std::fs::read(dir.path().join("registry.postcard")).unwrap())
                    .unwrap();
            let restarted_provider = SecretServiceMlsVault {
                root: dir.path().to_path_buf(),
                master_key: Zeroizing::new([6; 32]),
                registry: Mutex::new(restarted_registry),
            };
            assert_eq!(
                channel_crypto::encode_key_package(
                    &restarted_provider.key_package(package_handle).unwrap()
                )
                .unwrap(),
                expected_package
            );
            let channel = ChannelId::new();
            let (snapshot_signer, snapshot_author) = generate_keypair();
            let mut encrypted = collab_types::SignedOp::new_unsigned_with_encryption(
                vec![1, 2, 3],
                collab_types::ArtifactId::new(),
                snapshot_author,
                collab_types::AttestationId::default(),
                collab_types::OpClass::Compaction,
                std::time::SystemTime::UNIX_EPOCH,
                1,
                collab_types::EncryptionMetadata::mls_application(0, channel),
            );
            assert!(provider.mint_snapshot_receipt(channel, &encrypted).is_err());
            collab_types::sign_op(&mut encrypted, &snapshot_signer).unwrap();
            assert!(
                provider
                    .mint_snapshot_receipt(ChannelId::new(), &encrypted)
                    .is_err()
            );
            let receipt = provider.mint_snapshot_receipt(channel, &encrypted).unwrap();
            let digest = provider.snapshot_digest(receipt, channel).unwrap();
            assert_eq!(provider.latest_snapshot_receipt(channel), Some(receipt));
            assert!(provider.snapshot_digest(receipt, ChannelId::new()).is_err());

            let plaintext = collab_types::SignedOp::new_unsigned(
                vec![1],
                encrypted.artifact,
                encrypted.author,
                encrypted.attestation_id,
                collab_types::OpClass::Compaction,
                std::time::SystemTime::UNIX_EPOCH,
                2,
            );
            assert!(provider.mint_snapshot_receipt(channel, &plaintext).is_err());

            let mut wrong_class = collab_types::SignedOp::new_unsigned_with_encryption(
                vec![1],
                encrypted.artifact,
                encrypted.author,
                encrypted.attestation_id,
                collab_types::OpClass::EditMeta,
                std::time::SystemTime::UNIX_EPOCH,
                3,
                collab_types::EncryptionMetadata::mls_application(0, channel),
            );
            collab_types::sign_op(&mut wrong_class, &snapshot_signer).unwrap();
            assert!(
                provider
                    .mint_snapshot_receipt(channel, &wrong_class)
                    .is_err()
            );

            let snapshot_path = {
                let registry = provider.registry.lock().unwrap();
                dir.path().join(&registry.snapshots[&receipt].envelope_file)
            };
            let mut bytes = std::fs::read(&snapshot_path).unwrap();
            bytes.push(0);
            std::fs::write(snapshot_path, bytes).unwrap();
            assert!(provider.snapshot_digest(receipt, channel).is_err());
            assert_ne!(digest, [0; 32]);
        }

        unavailable_provider_rejects_arbitrary_handles();
        production_receipt_store_is_durable_route_bound_and_tamper_evident();
    }
}
