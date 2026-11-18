//! The person's root, held by this device, for enrollment.

use anyhow::{Context, bail};
use tilo_device_identity::IdentityPaths;
use tilo_user_identity::StatementSigner;

/// The person's root held here. Every statement this crate issues is signed
/// through [`StatementSigner`], the one signing path a person's statements
/// share.
pub trait RootSigner: StatementSigner {}

impl RootSigner for ed25519_dalek::SigningKey {}

impl RootSigner for runtime_identity::SigningIdentity {}

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
        let root = tilo_device_identity::persist::read_owned_signing_key(&paths.user_privkey)
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
