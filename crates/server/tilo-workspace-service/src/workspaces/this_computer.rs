//! The model a computer's Agent models pane chose, linked as the workspace's.
//!
//! A workspace's agents answer on the one model configuration it keeps ([`super::person_model`]).
//! On a person's computer that configuration is also where the computer's own Agent models pane
//! puts its choice: a model this computer runs itself (Ollama, llama.cpp, a server on its
//! network) joins the workspace's providers, and the workspace's work runs on it. Nothing reads
//! the computer's pane in its place: a choice that is not linked here answers nothing.
//!
//! A choice is linked in one of three ways ([`Linking`]). A computer that held a choice before
//! links it once into each workspace it keeps, filling only what the workspace has not chosen
//! itself. Afterwards a new choice in the pane follows into the workspaces that still run on
//! this computer's last one. And a person who wants their computer's choice in a workspace that
//! chose another says so ([`tilo_actions::remote::person_models::LINK_THIS_COMPUTER`]).
//!
//! Only a provider that is asked without a key is linked: a computer keeps no provider keys, and a
//! sign-in made on it stays on it. A choice of anything else is left where it is.

use std::path::Path;

use agent_provider_config::{
    AgentModel, EndpointLocation, Interaction, ProviderConfig, ProviderProfile,
};
use serde::{Deserialize, Serialize};
use settings_store::SettingsSource as _;

use super::Workspaces;
use super::person_settings::{provider_config, save_provider_config};

/// The section of a workspace's settings that keeps what this computer linked into it last.
const LINKED_SECTION: &str = "agent_providers_this_computer";
/// Its one leaf.
const LINKED_KEY: &str = "linked";

/// How a computer's choice is linked into a workspace.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Linking {
    /// The first time this computer links into the workspace: only what the workspace has not
    /// chosen is filled, and a workspace this computer linked into before is left as it is.
    Once,
    /// The pane's choice changed: it is linked where the workspace still runs on what this
    /// computer linked last, or on nothing.
    Following,
    /// The person asked for it: the choice is the workspace's, whatever it ran on before.
    Chosen,
}

/// What this computer linked into a workspace last: the default and the model of each kind.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct LinkedLast {
    default: Option<AgentModel>,
    kinds: std::collections::BTreeMap<String, AgentModel>,
}

/// The choice `computer` holds that a workspace can run on: the default provider and its model,
/// the model of each kind of work, and the providers they name. A provider that needs a key or a
/// sign-in, or is switched off, is no part of it.
struct Choice {
    default: Option<AgentModel>,
    kinds: Vec<(Interaction, AgentModel)>,
    profiles: Vec<ProviderProfile>,
}

impl Choice {
    fn of(computer: &ProviderConfig) -> Self {
        let linkable = |id: &str, model: &str| {
            computer
                .profiles
                .iter()
                .find(|profile| profile.id == id)
                .filter(|profile| {
                    profile.enabled
                        && profile.sign_in.is_none()
                        && profile.credential.is_none()
                        && profile.location != EndpointLocation::Hosted
                        && profile.models.iter().any(|known| known.id == model)
                })
        };
        let mut profiles: Vec<ProviderProfile> = Vec::new();
        let mut take = |profile: &ProviderProfile| {
            if !profiles.iter().any(|known| known.id == profile.id) {
                profiles.push(profile.clone());
            }
        };
        let default = computer.active_profile().and_then(|profile| {
            let model = profile.default_model.clone()?;
            let profile = linkable(&profile.id, &model)?;
            take(profile);
            Some(AgentModel {
                profile: profile.id.clone(),
                model,
            })
        });
        let kinds = Interaction::ALL
            .iter()
            .filter_map(|kind| {
                let chosen = computer.interaction_models.get(kind.id())?;
                take(linkable(&chosen.profile, &chosen.model)?);
                Some((*kind, chosen.clone()))
            })
            .collect();
        Self {
            default,
            kinds,
            profiles,
        }
    }
}

/// The workspace's default, as a choice names it.
fn default_of(config: &ProviderConfig) -> Option<AgentModel> {
    let profile = config.active_profile()?;
    Some(AgentModel {
        profile: profile.id.clone(),
        model: profile.default_model.clone()?,
    })
}

