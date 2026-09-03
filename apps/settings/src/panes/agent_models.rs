//! Agent Models — a personal, device-local view of model-provider profiles.
//!
//! Agent permissions remain workspace-scoped. This pane only configures where
//! an already-authorised agent may obtain inference. It reads the same typed
//! profile document as the agent runner, never a second UI-only representation.

use agent_provider_config::{EndpointLocation, ProviderConfig, ProviderProfile, ProviderProtocol};
use ui::widget::{Button, Row, Spacer, Text, Widget};

use crate::widgets::{caption, card, card_with_action, hgap, rows, value_row};

fn save_profile(profile: ProviderProfile) {
    let mut config = ProviderConfig::load();
    // Adding is idempotent at the profile-id level and cannot reveal or
    // overwrite a credential: `profile` contains a reference, not a key.
    config.upsert(profile);
    let _ = config.save();
}

fn select_default(profile_id: String) {
    let mut config = ProviderConfig::load();
    // A profile with no discovered model cannot become an executable default;
    // keeping the profile visible is useful, but guessing a model id is not.
    if config
        .profiles
        .iter()
        .any(|profile| profile.id == profile_id && profile.default_model.is_some())
    {
        config.default_profile = Some(profile_id);
        let _ = config.save();
    }
}

fn add_actions() -> Box<dyn Widget> {
    Box::new(
        Row::new()
            .push(Box::new(
                Button::new("agent-models-add-anthropic", "Add Anthropic", None).on_effect(|| {
                    save_profile(ProviderProfile::hosted(ProviderProtocol::Anthropic));
                }),
            ))
            .push(hgap(8.0))
            .push(Box::new(
                Button::new("agent-models-add-openai", "Add OpenAI-compatible", None).on_effect(
                    || {
                        save_profile(ProviderProfile::hosted(ProviderProtocol::OpenAiCompatible));
                    },
                ),
            ))
            .push(hgap(8.0))
            .push(Box::new(
                Button::new("agent-models-add-vllm", "Add local vLLM", None).on_effect(|| {
                    save_profile(ProviderProfile::local_vllm());
                }),
            ))
            .push(Box::new(Spacer::new())),
    )
}

/// Build the profile overview from durable configuration. The add shortcuts
/// create safe starting profiles; model discovery and credential consent are
/// then performed by the agent runtime through its resource/secret portals.
pub fn build() -> Box<dyn Widget> {
    let config = ProviderConfig::load();
    let mut surfaces = vec![card(
        "Where agent inference runs",
        rows(vec![
            caption(
                "A provider is personal to this device. It does not grant an agent access to a workspace, app, file, or message — those grants stay explicit on each task.",
            ),
            value_row(
                "Default",
                config
                    .active_profile()
                    .map_or("No default provider", ProviderProfile::display_label),
            ),
            caption(
                "Only endpoint metadata and a system-vault reference are saved here. API keys are never typed into, stored in, or replicated through Settings.",
            ),
        ]),
    )];
    surfaces.push(card(
        "Add a provider",
        rows(vec![
            caption("Start from a known wire protocol. Local vLLM uses the OpenAI-compatible API but is visibly marked as running on this device."),
            add_actions(),
        ]),
    ));
    if config.profiles.is_empty() {
        surfaces.push(card(
            "No providers yet",
            rows(vec![caption("Add a hosted provider or a local endpoint. Agents can still be present and collaborate without model inference." )]),
        ));
    }
    for profile in &config.profiles {
        let location = match profile.location {
            EndpointLocation::ThisDevice => "This device · local endpoint",
            other => other.label(),
        };
        let mut details = vec![
            value_row("Protocol", profile.protocol.label()),
            value_row("Location", location),
            value_row("Endpoint", &profile.endpoint),
            value_row(
                "Credential",
                if profile.credential.is_some() {
                    "System vault reference"
                } else {
                    "No credential configured"
                },
            ),
            value_row("Models", &profile.models.len().to_string()),
        ];
        if profile.is_local_vllm_candidate() {
            details.push(caption(
                "This profile stays local: qOS will not redirect it to a hosted service.",
            ));
        }
        details.push(caption("Model discovery, credential consent, and task-level model selection happen through the Agent Task Center so there is one audited execution surface."));
        if profile.default_model.is_some() {
            let profile_id = profile.id.clone();
            details.push(Box::new(
                Button::new(
                    &format!("agent-models-default-{profile_id}"),
                    if config.default_profile.as_deref() == Some(&profile.id) {
                        "Default for new tasks"
                    } else {
                        "Use by default"
                    },
                    None,
                )
                .on_effect(move || select_default(profile_id.clone())),
            ));
        }
        surfaces.push(card_with_action(
            profile.display_label(),
            Box::new(
                Text::new(if profile.enabled {
                    "Enabled"
                } else {
                    "Disabled"
                })
                .with_size(12.0),
            ),
            rows(details),
        ));
    }
    crate::widgets::cards(surfaces)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn local_vllm_profile_is_explicitly_local() {
        let profile = ProviderProfile::local_vllm();
        assert_eq!(profile.location, EndpointLocation::ThisDevice);
        assert!(profile.is_local_vllm_candidate());
    }
}
