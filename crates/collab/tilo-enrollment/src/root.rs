//! The person's root, held by this device, for enrollment.

use anyhow::{Context, bail};
use tilo_user_identity::{IdentityPaths, StatementSigner};

/// The person's root held here, on top of the identity crate's signer: every
/// statement this crate issues is signed through [`StatementSigner`], and the
/// pairing handshake's answer, which is domain-separated bytes rather than a
/// statement's digest, is signed by [`RootSigner::sign_message`].
pub trait RootSigner: StatementSigner {
    /// Ed25519-sign `message` as the person's root, without exporting it.
    ///
    /// # Errors
    ///
    /// The key is locked, was refused by whoever holds it, or failed.
    fn sign_message(&self, message: &[u8]) -> anyhow::Result<[u8; 64]>;
}

impl RootSigner for ed25519_dalek::SigningKey {
    fn sign_message(&self, message: &[u8]) -> anyhow::Result<[u8; 64]> {
        use ed25519_dalek::Signer as _;
        Ok(self.sign(message).to_bytes())
    }
}

impl RootSigner for runtime_identity::SigningIdentity {
    fn sign_message(&self, message: &[u8]) -> anyhow::Result<[u8; 64]> {
        self.sign(message)
            .map_err(|error| anyhow::anyhow!("sign with the identity file: {error}"))
    }
}

/// Open the root signer, falling back to an identity file only for an absent seed.
///
/// # Errors
///
/// This device holds no identity root, or the identity file it holds cannot sign.
pub fn load_root(
    identity_root: &std::path::Path,
    identity_dir: Option<&std::path::Path>,
) -> anyhow::Result<Box<dyn RootSigner>> {
    const MISSING_ROOT: &str =
        "this device does not hold the identity root needed to approve enrollment";
    let paths = IdentityPaths::under(identity_root);
    let seed_absent = matches!(
        std::fs::symlink_metadata(&paths.user_privkey),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound
    );
    let Some(identity_dir) = identity_dir.filter(|_| seed_absent) else {
        let root = tilo_user_identity::persist::read_owned_signing_key(&paths.user_privkey)
            .context(MISSING_ROOT)?;
        return Ok(Box::new(root));
    };
    let file = runtime_identity::resolve_active_identity(identity_dir).with_context(|| {
        format!(
            "{MISSING_ROOT}: {} holds no active identity",
            identity_dir.display()
        )
    })?;
    let identity = runtime_identity::load_from_path(file.clone()).map_err(|error| {
        anyhow::anyhow!(
            "{MISSING_ROOT}: cannot open {}: {error} (start this as the account that owns the \
             identity, in a session that can reach its secret store)",
            file.display()
        )
    })?;
    if !identity.is_single_device() {
        bail!(
            "{}",
            tilo_identity::certification_ceremony::cannot_sign_alone()
        );
    }
    Ok(Box::new(identity))
}