fn linked_last(root: &Path) -> Option<LinkedLast> {
    let store = settings_store::profile(root);
    let document = store.load();
    let value = document.get(LINKED_SECTION)?.get(LINKED_KEY)?.clone();
    match serde_json::from_value(value) {
        Ok(linked) => Some(linked),
        // Read as nothing linked yet: the computer's choice is linked again, once.
        Err(error) => {
            tracing::warn!(%error, "what this computer linked into a workspace is unreadable");
            None
        }
    }
}

fn keep_linked(root: &Path, linked: &LinkedLast) -> Result<(), String> {
    let store = settings_store::profile(root);
    let value = serde_json::to_value(linked).map_err(|error| error.to_string())?;
    store
        .update_section(
            LINKED_SECTION,
            serde_json::Map::from_iter([(LINKED_KEY.to_owned(), value)]),
        )
        .map_err(|error| error.to_string())
}

/// Link `computer`'s choice into the model configuration kept at `root`, as `how` says. Whether
/// the configuration changed.
fn link(root: &Path, computer: &ProviderConfig, how: Linking) -> Result<bool, String> {
    let before = linked_last(root);
    if how == Linking::Once && before.is_some() {
        return Ok(false);
    }
    let choice = Choice::of(computer);
    let mut config = provider_config(root);
    let was = config.clone();
    // What decides each slot: whether the workspace has chosen it itself.
    let takes = |current: Option<&AgentModel>, last: Option<&AgentModel>| match how {
        Linking::Chosen => true,
        Linking::Once => current.is_none(),
        Linking::Following => current.is_none() || current == last,
    };
    for profile in &choice.profiles {
        // A provider the workspace keeps already under this id keeps its own name and key.
        if let Some(known) = config
            .profiles
            .iter_mut()
            .find(|known| known.id == profile.id)
        {
            known.endpoint.clone_from(&profile.endpoint);
            known.protocol = profile.protocol;
            known.location = profile.location;
            known.models.clone_from(&profile.models);
            known.enabled = true;
        } else {
            let mut profile = profile.clone();
            profile.credential = None;
            config.profiles.push(profile);
        }
    }
    // A slot this computer does not take now keeps what it linked there last, so the workspace
    // follows it there only while it still runs on that.
    let mut linked = before.clone().unwrap_or_default();
    if let Some(default) = &choice.default
        && takes(
            default_of(&config).as_ref(),
            before.as_ref().and_then(|last| last.default.as_ref()),
        )
    {
        config.default_profile = Some(default.profile.clone());
        if let Some(profile) = config
            .profiles
            .iter_mut()
            .find(|profile| profile.id == default.profile)
        {
            profile.default_model = Some(default.model.clone());
        }
        linked.default = Some(default.clone());
    }
    for (kind, chosen) in &choice.kinds {
        let last = before.as_ref().and_then(|last| last.kinds.get(kind.id()));
        if takes(config.interaction_models.get(kind.id()), last) {
            config
                .set_interaction_model(*kind, Some(chosen.clone()))
                .map_err(str::to_owned)?;
            linked.kinds.insert(kind.id().to_owned(), chosen.clone());
        }
    }
    // A provider linked for no slot is not left behind in the workspace.
    config.profiles.retain(|profile| {
        was.profiles.iter().any(|known| known.id == profile.id)
            || linked
                .default
                .as_ref()
                .is_some_and(|d| d.profile == profile.id)
            || linked.kinds.values().any(|k| k.profile == profile.id)
    });
    let changed = config != was;
    if changed {
        save_provider_config(root, &config)?;
    }
    // Once is once: kept even where the workspace took nothing, so it is not asked again.
    if before.as_ref() != Some(&linked) {
        keep_linked(root, &linked)?;
    }
    Ok(changed)
}

impl Workspaces {
    /// Link `computer`'s Agent models choice into `workspace`'s model configuration on this
    /// computer, as `how` says ([`Linking`]). Whether the workspace's models changed. It reads
    /// and writes the settings from disk, so an asynchronous caller asks it off the runtime's
    /// workers.
    ///
    /// # Errors
    /// The workspace's settings could not be written.
    pub fn link_this_computer(
        &self,
        workspace: &str,
        computer: &ProviderConfig,
        how: Linking,
    ) -> Result<bool, String> {
        let root = self.workspace_root(workspace);
        std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
        let changed = link(&root, computer, how)?;
        if changed {
            self.person_settings_changed();
        }
        Ok(changed)
    }

