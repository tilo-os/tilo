//! An app's resources moved into the workspace's store read what the app reads there: a root of
//! the platform's packages it does not run (`use tilo.workspace`, `workspace.members`), which
//! the seed that fills the store and every action invoked on it after read. What the app hands its
//! outside service stays routed to it across the move.

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
    /// A workspace governed by `policy`, its people in its members' resource as the workspace
    /// would list them: this test's store keeps no governance of its own.
    fn governed(policy: tilo_action_policy::WorkspacePolicy) -> Self {
        let workspace = Workspace::new();
        let host = tilolang_tilo::host::Host::open(
            workspace.store(),
            tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            None,
            || None,
        )
        .expect("a host over the workspace");
        crate::fixtures::stage_people(&host, &policy);
        Self(workspace, policy)
    }

    /// Ada's own workspace: she is its only member.
    fn ada_alone() -> Self {
        Self::governed(tilo_action_policy::WorkspacePolicy::new("workspace", "ada"))
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
        Some(tilo_action_policy::MemberAccess::person(
            self.1.clone(),
            "ada",
        ))
    }
}

/// A service moved into the workspace's store reads the workspace's members there: the packages
/// its app does not run are described again over the store it moved to, so the seed that fills
/// that store and every action invoked on it after read them as the app did on its own host.
#[test]
fn a_service_moved_into_the_workspace_reads_the_package_it_does_not_run() {
    let (ada, _) = Replica::new("ada", Workspace::new(), &[SOURCE]);
    let mut service = ResourceService::of_app(APP, ada.host, "ada");
    let store: std::rc::Rc<dyn tilo_service::WorkspaceStore> = std::rc::Rc::new(Store::ada_alone());
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

/// The talk app's ids, which every compile of it shares: a root is found by its type's id, so
/// the session that seeds it and the one that opens it again read one talk.
const TALK_IDS: &str = "Line = tlkline1
Line.said = tlksaid1
Talk = tlktalk1
Talk.lines = tlklins1
";

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
fn seed_as_ada(
    by: &str,
    policy: tilo_action_policy::WorkspacePolicy,
) -> (Result<bool, String>, String) {
    let source = talk(by);
    let (ada, _) = Replica::new("ada", Workspace::new(), &[(source.as_str(), TALK_IDS)]);
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
    let held = std::rc::Rc::new(Store::governed(policy));
    let store: std::rc::Rc<dyn tilo_service::WorkspaceStore> = held;
    let refused = service.keep_in_workspace(&store);
    // The workspace as Ada's next session opens it: the app's service moved into the same store,
    // which holds the app's resources now, so it seeds nothing.
    let (again, _) = Replica::new("ada", Workspace::new(), &[(source.as_str(), TALK_IDS)]);
    let mut opened = ResourceService::of_app("test.talk", again.host, "ada");
    opened
        .keep_in_workspace(&store)
        .expect("the workspace opens again");
    let host = opened.host();
    let said = host
        .package("test.talk")
        .and_then(|package| {
            package
                .meta
                .actions
                .iter()
                .find(|action| action.name == "said")
                .map(|action| action.id.clone())
        })
        .expect("the app's action that reads its lines");
    let said = match host.runtime().invoke_action(
        &tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        (said.as_str(), vec![]),
    ) {
        Ok(tilolang_runtime::InvokeOutcome::Done(tilolang_runtime::Value::Text(said))) => said,
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
    let refused = refused.expect_err("the seed's step taken as someone else is refused");
    assert!(
        refused.starts_with("the scenario stopped at a step it may not take here: "),
        "the seed answers that it stopped there, apart from a failure: {refused}"
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
    let refused = refused.expect_err("the seed's step taken as someone else is refused");
    assert!(
        refused.starts_with("the scenario stopped at a step it may not take here: "),
        "the seed answers that it stopped there, apart from a failure: {refused}"
    );
    assert!(
        refused.contains("can't write as ")
            && refused.contains("is not you, so this can't be done as them"),
        "a member who isn't the person running the seed is refused as not being them: {refused}"
    );
    assert_eq!(
        said, "Morning, from me",
        "Ada's own step landed, and nothing after the refusal"
    );
}

/// The app whose action hands an effect to its outside service, and the service's name.
const OUTSIDE: &str = "test.outside";

/// The outside service a moved app still reaches, over the connection it was handed before the
/// move: a service reaches no network of its own, so the connection is the only way out.
#[cfg(unix)]
mod outside {
    use std::cell::RefCell;
    use std::rc::Rc;
    use std::sync::{Arc, Mutex, OnceLock};

    use tilo_service::Service as _;
    use tilolang_tilo::origin::{Origin, OriginCx, Polled};
    use tilolang_tilo::runtime::Delivery;

    use super::{OUTSIDE, Replica, ResourceService, Store, Workspace};

    const SOURCE: (&str, &str) = (
        r#"app test.outside "Outside"

type Item {
    key: Text merge register
}

root Item

default view item_page for Item

view item_page(d: Item) {
    ui.Text(d.key)
}

action ask(d: Item) {
    outside("post", "ada")
}
"#,
        "",
    );

    /// The capability the app hands outside: one the platform declares, which this test's origin
    /// takes instead of the development log's sink.
    const CAPABILITY: &str = "post";

    /// What the test origin was handed: the capability, and what it was asked for.
    type Handed = Arc<Mutex<Vec<(String, serde_json::Value)>>>;

    /// The one recorder this test's origin writes to.
    fn handed() -> &'static Handed {
        static HANDED: OnceLock<Handed> = OnceLock::new();
        HANDED.get_or_init(Handed::default)
    }

    /// The origin "test", reached over a connection this test hands the service: it holds what the
    /// app handed it outside, and asks nothing back.
    struct TestOrigin;

    impl Origin for TestOrigin {
        fn name(&self) -> &'static str {
            "test"
        }

        fn serves(&self, capability: &str) -> bool {
            capability == CAPABILITY
        }

        fn due(&self) -> bool {
            false
        }

        fn poll(&mut self, _cx: &OriginCx<'_>) -> Polled {
            Polled::default()
        }

        fn deliver(&mut self, capability: &str, payload: &serde_json::Value) -> Delivery {
            handed()
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push((capability.to_owned(), payload.clone()));
            Delivery::Pending
        }
    }

    /// The client a crate registers for the origin "test"
    /// ([`tilolang_tilo::origin::register_remote`]).
    const TEST_CLIENT: &tilolang_tilo::origin::Remote = &|_stream, _wake| {
        Ok(Rc::new(RefCell::new(TestOrigin)) as tilolang_tilo::origin::SharedOrigin)
    };

    /// A service moved into the workspace's store keeps the connection it reached its outside
    /// service over: what its app's actions hand outside still goes there, so the service's answer
    /// reaches the record the app keeps in the workspace.
    #[test]
    fn a_service_moved_into_the_workspace_still_hands_its_outside_effects_to_its_origin() {
        tilolang_tilo::origin::register_remote("test", TEST_CLIENT);
        let (ada, _) = Replica::new("ada", Workspace::new(), &[SOURCE]);
        let mut service = ResourceService::of_app(OUTSIDE, ada.host, "ada");
        // The service reaches its outside service over a connection of its own, as the session
        // hands it one: the far end is this test's origin.
        let (_ours, theirs) = tilo_socket::pair().expect("a socket pair");
        service
            .connect_origin("test", theirs.into())
            .expect("the origin is reached");
        let store: Rc<dyn tilo_service::WorkspaceStore> = Rc::new(Store::ada_alone());
        service
            .keep_in_workspace(&store)
            .expect("the workspace takes the app's resources");
        let package = service
            .host()
            .package(OUTSIDE)
            .expect("the app is loaded over the workspace");
        let action = package
            .meta
            .actions
            .iter()
            .find(|action| action.name == "ask")
            .expect("the app's action")
            .id
            .clone();
        let actor =
            tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"));
        let root = service.host().root_of(OUTSIDE).expect("the app's root");
        service
            .host()
            .runtime()
            .invoke_action(
                &actor,
                (
                    action.as_str(),
                    vec![tilolang_runtime::Value::resource(&root)],
                ),
            )
            .expect("the action runs");
        // An effect is marked as handed over before it goes, so it takes two passes to be handed
        // to the origin.
        service.poll();
        service.poll();
        let handed = handed()
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone();
        assert!(
            handed
                .iter()
                .any(|(capability, payload)| capability == CAPABILITY
                    && payload == &serde_json::json!("ada")),
            "the moved service handed its outside effect to its origin: {handed:?}"
        );
    }
}

/// A seed whose every step is the person's own runs whole: each of their lines is in the
/// workspace, in order.
#[test]
fn a_seed_taken_wholly_as_the_person_lands_whole() {
    let (kept, said) = seed_as_ada(
        "ada",
        tilo_action_policy::WorkspacePolicy::new("workspace", "ada"),
    );
    kept.expect("the workspace takes a seed of the person's own steps");
    assert_eq!(
        said, "Morning, from me | Morning, from someone else | Anyone there?",
        "every step of the person's own landed"
    );
}
