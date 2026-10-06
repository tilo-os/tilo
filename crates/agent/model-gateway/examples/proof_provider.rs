//! Configure the bounded loopback provider used by the isolated desktop proof.
//! This is an explicit example, not a live service or app configuration path.
//! It uses the ordinary profile and encrypted-vault formats; agents still
//! reach the real gateway through their launcher-issued descriptor.

use accounts::{AccountId, Secret, SecretVault};
use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, SecretReference,
};
use std::path::PathBuf;

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args_os().skip(1);
    let root = PathBuf::from(args.next().ok_or("expected empty config root")?);
    let endpoint = args.next().ok_or("expected loopback provider URL")?;
    if args.next().is_some() || !root.is_absolute() {
        return Err("expected exactly an absolute config root and loopback URL".into());
    }
    let endpoint = url::Url::parse(&endpoint.to_string_lossy())?;
    if endpoint.scheme() != "http" || endpoint.host_str() != Some("127.0.0.1") {
        return Err("proof provider must be an HTTP IPv4 loopback endpoint".into());
    }
    let settings = root.join("tilo/settings.json");
    if settings.exists() || root.join("tilo/accounts").exists() {
        return Err("proof refuses existing settings or account state".into());
    }
    if accounts::config_root().as_deref() != Some(root.join("tilo").as_path()) {
        return Err("proof config root must match its isolated session environment".into());
    }
    let profile_id = "desktop-proof";
    let model_id = "bounded-fixture";
    let config = ProviderConfig {
        default_profile: Some(profile_id.into()),
        profiles: vec![ProviderProfile {
            id: profile_id.into(),
            label: "Isolated desktop proof".into(),
            endpoint: endpoint.to_string(),
            location: EndpointLocation::ThisDevice,
            credential: Some(SecretReference::provider_api_key(profile_id)),
            enabled: true,
            models: vec![ModelDescriptor::new(model_id)],
            default_model: Some(model_id.into()),
            ..ProviderProfile::default()
        }],
    };
    if !config.validation_errors().is_empty() {
        return Err("proof provider profile failed ordinary validation".into());
    }
    let mut vault = SecretVault::open();
    vault.put(
        &AccountId::new(format!("agent-provider-{profile_id}")),
        &Secret::password("isolated-fixture-not-a-real-provider-key"),
    );
    vault.try_save()?;
    config.save_at(&settings)?;
    println!("configured ordinary loopback profile and encrypted fixture credential");
    Ok(())
}