    /// Link this computer's Agent models choice into every workspace this service keeps, once,
    /// and from then on each new choice the pane makes into the workspaces that still run on
    /// this computer's last ([`Linking`]). A workspace the service starts keeping later is linked
    /// once as it is held. The choice is read from this process's settings, the computer's own;
    /// it is followed for as long as the service runs.
    pub fn follow_this_computer(&self) {
        if let Some(source) = settings_store::source() {
            self.follow_choice_in(source);
        }
    }

    /// [`Self::follow_this_computer`], with the choice read from `source`.
    pub fn follow_choice_in(&self, source: std::sync::Arc<dyn settings_store::SettingsSource>) {
        let (ringer, mut rings) = tokio::sync::mpsc::unbounded_channel::<()>();
        let listener: std::sync::Arc<settings_store::Listener> = std::sync::Arc::new(move || {
            let _ = ringer.send(());
        });
        source.changes().listen(&listener);
        let mut holds = self.inner.readings.holds();
        let workspaces = self.clone();
        tokio::spawn(async move {
            // The settings ring it for as long as it is held.
            let _listening = listener;
            let mut linked = std::collections::BTreeSet::<String>::new();
            let mut seen: Option<ProviderConfig> = None;
            loop {
                holds.mark_unchanged();
                let held = workspaces.inner.readings.workspaces();
                let linking = workspaces.clone();
                let before = seen.clone();
                let known = linked.clone();
                let from = std::sync::Arc::clone(&source);
                let read = tokio::task::spawn_blocking(move || {
                    let computer = ProviderConfig::load_in(&*from);
                    // Every change of the settings rings; only a change of the choice follows.
                    let follows = before.as_ref().is_some_and(|before| *before != computer);
                    for workspace in &held {
                        let how = if !known.contains(workspace) {
                            Linking::Once
                        } else if follows {
                            Linking::Following
                        } else {
                            continue;
                        };
                        if let Err(error) = linking.link_this_computer(workspace, &computer, how) {
                            tracing::warn!(%workspace, %error, "this computer's model choice is not linked into a workspace");
                        }
                    }
                    (computer, held)
                })
                .await;
                match read {
                    Ok((computer, held)) => {
                        seen = Some(computer);
                        linked.extend(held);
                    }
                    Err(error) => {
                        tracing::warn!(%error, "this computer's model choice was not read");
                    }
                }
                tokio::select! {
                    heard = rings.recv() => {
                        if heard.is_none() {
                            return;
                        }
                        while rings.try_recv().is_ok() {}
                    }
                    held = holds.changed() => {
                        if held.is_err() {
                            return;
                        }
                    }
                }
            }
        });
    }

    /// `workspace`'s model configuration on this host: its providers, with what each offers,
    /// and the model each kind of its work runs on. No key: a provider names its vault entry.
    #[must_use]
    pub fn workspace_models(&self, workspace: &str) -> ProviderConfig {
        provider_config(&self.workspace_root(workspace))
    }
}

/// The model configuration of `workspace` that the workspace service keeping its state under
/// `service_root` holds: what [`Workspaces::workspace_models`] answers, for a reader beside that
/// service on the same computer.
#[must_use]
pub fn workspace_models_at(service_root: &Path, workspace: &str) -> ProviderConfig {
    provider_config(&workspace_root_at(service_root, workspace))
}

/// Link `computer`'s choice into `workspace` as [`Workspaces::link_this_computer`] does, for a
/// writer beside the workspace service keeping its state under `service_root` on the same
/// computer: one that holds the directory but not the service.
///
/// # Errors
/// The workspace's settings could not be written.
pub fn link_this_computer_at(
    service_root: &Path,
    workspace: &str,
    computer: &ProviderConfig,
    how: Linking,
) -> Result<bool, String> {
    let root = workspace_root_at(service_root, workspace);
    std::fs::create_dir_all(&root).map_err(|error| error.to_string())?;
    link(&root, computer, how)
}

/// Where the workspace service keeping its state under `service_root` keeps `workspace`'s
/// settings.
fn workspace_root_at(service_root: &Path, workspace: &str) -> std::path::PathBuf {
    super::person_asking::workspace_store_dir(&service_root.join("workspaces"), workspace)
}
