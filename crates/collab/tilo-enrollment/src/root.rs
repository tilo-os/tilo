//! The person's root, held by this device, for enrollment.

use anyhow::{Context, bail};
use tilo_user_identity::StatementSigner;

/// The person's root held here. Every statement this crate issues is signed
/// through [`StatementSigner`], the one signing path a person's statements
/// share.
pub trait RootSigner: StatementSigner {}

impl RootSigner for ed25519_dalek::SigningKey {}

impl RootSigner for tilo_identity::store::SigningIdentity {}

/// Open the person's root from the active identity in their identity store,
/// the one place a root is kept.
///
/// # Errors
///
/// No identity directory is named, it holds no active identity, or that
/// identity cannot sign alone here.
pub fn load_root(identity_dir: Option<&std::path::Path>) -> anyhow::Result<Box<dyn RootSigner>> {
    const MISSING_ROOT: &str =
        "this device does not hold the identity root needed to approve enrollment";
    let identity_dir = identity_dir
        .with_context(|| format!("{MISSING_ROOT}: name the identity directory that holds it"))?;
    let file = tilo_identity_client::resolve_active_identity(identity_dir).with_context(|| {
        format!(
            "{MISSING_ROOT}: {} holds no active identity",
            identity_dir.display()
        )
    })?;
    let identity = tilo_identity::store::load_from_path(file.clone()).map_err(|error| {
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
