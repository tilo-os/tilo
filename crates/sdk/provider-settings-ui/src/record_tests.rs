//! Provider records as the settings host keeps them: two replicas changing
//! the same or different profiles, and the older list-shaped document they
//! migrate from.

use super::*;
use tilo_automerge::ApplyIncoming as _;
use ui::tilo_actions::Action as _;
use ui::tilo_resource::__private::{ROOT, ReadDoc as _};
use ui::tilo_resource::{Doc, Fields as _};

const CONFIG: &str = "config";

fn providers(config: ProviderConfig) -> AgentProviders {
    AgentProviders { config }
}

fn stored(providers: &AgentProviders) -> Doc {
    let mut doc = Doc::new();
    providers
        .write_fields(&mut doc, &ROOT)
        .expect("the configuration writes");
    doc.commit();
    doc
}

fn held(doc: &Doc) -> AgentProviders {
    AgentProviders::read_fields(doc, &ROOT).expect("the configuration reads")
}

#[allow(
    clippy::needless_pass_by_value,
    reason = "a test names the effect it invokes as the panel does"
)]
fn change(doc: &mut Doc, effect: ProviderEffect, listed: Option<ModelDescriptor>) {
    let operation = effect
        .agent_action(listed.as_ref())
        .expect("the effect changes provider metadata");
    ui::tilo_resource::Stamped {
        operation,
        now_ms: 0,
    }
    .apply(doc)
    .expect("the typed change applies");
    doc.commit();
}

fn two_profiles() -> ProviderConfig {
    let mut anthropic = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    anthropic.label = "Anthropic".into();
    anthropic.models = vec![ModelDescriptor::new("claude")];
    let mut local = ProviderProfile::local_vllm();
    local.label = "Local".into();
    local.models = vec![ModelDescriptor::new("llama")];
    ProviderConfig {
        default_profile: Some(anthropic.id.clone()),
        profiles: vec![anthropic, local],
        ..Default::default()
    }
}

fn profile<'a>(providers: &'a AgentProviders, label: &str) -> &'a ProviderProfile {
    providers
        .config
        .profiles
        .iter()
        .find(|profile| profile.label == label)
        .unwrap_or_else(|| panic!("the `{label}` provider remains"))
}

/// Two people changing different fields of one provider both keep their
/// change through the typed effects the panel publishes, not a whole
/// configuration write.
#[test]
fn same_provider_disjoint_fields_survive_the_production_effects() {
    let base = providers(two_profiles());
    let id = base.config.profiles[0].id.clone();
    let mut ours = stored(&base);
    let mut theirs = ours.fork();

    change(
        &mut ours,
        ProviderEffect::SetProviderDetails {
            provider: id.clone(),
            label: "Anthropic (work)".into(),
            endpoint: base.config.profiles[0].endpoint.clone(),
            model: None,
        },
        None,
    );
    change(
        &mut theirs,
        ProviderEffect::SetProviderEnabled {
            provider: id,
            on: false,
        },
        None,
    );
    ours.merge_incoming(&mut theirs).expect("the changes merge");

    let merged = held(&ours);
    let anthropic = profile(&merged, "Anthropic (work)");
    assert!(!anthropic.enabled, "the enabled flag keeps its own edit");
    assert_eq!(
        profile(&merged, "Local").endpoint,
        "http://127.0.0.1:8000/v1"
    );
}

/// Two people changing two providers both keep their change: one profile is
/// stored under its id, not at a position another change can move.
#[test]
fn different_providers_survive_the_production_effects() {
    let base = providers(two_profiles());
    let anthropic = base.config.profiles[0].id.clone();
    let local = base.config.profiles[1].id.clone();
    let mut ours = stored(&base);
    let mut theirs = ours.fork();

    change(
        &mut ours,
        ProviderEffect::SetProviderModel {
            provider: anthropic,
            model: "claude-opus".into(),
        },
        Some(ModelDescriptor::new("claude-opus")),
    );
    change(
        &mut theirs,
        ProviderEffect::SetProviderDetails {
            provider: local,
            label: "Local".into(),
            endpoint: "http://127.0.0.1:9000/v1".into(),
            model: Some("llama".into()),
        },
        None,
    );
    ours.merge_incoming(&mut theirs).expect("the changes merge");

    let merged = held(&ours);
    assert_eq!(
        profile(&merged, "Anthropic").default_model.as_deref(),
        Some("claude-opus")
    );
    assert_eq!(
        profile(&merged, "Local").endpoint,
        "http://127.0.0.1:9000/v1"
    );
}

