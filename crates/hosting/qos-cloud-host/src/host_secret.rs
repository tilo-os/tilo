//! The Cloud host's signing secret, sealed at rest apart from `state.json`.
//!
//! The 32-byte secret is a `tilo-seal` envelope bound to
//! [`Label::CloudHostSecret`] and the provisioned machine identity, under a
//! key from a [`SealKeySource`]. It lives in its own owner-only directory
//! beside the Store and is never serialized into the Store, so the state file
//! can be inspected or copied without carrying the host identity. A tampered
//! blob, or one sealed for another machine or under another key, is refused.
//!
//! Limit of [`LocalFileKey`]: its key is an owner-only file on the same
//! machine, so anyone who can read the state directory as its owner (or
//! root) can open the secret. It keeps the secret out of the state file and
//! authenticates the sealed blob; it is not hardware binding. A TPM2- or
//! systemd-credential-backed [`SealKeySource`] can replace it without
//! changing the sealed format's label or context.
use anyhow::{Context as _, Result, anyhow, bail};
use ed25519_dalek::SigningKey;
use std::path::Path;
use tilo_seal::{Context, Label, SymmetricKey, envelope};
use zeroize::Zeroizing;

/// Directory below the Store root that holds the sealing key and blob.
const DIRECTORY: &str = "host-secret";

/// Where the key that seals the host secret comes from.
pub(crate) trait SealKeySource {
    /// Load the sealing key held for `dir`. A key is minted only while no
    /// sealed secret exists there; an existing blob whose key is gone is an
    /// error, never a fresh key.
    fn load_or_create(&self, dir: &Path) -> Result<SymmetricKey>;
}

/// An owner-only key file beside the sealed secret (see the module limit).
pub(crate) struct LocalFileKey;

const LOCAL_FORMAT: secret_vault::VaultFormat = secret_vault::VaultFormat {
    name: "cloud host secret",
    magic: b"TCHS",
    secret_service_label: "Tilo Cloud host secret",
    secret_service_attributes: &[("tilo-purpose", "cloud-host-secret")],
    dpapi_entropy: b"",
};

impl SealKeySource for LocalFileKey {
    fn load_or_create(&self, dir: &Path) -> Result<SymmetricKey> {
        LOCAL_FORMAT
            .load_or_create_key(dir, secret_vault::Custody::LocalFile)
            .ok_or_else(|| anyhow!("host secret sealing key unavailable"))
    }
}

/// The host's Ed25519 signing secret. Zeroized on drop, never printed.
#[derive(Clone)]
pub(crate) struct HostSecret(Zeroizing<[u8; 32]>);

impl std::fmt::Debug for HostSecret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("HostSecret(..)")
    }
}

impl HostSecret {
    pub(crate) fn signing_key(&self) -> SigningKey {
        SigningKey::from_bytes(&self.0)
    }
    pub(crate) fn expose(&self) -> &[u8; 32] {
        &self.0
    }

    /// Open the sealed secret of the Store at `root`, or — only when `mint`
    /// (no Store exists yet) — seal a fresh one. A Store without its sealed
    /// secret is refused rather than given a new host identity.
    pub(crate) fn load_or_create(
        root: &Path,
        machine: &str,
        mint: bool,
        source: &dyn SealKeySource,
    ) -> Result<Self> {
        let dir = root.join(DIRECTORY);
        let blob = dir.join(secret_vault::VAULT_FILE);
        let context = Context::new(Label::CloudHostSecret, machine.as_bytes());
        match read_private(&blob) {
            Ok(sealed) => {
                let key = source.load_or_create(&dir)?;
                let opened = envelope::open(&key, context, &sealed)
                    .map_err(|_| anyhow!("sealed host secret refused"))?;
                let secret = <[u8; 32]>::try_from(opened.as_slice())
                    .map_err(|_| anyhow!("sealed host secret refused"))?;
                Ok(Self(Zeroizing::new(secret)))
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if !mint {
                    bail!("sealed host secret missing; refusing a new host identity")
                }
                create_private_dir(root, &dir)?;
                let key = source.load_or_create(&dir)?;
                let fresh = SymmetricKey::generate().context("host secret entropy unavailable")?;
                let sealed = envelope::seal(&key, context, fresh.expose_secret())
                    .map_err(|_| anyhow!("host secret sealing failed"))?;
                private_fs::create_atomic(&blob, &sealed)
                    .context("sealed host secret write failed")?;
                private_fs::sync_directory(&dir).context("sealed host secret flush failed")?;
                // A concurrent creator may have published first; always
                // return what reached disk.
                Self::load_or_create(root, machine, false, source)
            }
            Err(error) => Err(anyhow::Error::new(error).context("sealed host secret unreadable")),
        }
    }
}

fn read_private(path: &Path) -> std::io::Result<Vec<u8>> {
    use std::io::Read as _;
    let mut bytes = Vec::new();
    private_fs::read(path)?.take(4096).read_to_end(&mut bytes)?;
    Ok(bytes)
}

fn create_private_dir(root: &Path, dir: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        match std::fs::DirBuilder::new().mode(0o700).create(dir) {
            Ok(()) => private_fs::sync_directory(root)?,
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error.into()),
        }
        Ok(())
    }
    #[cfg(not(unix))]
    {
        let _ = (root, dir);
        bail!("sealed host secret requires Unix storage")
    }
}

#[cfg(test)]
mod tests;

/// Copy the sealed secret of the Store at `from` to the Store at `to`, as a
/// byte-for-byte replacement of the state directory would.
#[cfg(test)]
pub(crate) fn copy_for_test(from: &Path, to: &Path) {
    let dir = to.join(DIRECTORY);
    create_private_dir(to, &dir).unwrap();
    for name in [secret_vault::VAULT_FILE, secret_vault::KEY_FILE] {
        let bytes = read_private(&from.join(DIRECTORY).join(name)).unwrap();
        private_fs::write_atomic(&dir.join(name), &bytes).unwrap();
    }
}
