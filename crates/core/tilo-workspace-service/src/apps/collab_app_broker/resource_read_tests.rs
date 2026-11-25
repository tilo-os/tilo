//! The record change api: an agent acting for its person changes what the workspace lets it, at
//! the record's origin, and nobody else changes records here.

use std::sync::Mutex;

use super::*;
use crate::apps::collab_app_broker::person_id_for_root;
use crate::apps::services::{DocumentChangeServices, ProcessServices};
use crate::apps::test_support::{
    Grantee, Grants as _, KindSet, TreePath, WorkspaceId, WorkspaceTree,
};

const NOTES: &str = "so.tilo.notes";
const MAIL: &str = "so.tilo.mail";

/// The workspace's origin, recording every change it was handed and who it was for.
#[derive(Default)]
struct Origin(
    Mutex<
        Vec<(
            String,
            tilo_actions::remote::ResourceChange,
            tilo_actions::ActionInvoker,
        )>,
    >,
);

impl DocumentChangeServices for Origin {
    fn apply(
        &self,
        workspace: &str,
        change: &tilo_actions::remote::ResourceChange,
        invoker: &tilo_actions::ActionInvoker,
    ) -> Result<serde_json::Value, String> {
        self.0
            .lock()
            .unwrap()
            .push((workspace.to_owned(), change.clone(), invoker.clone()));
        Ok(serde_json::json!({"ok": {"value": null, "resources": []}, "differs": false}))
    }
}

struct Fixture {
    state: Arc<AppBrokerState>,
    origin: Arc<Origin>,
    person: String,
    workspace: String,
}

/// A workspace its person owns, with agent `scribe` admitted, which keeps Mail's changes from it.
fn fixture() -> Fixture {
    let person = person_id_for_root([7; 32]);
    let mut workspace =
        crate::apps::test_support::Workspace::new(WorkspaceId::from_u128(71), "Record changes");
    workspace.policy.set_owner(person.clone());
    let workspace_id = workspace.id().to_string();
    let scribe = || Grantee::agent("scribe", person.as_str());
    workspace.allow(scribe(), &TreePath::root(), KindSet::READ);
    workspace.allow_app(scribe(), NOTES, KindSet::ALL);
    workspace.deny(scribe(), &TreePath::root().app(MAIL), KindSet::UNDOABLE);
    let mut tree = WorkspaceTree::default();
    tree.desktop.workspaces.push(workspace);
    let membership = tree.shared();
    let origin = Arc::new(Origin::default());
    let services = ProcessServices {
        resource_changes: Some(Arc::clone(&origin) as Arc<dyn DocumentChangeServices>),
        ..membership.services()
    };
    let broker = AppBrokerState::test_broker_with_authority(membership.authority(), services);
    Fixture {
        state: Arc::clone(broker.state()),
        origin,
        person,
        workspace: workspace_id,
    }
}

impl Fixture {
    /// The agent `agent` acting for the person, as the desktop's api admits it.
    fn agent(&self, agent: &str) -> Caller {
        Caller::local_agent(agent, self.person.clone())
            .in_workspace(self.workspace.clone())
            .in_new_execution()
    }
}

fn change(app: &str) -> tilo_actions::remote::ResourceChange {
    tilo_actions::remote::ResourceChange {
        app: app.to_owned(),
        record: String::new(),
        resource: format!("tl:{app}:root"),
        action: "new_note".into(),
        arguments: serde_json::Map::new(),
        preview: None,
    }
}

tilo_test_env::scenario! {
    fn records_are_changed_here_only_by_an_agent_for_its_person;

    phase an_agents_change_reaches_the_origin_acting_for_its_person;
    {
        let f = fixture();
        let changes = f.state.resource_changes().expect("a host with an origin");
        let answer = changes.apply(&f.agent("scribe"), &change(NOTES)).unwrap();
        assert_eq!(answer["differs"], false);
        let handed = f.origin.0.lock().unwrap();
        let (workspace, applied, invoker) = handed.first().expect("the origin was handed the change");
        assert_eq!(workspace, &f.workspace);
        assert_eq!(applied, &change(NOTES));
        assert_eq!(
            invoker.acting.by,
            tilo_actions::Actor::member(tilo_actions::MemberId::agent("scribe", f.person.as_str()))
        );
        assert!(invoker.acting.is_agent());
        assert_eq!(invoker.acting.acted_for_person(), Some(f.person.as_str()));
    }

    phase a_change_the_workspace_denies_the_agent_never_reaches_the_origin;
    {
        let f = fixture();
        let changes = f.state.resource_changes().unwrap();
        assert!(matches!(
            changes.apply(&f.agent("scribe"), &change(MAIL)),
            Err(ActionCallError::Refused { .. })
        ));
        assert!(f.origin.0.lock().unwrap().is_empty());
    }

    phase only_an_agent_acting_for_its_person_changes_records_here;
    {
        let f = fixture();
        let changes = f.state.resource_changes().unwrap();
        let person = Caller::local_person(f.person.clone())
            .in_workspace(f.workspace.clone())
            .in_new_execution();
        for caller in [person.clone(), person.delegated_to("scribe")] {
            assert!(matches!(
                changes.apply(&caller, &change(NOTES)),
                Err(ActionCallError::Refused { reason }) if reason == tilo_actions::remote::RESOURCE_READS_FOR_AGENTS
            ));
        }
        assert!(f.origin.0.lock().unwrap().is_empty());
    }
}
