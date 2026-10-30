//! Give a configuration's person a model provider without Settings' window,
//! through what Settings' provider changes write: the profile into their
//! provider settings ([`ProviderConfig::save`]), the key into this gateway's
//! credential custody ([`super::set_key`]). A scripted session
//! (`dist/dev/nested-session.sh --model-provider`) uses it, from the
//! developer's key or their own default provider ([`export`]).
//!
//! A key only ever crosses stdin and stdout, never an argument or a log line.

use std::io::{Read as _, Write as _};

use agent_provider_config::presets::Preset;
use agent_provider_config::{ModelDescriptor, ProviderConfig, ProviderProfile};
use zeroize::Zeroizing;

/// The most of stdin read: a key, or a record holding one profile.
const MAX_INPUT_BYTES: u64 = 256 * 1024;

/// One provider and its key, as [`export`] writes it and
/// [`provision`] with `--record` reads it.
#[derive(serde::Serialize, serde::Deserialize)]
struct Record {
    profile: ProviderProfile,
    key: Option<String>,
}

fn read_stdin() -> Result<Zeroizing<String>, String> {
    let mut input = Zeroizing::new(String::new());
    std::io::stdin()
        .take(MAX_INPUT_BYTES)
        .read_to_string(&mut input)
        .map_err(|error| format!("reading stdin: {error}"))?;
    Ok(input)
}

/// Add the provider to this configuration's settings, make it the default,
/// and keep its key: `preset` with `model` and the key on stdin (none for a
/// keyless preset), or with `record`, [`export`]'s output on stdin.
///
/// # Errors
/// An unknown or sign-in preset, an invalid profile, a keyed provider given
/// no key, or settings or custody that cannot be written.
pub(super) fn provision(
    preset: Option<&str>,
    model: Option<&str>,
    record: bool,
) -> Result<(), String> {
    let input = read_stdin()?;
    let (profile, key) = if record {
        let record: Record = serde_json::from_str(input.trim())
            .map_err(|_| "stdin holds no provider record".to_string())?;
        let key = record.key.map(Zeroizing::new);
        (record.profile, key)
    } else {
        let slug = preset.ok_or("name a preset")?;
        let model = model.ok_or("name a model")?;
        let preset = Preset::from_slug(slug)
            .filter(|preset| {
                !matches!(
                    preset.authentication(),
                    agent_provider_config::presets::Authentication::SignIn(_)
                )
            })
            .ok_or_else(|| format!("no API-key or keyless preset `{slug}`"))?;
        let mut profile = preset.profile();
        profile.models = vec![ModelDescriptor {
            id: model.to_owned(),
            label: model.to_owned(),
            tool_calling: true,
            ..ModelDescriptor::default()
        }];
        profile.default_model = Some(model.to_owned());
        let key = Some(Zeroizing::new(input.trim().to_owned())).filter(|key| !key.is_empty());
        (profile, key)
    };
    if profile.credential.is_some() && key.is_none() {
        return Err(format!(
            "{} takes an API key; none was given",
            profile.label
        ));
    }
    let id = profile.id.clone();
    let label = profile.label.clone();
    let mut config = ProviderConfig::load();
    // One provider of a kind: a second provisioning replaces the first.
    let replaced: Vec<String> = config
        .profiles
        .iter()
        .filter(|held| held.id != id && held.label == label && held.endpoint == profile.endpoint)
        .map(|held| held.id.clone())
        .collect();
    for held in replaced {
        config.remove(&held);
    }
    config.upsert(profile);
    config.default_profile = Some(id.clone());
    if let Some(issue) = config.validation_errors().into_iter().next() {
        return Err(format!("the provider is not valid: {issue:?}"));
    }
    config
        .save()
        .map_err(|error| format!("saving the provider settings: {error}"))?;
    if let Some(key) = key {
        super::set_key(&id, key.to_string())?;
    }
    println!("agents use {label}");
    Ok(())
}

/// Write this configuration's default provider, and its key where it keeps
/// one, to stdout as a [`Record`].
///
/// # Errors
/// No default provider, a signed-in one (its grant stays on this device), or
/// a key custody cannot answer.
pub(super) fn export() -> Result<(), String> {
    let config = ProviderConfig::load();
    let profile = config
        .active_profile()
        .cloned()
        .ok_or("this configuration has no default provider")?;
    if profile.sign_in.is_some() {
        return Err(format!(
            "{} is signed in to, not keyed: sign in in the session instead",
            profile.label
        ));
    }
    let key = match profile.credential {
        Some(_) => Some(
            super::resolve_key(&profile)
                .map_err(|_| format!("{} has no stored key here", profile.label))?,
        ),
        None => None,
    };
    let record = Zeroizing::new(
        serde_json::to_string(&Record {
            profile,
            key: key.as_ref().map(|key| key.to_string()),
        })
        .map_err(|error| error.to_string())?,
    );
    let mut out = std::io::stdout().lock();
    out.write_all(record.as_bytes())
        .and_then(|()| out.flush())
        .map_err(|error| format!("writing the record: {error}"))
}
