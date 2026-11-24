//! An app's resources moved into the workspace's store read what the app reads there: a root of
//! the platform's packages it does not run (`use tilo.workspace`, `workspace.members`), which
//! the seed that fills the store and every action invoked on it after read.

use crate::fixtures::{Replica, Workspace};
use tilo_service::Service as _;
use tilolang_service::ResourceService;

/// The app: its one action reads the workspace's members, a root of the package it names with
/// `use tilo.workspace`.
const APP: &str = "test.members";
const SOURCE: (&str, &str) = (
    r#"app test.members "Members"

use tilo.workspace as workspace

action anyone() -> Bool {
    not workspace.members.people.is_empty()
}
"#,
    "",
);

/// The workspace's store as the session hands it a service: the commands an app's resources are
/// kept by, answered by a workspace of this test's.
struct Store(Workspace);

impl tilo_service::WorkspaceStore for Store {
    fn command(&self, _app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: collab_types::store::wire::Command =
            serde_json::from_slice(command).map_err(|error| error.to_string())?;
        let reply = self.0.command(command);
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }
}

/// A service moved into the workspace's store reads the workspace's members there: the packages
/// its app does not run are described again over the store it moved to, so the seed that fills
/// that store and every action invoked on it after read them as the app did on its own host.
#[test]
fn a_service_moved_into_the_workspace_reads_the_package_it_does_not_run() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[SOURCE]);
    let mut service = ResourceService::of_app(APP, ada.host, "ada");
    let store: std::rc::Rc<dyn tilo_service::WorkspaceStore> =
        std::rc::Rc::new(Store(Workspace::new()));
    service
        .keep_in_workspace(&store)
        .expect("the workspace takes the app's resources");
    let host = service.host();
    let package = host
        .package(APP)
        .expect("the app is loaded over the workspace");
    let action = package
        .meta
        .actions
        .iter()
        .find(|action| action.name == "anyone")
        .unwrap_or_else(|| {
            let held: Vec<(&str, &str)> = package
                .meta
                .actions
                .iter()
                .map(|action| (action.id.as_str(), action.name.as_str()))
                .collect();
            panic!("the app's action: {held:?}")
        })
        .id
        .clone();
    let actor = tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
    match host
        .runtime()
        .invoke_action(&actor, (action.as_str(), vec![]))
    {
        Ok(tilolang_runtime::InvokeOutcome::Done(anyone)) => assert_eq!(
            anyone,
            tilolang_runtime::Value::Bool(false),
            "the action ran, and nobody is in the workspace yet"
        ),
        other => panic!("the moved service reads the workspace's members: {other:?}"),
    }
}
