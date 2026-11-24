//! The model a person chose in their computer's Agent models pane is the one their workspace's
//! agents answer on there: a model the computer runs itself, linked into the workspace's own model
//! configuration once for a choice made before, and followed each time the pane changes it. With
//! nothing linked, the agent says so in Chat, in the person's own language, and where to choose one.

use serde_json::json;
use tilo_workspace_service::workspaces::person_model::{
    ModelDescriptor, ProviderProfile, ProviderProtocol,
};
use tilo_workspace_service::workspaces::this_computer::Linking;

use crate::agent_fixture::act;
use crate::host;

/// The person chose a model Ollama runs on her computer in its Agent models pane: the choice is
/// linked into the workspace's own model configuration with no key, her agent's next answer comes
/// from that model, and the pane's next choice is followed. The daemon links its computer's own
/// choice as its settings change, so the pane's first choice is linked as a change here.
#[test]
fn the_model_chosen_in_a_computers_agent_models_pane_answers_its_agents() {
    host::run(|host| async move {
        let daemon = host.daemon();
        let workspaces = daemon.workspaces();
        // The pane's choice: Ollama on this computer's loopback, offering these models, of which
        // agents answer with `chosen`, and asked without a key.
        let choose = |models: &[&str], chosen: &str| {
            let profile = ProviderProfile {
                id: "ollama".to_owned(),
                label: "Ollama".to_owned(),
                protocol: ProviderProtocol::OpenAiCompatible,
                endpoint: "http://127.0.0.1:11434/v1".to_owned(),
                models: models
                    .iter()
                    .map(|model| ModelDescriptor {
                        tool_calling: true,
                        ..ModelDescriptor::new(*model)
                    })
                    .collect(),
                default_model: Some(chosen.to_owned()),
                ..ProviderProfile::local_vllm()
            };
            let mut config = workspaces.workspace_models(host.workspace());
            config.default_profile = Some(profile.id.clone());
            config.profiles = vec![profile];
            config
        };

        let choice = choose(&["llama3.2", "qwen3"], "llama3.2");
        assert!(
            workspaces
                .link_this_computer(host.workspace(), &choice, Linking::Chosen)
                .unwrap(),
            "the computer's choice becomes the workspace's"
        );
        let models = workspaces.workspace_models(host.workspace());
        assert_eq!(models.default_profile.as_deref(), Some("ollama"));
        assert_eq!(
            models
                .profiles
                .iter()
                .find(|profile| profile.id == "ollama")
                .and_then(|profile| profile.default_model.as_deref()),
            Some("llama3.2"),
            "the pane's model answers: {models:?}"
        );
        assert!(
            models
                .profiles
                .iter()
                .all(|profile| profile.credential.is_none()),
            "no key comes with it"
        );

        // Her agent answers on it: what she asks in Chat comes back from the model the pane chose.
        let agent = host.agent();
        let (reply, failed) = act(&agent, "chat#send", json!({"text": "what is planned?"})).await;
        assert!(
            !failed && reply.contains("the plan is ready"),
            "the workspace's agent answers on the model the pane chose: {reply}"
        );

        // She picks another model in the pane: the workspace, still on her computer's choice,
        // follows.
        let picked = choose(&["llama3.2", "qwen3"], "qwen3");
        assert!(
            workspaces
                .link_this_computer(host.workspace(), &picked, Linking::Following)
                .unwrap(),
            "the pane's next choice is followed"
        );
        let models = workspaces.workspace_models(host.workspace());
        assert_eq!(
            models
                .profiles
                .iter()
                .find(|profile| profile.id == "ollama")
                .and_then(|profile| profile.default_model.as_deref()),
            Some("qwen3"),
            "the workspace follows the pane: {models:?}"
        );
        let (reply, failed) = act(&agent, "chat#send", json!({"text": "who owns it?"})).await;
        assert!(
            !failed && reply.contains("Ada owns it"),
            "the model picked in the pane answers: {reply}"
        );
    });
}

/// Neither the person nor her computer linked a model: the agent's turn ends with a refusal in the
/// person's language, naming the Agent models pane where she chooses one.
#[test]
fn with_no_model_linked_the_agent_says_so_in_chat_in_its_persons_language() {
    host::run(|host| async move {
        let models = host
            .daemon()
            .workspaces()
            .workspace_models(host.workspace());
        assert_eq!(models.default_profile, None, "{models:?}");
        assert!(models.profiles.is_empty(), "no model is linked: {models:?}");

        // Ada writes to her workspace's agent in German, and the turn ends with the refusal in
        // German, naming the pane.
        let agent = host.agent();
        let (said, failed) = act(&agent, "chat#send", json!({"text": "Was steht an?"})).await;
        assert!(!failed, "the person writes to her agent: {said}");
        assert!(
            said.contains(
                "Für diese Art von Arbeit ist kein Modell verknüpft. Wähle eines in den \
                 Einstellungen unter Agentenmodelle."
            ),
            "the refusal reads in German and names the pane: {said}"
        );
    });
}
