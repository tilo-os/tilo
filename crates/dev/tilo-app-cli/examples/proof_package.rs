//! Package an explicit existing builder payload for the isolated desktop proof.
//! Uses the normal source declaration collector; never invents resource kinds.
use std::{
    io::Write as _,
    path::{Path, PathBuf},
};

fn main() -> anyhow::Result<()> {
    let args = std::env::args_os()
        .skip(1)
        .map(PathBuf::from)
        .collect::<Vec<_>>();
    let [
        app,
        manifest_path,
        payload,
        identity_path,
        output,
        extra @ ..,
    ] = args.as_slice()
    else {
        anyhow::bail!(
            "expected app source, manifest, payload, signing identity, new output, optional attestor identity"
        );
    };
    if extra.len() > 1 || output.exists() || !args.iter().all(|path| path.is_absolute()) {
        anyhow::bail!("fixture paths must be absolute and output must not exist");
    }
    let mut manifest =
        tilo_app_package::Manifest::from_toml_str(&std::fs::read_to_string(manifest_path)?)?;
    manifest.resources.kinds = tilo_app_cli::declared_resource_kinds(app)?;
    let profile = identity_path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("fixture identity parent missing"))?;
    if !profile
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name.starts_with("tilo-headless."))
        || std::env::var_os("XDG_CONFIG_HOME").map(PathBuf::from) != Some(profile.join("config"))
    {
        anyhow::bail!("proof package requires its isolated headless profile");
    }
    let identity = load_identity(identity_path)?;
    let public = hex::encode(identity.public_key_bytes());
    let pin = profile.join("runtime-publishers.txt");
    if pin.exists() {
        if std::fs::read_to_string(&pin)?.trim() != public {
            anyhow::bail!("fixture runtime pin changed");
        }
    } else {
        writeln!(std::fs::File::create_new(pin)?, "{public}")?;
    }
    manifest.publisher.identity = format!("ed25519:{public}");
    let initial = tilo_app_package::pack(payload, &manifest, &identity)?;
    let packed = if let Some(attestor_path) = extra.first() {
        attest_agent(
            payload,
            &mut manifest,
            &identity,
            &initial,
            attestor_path,
            profile,
        )?
    } else {
        initial
    };
    let mut writer = tilo_app_package::tapp::write(
        std::io::BufWriter::new(std::fs::File::create_new(output)?),
        &packed.meta,
        Some(&packed),
    )?;
    writer.flush()?;
    Ok(())
}

fn load_identity(path: &Path) -> anyhow::Result<tilo_app_package::SigningIdentity> {
    let file: serde_json::Value = serde_json::from_slice(&std::fs::read(path)?)?;
    let seed = file
        .get("secret_key_hex")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| anyhow::anyhow!("fixture signing identity has no key"))?;
    let seed: [u8; 32] = hex::decode(seed)?
        .try_into()
        .map_err(|_| anyhow::anyhow!("fixture signing key has wrong length"))?;
    Ok(tilo_app_package::SigningIdentity::from_seed(&seed))
}

fn attest_agent(
    payload: &Path,
    manifest: &mut tilo_app_package::Manifest,
    publisher: &tilo_app_package::SigningIdentity,
    initial: &tilo_app_package::Packed,
    attestor_path: &Path,
    profile: &Path,
) -> anyhow::Result<tilo_app_package::Packed> {
    let attestor = load_identity(attestor_path)?;
    let trust_path = tilo_app_package::default_trusted_attestors_path()
        .ok_or_else(|| anyhow::anyhow!("fixture trust path missing"))?;
    if manifest.package.id != "so.tilo.agent"
        || attestor_path.parent() != Some(profile)
        || trust_path != profile.join("config/tilo/trusted-attestors.toml")
        || trust_path.exists()
        || attestor.public_key_bytes() == publisher.public_key_bytes()
    {
        anyhow::bail!(
            "fixture attestation requires distinct setup key, exact Agent and empty scratch trust"
        );
    }
    let id = "isolated-desktop-proof";
    let trusted = tilo_app_package::TrustedAttestors {
        attestor: vec![tilo_app_package::TrustedAttestor {
            id: id.into(),
            pubkey_hex: hex::encode(attestor.public_key_bytes()),
            scope: tilo_app_package::AttestorScope::Prefix(vec!["so.tilo.agent".into()]),
        }],
    };
    let preimage = tilo_app_package::attestation_preimage(
        &initial.meta.root.payload_root_hash,
        &publisher.public_key_bytes(),
    );
    manifest
        .trust
        .attested_by
        .push(tilo_app_package::manifest::TrustAttestation {
            attestor: id.into(),
            claim_hex: String::new(),
            signature_hex: hex::encode(attestor.sign_message(&preimage)?),
        });
    let packed = tilo_app_package::pack(payload, manifest, publisher)?;
    if !matches!(
        tilo_app_package::verify_with_trust(
            &packed.meta,
            &[],
            &trusted.attestor,
            &tilo_app_package::RevocationList::empty()
        ),
        tilo_app_package::Verification::AttestedBy { .. }
    ) {
        anyhow::bail!("fixture attestation failed normal independent trust verification");
    }
    trusted.save(&trust_path)?;
    Ok(packed)
}