/// One person removes a provider while another adds one and changes a third:
/// the removal is of that id alone, and the other changes both survive.
#[test]
fn add_remove_and_a_stale_field_keep_to_their_own_ids() {
    let base = providers(two_profiles());
    let anthropic = base.config.profiles[0].id.clone();
    let local = base.config.profiles[1].id.clone();
    let mut ours = stored(&base);
    let mut theirs = ours.fork();

    change(
        &mut ours,
        ProviderEffect::RemoveProvider {
            provider: anthropic.clone(),
        },
        None,
    );
    change(
        &mut theirs,
        ProviderEffect::SetProviderDetails {
            provider: local.clone(),
            label: "Local".into(),
            endpoint: "http://127.0.0.1:9100/v1".into(),
            model: Some("llama".into()),
        },
        None,
    );
    change(
        &mut theirs,
        ProviderEffect::AddProvider {
            preset: "ollama".into(),
        },
        None,
    );
    ours.merge_incoming(&mut theirs).expect("the changes merge");

    let merged = held(&ours);
    assert!(
        !merged
            .config
            .profiles
            .iter()
            .any(|profile| profile.id == anthropic),
        "the removed provider stays removed"
    );
    assert_eq!(
        profile(&merged, "Local").endpoint,
        "http://127.0.0.1:9100/v1"
    );
    assert!(
        merged
            .config
            .profiles
            .iter()
            .any(|profile| profile.id != anthropic && profile.id != local),
        "the added provider remains"
    );
}

/// A version-1 record stored each profile as one JSON string in a list. The
/// migration reads that same serde form and writes each profile under its
/// id, leaving the settings file's JSON unchanged.
#[test]
fn an_old_list_of_json_profile_records_migrates_to_keyed_profiles() {
    let profile = ProviderProfile::hosted(ProviderProtocol::Anthropic);
    let id = profile.id.clone();
    let old = serde_json::json!({
        "config": {
            "profiles": [serde_json::to_string(&profile).unwrap()],
            "default_profile": id,
            "agent_models": {}
        }
    });
    let mut doc = Doc::new();
    ui::tilo_resource::evolution::fixture(&mut doc, &ROOT, &old).unwrap();
    doc.commit();

    assert!(ui::tilo_resource::evolution::migrate::<AgentProviders>(&mut doc).unwrap());
    let migrated = held(&doc);
    assert_eq!(migrated.config.profiles.len(), 1);
    assert_eq!(migrated.config.profiles[0].id, id);
    assert_eq!(
        migrated.config.default_profile.as_deref(),
        Some(id.as_str())
    );

    // The keyed map is the document's shape; the file still serialises the
    // same list of profiles.
    let Some((ui::tilo_resource::__private::Value::Object(_), config)) =
        doc.get(&ROOT, CONFIG).unwrap()
    else {
        panic!("the configuration is a map");
    };
    let Some((
        ui::tilo_resource::__private::Value::Object(ui::tilo_resource::__private::ObjType::Map),
        profiles,
    )) = doc.get(&config, "profiles").unwrap()
    else {
        panic!("the profiles are keyed");
    };
    assert!(doc.get(&profiles, id.as_str()).unwrap().is_some());
    assert!(serde_json::to_value(&migrated.config).unwrap()["profiles"].is_array());
}

/// A record written after profiles became records but before they were
/// keyed stored them as a list of maps; it migrates the same way.
#[test]
fn an_old_list_of_profile_maps_migrates_to_keyed_profiles() {
    let profile = ProviderProfile::local_vllm();
    let id = profile.id.clone();
    let old = serde_json::json!({
        "config": {
            "profiles": [serde_json::to_value(&profile).unwrap()],
            "agent_models": {}
        }
    });
    let mut doc = Doc::new();
    ui::tilo_resource::evolution::fixture(&mut doc, &ROOT, &old).unwrap();
    doc.commit();

    assert!(ui::tilo_resource::evolution::migrate::<AgentProviders>(&mut doc).unwrap());
    let migrated = held(&doc);
    assert_eq!(migrated.config.profiles.len(), 1);
    assert_eq!(migrated.config.profiles[0].id, id);
}

/// A profile's own fields are still one record, so a change of one field
/// writes only that field (the model stores it, not one JSON string).
#[test]
fn a_keyed_profile_is_stored_as_its_fields() {
    let doc = stored(&providers(two_profiles()));
    let Some((ui::tilo_resource::__private::Value::Object(_), config)) =
        doc.get(&ROOT, CONFIG).unwrap()
    else {
        panic!("the configuration is a map");
    };
    let Some((
        ui::tilo_resource::__private::Value::Object(ui::tilo_resource::__private::ObjType::Map),
        profiles,
    )) = doc.get(&config, "profiles").unwrap()
    else {
        panic!("the profiles are keyed");
    };
    let key = doc.keys(&profiles).next().expect("a stored profile");
    let Some((ui::tilo_resource::__private::Value::Object(_), profile)) =
        doc.get(&profiles, key).unwrap()
    else {
        panic!("a provider is a map of its fields");
    };
    assert!(doc.get(&profile, "label").unwrap().is_some());
}
