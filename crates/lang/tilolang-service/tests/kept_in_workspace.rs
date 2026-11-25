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
/// kept by, answered by a workspace of this test's, decided by Ada's access under its policy.
struct Store(Workspace, tilo_action_policy::WorkspacePolicy);

impl Store {
    /// Ada's own workspace: she is its only member.
    fn ada_alone() -> Self {
        Self(
            Workspace::new(),
            tilo_action_policy::WorkspacePolicy::new("workspace", "ada"),
        )
    }
}

impl tilo_service::WorkspaceStore for Store {
    fn command(&self, _app: &str, command: &[u8]) -> Result<Vec<u8>, String> {
        let command: collab_types::store::wire::Command =
            serde_json::from_slice(command).map_err(|error| error.to_string())?;
        let reply = self.0.command(command);
        serde_json::to_vec(&reply).map_err(|error| error.to_string())
    }

    fn access(&self) -> Option<tilo_action_policy::MemberAccess> {
        Some(tilo_action_policy::MemberAccess::person(self.1.clone(), "ada"))
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
        std::rc::Rc::new(Store::ada_alone());
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
            tilolang_runtime::Value::Bool(true),
            "the action ran, and read the workspace's members as its policy names them: Ada"
        ),
        other => panic!("the moved service reads the workspace's members: {other:?}"),
    }
}

/// An app whose first scenario writes a line as the person running it, then one as `by`, then
/// one as the person again.
fn talk(by: &str) -> String {
    format!(
        r#"app test.talk "Talk"

type Line {{
    said: Text
}}

type Talk {{
    lines: Collection<Line>
}}

root Talk

default view talk for Talk

view talk(t: Talk) {{
    text("{{t.lines.len()}} lines")
}}

action write(t: Talk, said: Text) -> Line {{
    t.lines.insert(Line(said: said))
}}

/// What the talk's lines say, oldest first.
action said() -> Text {{
    let t = root Talk
    t.lines.map(l -> l.said).join(" | ")
}}

scenario "a morning" {{
    let t = root Talk
    invoke(write(t, "Morning, from me"))
    invoke(write(t, "Morning, from someone else"), by: person("{by}"))
    invoke(write(t, "Anyone there?"))
}}
"#
    )
}

/// Seed `talk(by)`'s app into a workspace of `policy` as Ada, as a session seeds an app it
/// moves there: its first scenario, run as her. What the move was refused with, and what the
/// workspace's talk says after, as another of Ada's devices reads it.
fn seed_as_ada(by: &str, policy: tilo_action_policy::WorkspacePolicy) -> (String, String) {
    let source = talk(by);
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(source.as_str(), "")]);
    let mut service = ResourceService::of_app("test.talk", ada.host, "ada");
    service.set_seed(|host| {
        let package = host.package("test.talk").expect("the app is loaded");
        let scenario = package
            .meta
            .scenarios
            .first()
            .expect("the app has a scenario");
        host.root_of("test.talk")
            .map_err(|error| error.to_string())?;
        host.runtime()
            .invoke_action(
                &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person(
                    "ada",
                )),
                (scenario.action.as_str(), vec![]),
            )
            .map(|_| ())
            .map_err(|error| error.to_string())
    });
    let held = std::rc::Rc::new(Store(Workspace::new(), policy));
    let store: std::rc::Rc<dyn tilo_service::WorkspaceStore> = held.clone();
    let refused = service
        .keep_in_workspace(&store)
        .expect_err("the seed's step taken as someone else is refused");
    let (reader, _) = Replica::new("ada", held.0.clone(), &[(source.as_str(), "")]);
    let said = match crate::fixtures::call(&reader, "ada", "test.talk", "said", vec![]) {
        tilolang_runtime::Value::Text(said) => said,
        other => panic!("the talk's lines are text: {other:?}"),
    };
    (refused, said)
}

/// A seed into a person's workspace does only what they do: a step it takes as someone who is
/// not in the workspace is refused in the words of why, and the steps the person took before it
/// stand in the workspace.
#[test]
fn a_seed_step_as_someone_outside_the_workspace_is_refused_and_ones_own_steps_land() {
    let (refused, said) = seed_as_ada(
        "Mia",
        tilo_action_policy::WorkspacePolicy::new("workspace", "ada"),
    );
    assert!(
        refused.contains("can't write as Mia: Mia is not in this workspace"),
        "the refusal names the step, who it was to run as, and why: {refused}"
    );
    assert_eq!(
        said, "Morning, from me",
        "Ada's own step landed, and nothing after the refusal"
    );
}

/// Naming another member is no better: a seed may not record Bo's words when Bo didn't write
/// them.
#[test]
fn a_seed_step_as_another_member_is_refused_as_not_being_them() {
    let mut policy = tilo_action_policy::WorkspacePolicy::new("workspace", "ada");
    policy
        .members
        .insert("bo".to_owned(), tilo_action_policy::Role::Editor);
    let (refused, said) = seed_as_ada("bo", policy);
    assert!(
        refused.contains("can't write as ") && refused.contains("is not you, so this can't be done as them"),
        "a member who isn't the person running the seed is refused as not being them: {refused}"
    );
    assert_eq!(
        said, "Morning, from me",
        "Ada's own step landed, and nothing after the refusal"
    );
}
