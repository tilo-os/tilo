//! Read-only native compute discovery. Presence and app publication are not registrations.
use super::{COLLECTION, PublishedAction};

pub(super) fn action() -> PublishedAction {
    super::action(
        COLLECTION,
        "read-task-compute",
        "Tasks",
        "Read configured task compute",
        Vec::new(),
        true,
    )
}

pub(crate) fn read(
    service: &dyn crate::services::WorkspaceMemberships,
    room: &str,
    caller: &tilo_user_identity::DeviceCert,
    current_agents: &std::collections::BTreeSet<String>,
) -> Result<serde_json::Value, String> {
    let registrations = service.task_compute(room, caller)?;
    let configured_model = service.task_compute_configuration(room, caller)?;
    let mut workers = Vec::new();
    if let Some(registrations) = registrations {
        let mut counts = std::collections::BTreeMap::new();
        for entry in &registrations {
            let count = counts.entry(entry.agent_id.as_str()).or_insert(0usize);
            *count = count.saturating_add(1);
        }
        for entry in &registrations {
            if entry.workspace != room
                || !current_agents.contains(&entry.agent_id)
                || counts.get(entry.agent_id.as_str()) != Some(&1)
                || !entry.model_ready
                || !entry.permitted
                || entry.profile.trim().is_empty()
                || configured_model.as_deref() != Some(entry.profile.as_str())
            {
                continue;
            }
            workers.push(serde_json::json!({
                "agent_id": entry.agent_id, "workspace": entry.workspace,
                "profile": entry.profile, "eligible": entry.current_certified_executor,
                "configured": true, "registered": entry.current_certified_executor,
            }));
        }
    }
    let available = workers.iter().any(|worker| worker["eligible"] == true);
    let status = if available {
        "available"
    } else if configured_model.is_some() {
        "needsExecutorSetup"
    } else {
        "needsConfiguration"
    };
    Ok(serde_json::json!({
        "setupRequired": !available,
        "status": status,
        "configuredModel": configured_model,
        "settings": "settings://agent_models",
        "workers": workers,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::services::{NativeTaskComputeRegistration, WorkspaceMemberships};

    struct Discovery(Vec<NativeTaskComputeRegistration>);
    impl WorkspaceMemberships for Discovery {
        fn requires_room(&self, room: &str) -> bool {
            room == "room"
        }
        fn task_snapshot(
            &self,
            _: &str,
        ) -> Result<tilo_peer_state::workspace_task::WorkspaceTaskSnapshot, String> {
            Err("discovery must not read task state".into())
        }
        fn execute(
            &self,
            _: tilo_owner_commit::OwnerCommitRequest,
        ) -> tilo_owner_commit::CommitFuture {
            panic!("discovery must not mutate task state")
        }
        fn task_compute(
            &self,
            _: &str,
            _: &tilo_user_identity::DeviceCert,
        ) -> Result<Option<Vec<NativeTaskComputeRegistration>>, String> {
            Ok(Some(self.0.clone()))
        }
        fn task_compute_configuration(
            &self,
            _: &str,
            _: &tilo_user_identity::DeviceCert,
        ) -> Result<Option<String>, String> {
            Ok(Some("Configured model".into()))
        }
    }

    #[test]
    fn configured_model_is_not_an_available_executor_without_every_current_binding() {
        let valid = NativeTaskComputeRegistration {
            agent_id: "scribe".into(),
            workspace: "room".into(),
            profile: "Configured model".into(),
            current_certified_executor: true,
            model_ready: true,
            permitted: true,
        };
        let root = ed25519_dalek::SigningKey::from_bytes(&[91; 32]);
        let device = ed25519_dalek::SigningKey::from_bytes(&[92; 32]);
        let caller = tilo_joined_room::testing::certificate(&root, &device, "Person");
        for invalid in 0..9 {
            let mut entry = valid.clone();
            let mut current = std::collections::BTreeSet::from(["scribe".into()]);
            match invalid {
                0 => entry.workspace = "other-room".into(),
                1 => current.clear(),
                2 => entry.current_certified_executor = false,
                3 => entry.model_ready = false,
                4 => entry.permitted = false,
                5 => entry.profile = " \t".into(),
                6 | 7 => {}
                8 => entry.profile = "Stale model".into(),
                _ => panic!("unexpected compute configuration state"),
            }
            let entries = match invalid {
                6 => vec![entry.clone(), entry],
                7 => Vec::new(),
                _ => vec![entry],
            };
            let value = read(&Discovery(entries), "room", &caller, &current).unwrap();
            assert_eq!(value["status"], "needsExecutorSetup", "case {invalid}");
            assert_eq!(value["setupRequired"], true, "case {invalid}");
            if invalid == 2 {
                assert_eq!(value["workers"][0]["agent_id"], "scribe");
                assert_eq!(value["workers"][0]["registered"], false);
                assert_eq!(value["workers"][0]["eligible"], false);
            } else {
                assert_eq!(value["workers"], serde_json::json!([]), "case {invalid}");
            }
            assert_eq!(value["configuredModel"], "Configured model");
        }
    }
}
