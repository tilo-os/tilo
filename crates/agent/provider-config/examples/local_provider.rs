//! Add a keyless provider on this device and make it the one agents use, for a scripted session.
//!
//! `local_provider <endpoint> <model> [openai-compatible|anthropic]` writes the profile into the
//! settings of the profile `$XDG_CONFIG_HOME/tilo` names, as Settings' "Add provider" with LM
//! Studio, its address and model, and "Set default provider" do. A running session holding the
//! settings applies the write as its own. The endpoint must be a literal loopback address: the
//! model gateway sends a keyless profile nowhere else.

use agent_provider_config::{
    EndpointLocation, ModelDescriptor, ProviderConfig, ProviderProfile, ProviderProtocol,
};

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let (Some(endpoint), Some(model)) = (args.next(), args.next()) else {
        return Err(
            "usage: local_provider <endpoint> <model> [openai-compatible|anthropic]".into(),
        );
    };
    let protocol = match args.next().as_deref() {
        None | Some("openai-compatible") => ProviderProtocol::OpenAiCompatible,
        Some("anthropic") => ProviderProtocol::Anthropic,
        Some(other) => return Err(format!("unknown protocol `{other}`").into()),
    };
    let mut profile = ProviderProfile::local_vllm();
    "recorded-model".clone_into(&mut profile.id);
    "Recorded model".clone_into(&mut profile.label);
    profile.protocol = protocol;
    profile.endpoint = endpoint;
    profile.location = EndpointLocation::ThisDevice;
    profile.models = vec![ModelDescriptor {
        id: model.clone(),
        label: model.clone(),
        tool_calling: true,
        ..ModelDescriptor::default()
    }];
    profile.default_model = Some(model);
    let mut config = ProviderConfig::load();
    config.default_profile = Some(profile.id.clone());
    config.upsert(profile);
    if let Some(issue) = config.validation_errors().into_iter().next() {
        return Err(format!("the profile is not valid: {issue:?}").into());
    }
    config.save()?;
    println!("agents use {:?}", config.active_profile().map(|p| &p.label));
    Ok(())
}
