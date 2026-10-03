//! The catalogue snapshot, the anti-rollback checkpoint, and version
//! selection.

use std::cmp::Ordering;

use registry_types::{CatalogueSnapshot, RegistryTarget, RegistryVersion};
use serde::{Deserialize, Serialize};

use crate::VerifyError;

/// Largest snapshot accepted, the same bound native clients apply.
pub const MAX_SNAPSHOT_BYTES: usize = registry_types::MAX_SNAPSHOT_ENCODED_BYTES;

/// The post-MVP features every browser that runs the toolkit core offers:
/// what `rustc` enables by default for `wasm32-unknown-unknown`.
pub const DEFAULT_WASM_FEATURES: &[&str] = &[
    "bulk-memory",
    "bulk-memory-opt",
    "call-indirect-overlong",
    "multivalue",
    "mutable-globals",
    "nontrapping-fptoint",
    "reference-types",
    "sign-ext",
];

/// The last snapshot a client accepted from one registry, kept across
/// visits. Same JSON shape as the native client's checkpoint file:
/// `{"sequence":7,"snapshot_hash_hex":"…"}`.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Checkpoint {
    pub sequence: u64,
    pub snapshot_hash_hex: String,
}

impl Checkpoint {
    /// Parse a stored checkpoint.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::Checkpoint`] for anything but the JSON above.
    pub fn from_json(bytes: &[u8]) -> Result<Self, VerifyError> {
        serde_json::from_slice(bytes).map_err(|error| VerifyError::Checkpoint(error.to_string()))
    }

    /// The JSON to store.
    pub fn to_json(&self) -> Vec<u8> {
        serde_json::to_vec(self).unwrap_or_default()
    }

    /// A newer snapshot, or the very same one again.
    fn admits(&self, sequence: u64, hash: &str) -> bool {
        match sequence.cmp(&self.sequence) {
            Ordering::Greater => true,
            Ordering::Equal => hash == self.snapshot_hash_hex,
            Ordering::Less => false,
        }
    }
}

/// A snapshot whose every proof checked out, and the checkpoint that
/// replaces the stored one.
#[derive(Clone, Debug)]
pub struct VerifiedSnapshot {
    snapshot: CatalogueSnapshot,
    checkpoint: Checkpoint,
}

/// What the page can run.
#[derive(Clone, Copy, Debug)]
pub struct Host<'a> {
    /// Post-MVP wasm features the browser offers (`target_features`
    /// spelling), e.g. [`DEFAULT_WASM_FEATURES`].
    pub features: &'a [&'a str],
    /// The exact runtime ABI id a version must have: the loaded core's, when
    /// selecting an app; the wanted core's, when selecting the core itself.
    /// `None` accepts any.
    pub runtime: Option<&'a str>,
}

/// One version chosen from a verified snapshot: everything
/// [`crate::verify_package`] binds the downloaded tapp to.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Selection {
    pub package_id: String,
    pub version: String,
    /// The entry's publisher key; the tapp must be signed by it.
    pub publisher_pubkey_hex: String,
    /// BLAKE3 of the tapp's signed root: the thin tapp's address.
    pub package_root_hex: String,
    /// BLAKE3 of the full tapp the publisher submitted.
    pub tapp_blake3_hex: String,
    pub target: RegistryTarget,
}

/// Verify a `/entries` response, full or compact, against the pinned owner
/// key and the stored checkpoint. Both forms of one snapshot share its
/// checkpoint, the full snapshot's hash.
///
/// # Errors
///
/// Returns [`VerifyError::SnapshotTooLarge`] or
/// [`VerifyError::SnapshotDecode`] for bytes that are not a snapshot,
/// [`VerifyError::Snapshot`] for any failed proof (including an owner key
/// that is not a valid, non-zero Ed25519 key), and [`VerifyError::Rollback`]
/// for an older snapshot than the checkpoint or a different one at the same
/// sequence.
pub fn verify_snapshot(
    bytes: &[u8],
    owner_pubkey_hex: &str,
    checkpoint: Option<&Checkpoint>,
    now_unix: u64,
) -> Result<VerifiedSnapshot, VerifyError> {
    if bytes.len() > MAX_SNAPSHOT_BYTES {
        return Err(VerifyError::SnapshotTooLarge);
    }
    let snapshot = CatalogueSnapshot::from_json(bytes)
        .map_err(|error| VerifyError::SnapshotDecode(error.to_string()))?;
    snapshot.verify(owner_pubkey_hex, now_unix)?;
    let hash = snapshot.checkpoint_hash()?;
    if let Some(old) = checkpoint
        && !old.admits(snapshot.sequence(), &hash)
    {
        return Err(VerifyError::Rollback {
            checkpoint: old.sequence,
            sequence: snapshot.sequence(),
        });
    }
    let checkpoint = Checkpoint {
        sequence: snapshot.sequence(),
        snapshot_hash_hex: hash,
    };
    Ok(VerifiedSnapshot {
        snapshot,
        checkpoint,
    })
}

impl VerifiedSnapshot {
    /// The checkpoint to store in place of the old one.
    pub fn checkpoint(&self) -> &Checkpoint {
        &self.checkpoint
    }

    pub fn sequence(&self) -> u64 {
        self.snapshot.sequence()
    }

    pub fn snapshot(&self) -> &CatalogueSnapshot {
        &self.snapshot
    }

    /// The newest `web`/`wasm32` version of `package_id` the host can run.
    ///
    /// # Errors
    ///
    /// Returns [`VerifyError::NotListed`] when the snapshot has no such
    /// entry and [`VerifyError::NoCompatibleVersion`] when none of its
    /// versions fits the host.
    pub fn select(&self, package_id: &str, host: &Host<'_>) -> Result<Selection, VerifyError> {
        let entries = self.snapshot.entries();
        let entry = entries
            .iter()
            .map(|listed| listed.entry)
            .find(|entry| entry.package_id == package_id)
            .ok_or_else(|| VerifyError::NotListed(package_id.to_string()))?;
        let chosen = entry
            .versions
            .iter()
            .filter(|version| runs_on(version, host))
            .max_by(|left, right| registry_types::version::compare(&left.version, &right.version))
            .ok_or_else(|| VerifyError::NoCompatibleVersion {
                package_id: package_id.to_string(),
                wanted: host.runtime.map_or_else(
                    || format!("features {:?}", host.features),
                    |runtime| format!("{runtime} with features {:?}", host.features),
                ),
            })?;
        Ok(Selection {
            package_id: entry.package_id.clone(),
            version: chosen.version.clone(),
            publisher_pubkey_hex: entry.publisher_pubkey_hex.to_ascii_lowercase(),
            package_root_hex: chosen.tapp_root_hash_hex.to_ascii_lowercase(),
            tapp_blake3_hex: chosen.tapp_blake3_hex.to_ascii_lowercase(),
            target: chosen.target.clone(),
        })
    }
}

fn runs_on(version: &RegistryVersion, host: &Host<'_>) -> bool {
    let target = &version.target;
    target.is_web()
        && target.validate().is_ok()
        && registry_types::parse_wasm_isa(&target.isa)
            .is_ok_and(|isa| isa.is_satisfied_by(host.features))
        && host
            .runtime
            .is_none_or(|runtime| target.runtime.as_deref() == Some(runtime))
}
