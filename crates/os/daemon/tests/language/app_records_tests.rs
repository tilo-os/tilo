//! Installing an app into a workspace gives it its own records, as a grant the workspace records
//! like any other: the app's trigger writes them with nobody at it, reaches nothing of another
//! app's, stops when a member revokes the grant (and says so in its window, in words a person
//! reads), and keeps them again once it is installed again. No grant is made by hand here.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    clippy::indexing_slicing,
    reason = "in a test these are the assertion, not a hazard"
)]

use std::sync::Arc;

use tilo_action_policy::{Answer, Authority, Context, TreePath, WorkspacePolicy, roles};
use tilo_action_types::ActionKind;
use tilo_member_types::Acting;
use tilo_workspace_service::tl_records::{Compiled, Package};
use tilo_workspace_service::workspaces::Workspaces;
use tilo_workspace_service::workspaces::fixtures::{
    TestGroupKeys, declare_app, hosting_device, install_app, revoke_grant, test_host_holding,
    uninstall_app,
};
use tilolang_workspace::records::{Packages, built_in, member, with_tl_record};

use super::mount;
use super::wakeups_tests::{APP, alarm};

/// The workspace's policy as this device holds it now.
async fn policy(workspaces: &Workspaces, workspace: &str) -> WorkspacePolicy {
    workspaces
        .policy_of(workspace)
        .await
        .expect("the workspace's policy is held here")
}

/// What `app`, acting by itself, is answered for changing what is at `node`: the decision every
/// admission of its writes makes.
fn app_may_change(policy: &WorkspacePolicy, app: &str, node: &TreePath) -> Answer {
    Authority::of_acting(&Acting::by_app(app), policy, &[])
        .decide(
            node,
            ActionKind::Undoable,
            &Context::at_time(tilo_clock::wall()),
        )
        .answer
}

/// Run what Clocks' triggers have next, as the device whose turn it is: how many ran, and what
/// Clocks said in its window meanwhile.
async fn ring_next(
    workspaces: &Workspaces,
    workspace: &str,
    packages: &Packages,
    person: &str,
) -> (usize, Vec<String>) {
    let who = member(tilo_actions::MemberId::person(person.to_owned()));
    with_tl_record(workspaces, workspace, packages, (who, person), |opened| {
        let host = opened.host();
        let now = tilo_clock::unix_now_ms_i64() / 1000;
        let at = host
            .next_trigger(now)
            .ok_or("no alarm of Clocks is to ring")?;
        let ran = host.run_triggers(at);
        let said = host
            .runtime()
            .take_said()
            .into_iter()
            .map(|said| said.text)
            .collect();
        Ok((ran, said))
    })
    .await
    .unwrap()
}

// USER REQUIREMENT: an app I install keeps its own records by itself (its alarms ring and write
// what they rang) without my granting it anything by hand; I see that permission among the
// workspace's grants and can take it back; then the app stops and tells me plainly it can't keep
// its records, rather than failing silently; installing it again gives the permission back.
#[tokio::test]
async fn installing_an_app_gives_it_its_own_records_until_a_member_takes_them_back() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let desk = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = desk.workspaces().clone();
    mount(&workspaces);
    let person = hex::encode(cert.user_public_key);
    let workspace = workspaces
        .create("Home")
        .await
        .expect("a workspace is made")
        .workspace;
    assert!(workspaces.join_keys(&workspace).await);
    let packages = built_in(APP).unwrap().expect("Clocks is built in");
    declare_app(
        &desk,
        APP,
        &packages.surface(APP).expect("Clocks declares its surface"),
    );
    let own = TreePath::workspace(&workspace).app(APP);
    let grant = roles::app_records_grant_id(APP);
    assert_eq!(
        app_may_change(&policy(&workspaces, &workspace).await, APP, &own),
        Answer::Deny,
        "an app nobody installed keeps nothing"
    );

    // The person installs Clocks: the workspace records the grant installing gives, as theirs.
    install_app(&workspaces, &workspace, APP)
        .await
        .expect("the person installs Clocks");
    let installed = policy(&workspaces, &workspace).await;
    let given = installed
        .grants
        .iter()
        .find(|held| held.id == grant)
        .expect("installing records the grant of Clocks' own records");
    assert!(roles::is_app_records_grant(given), "{given:?}");
    assert_eq!(given.granted_by.person_id(), Some(person.as_str()));
    assert_eq!(app_may_change(&installed, APP, &own), Answer::Allow);
    // Another app's records are no part of it: a write there by Clocks alone is not allowed.
    let tasks = TreePath::workspace(&workspace)
        .app("so.tilo.tasks")
        .kind("tl.so.tilo.tasks.Task");
    assert_ne!(
        app_may_change(&installed, APP, &tasks),
        Answer::Allow,
        "Clocks reaches no other app's records by being installed"
    );

    // A write of Clocks' records that names no one who made it is refused before it is signed:
    // it would be decided as the person whose device signed it, with their rights.
    let nameless = collab_types::store::wire::Command::Put {
        kind: "tl.so.tilo.clocks.Alarm".to_owned(),
        id: "nameless".to_owned(),
        resource: collab_types::store::Resource {
            readers: String::new(),
            parent: None,
            order: None,
            fields: std::collections::BTreeMap::new(),
        },
        stamp: tilo_resource_store::Stamp::default(),
    };
    match workspaces
        .record_command(&workspace, APP, &person, nameless)
        .await
    {
        Err(tilo_workspace_service::workspaces::RecordFailure::Refused(why)) => {
            assert!(why.contains("names no one"), "{why}");
        }
        other => panic!("a write naming no one is refused: {other:?}"),
    }

    // Its trigger writes its own record, the alarm's notice, with nobody at it.
    alarm(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("Standup", 6, 0, ""),
    )
    .await;
    let (ran, said) = ring_next(&workspaces, &workspace, &packages, &person).await;
    assert_eq!(ran, 1, "the trigger wrote Clocks' own record: {said:?}");
    assert!(said.is_empty(), "nothing went wrong to say: {said:?}");
    let notices = workspaces
        .app_notices(
            &workspace,
            APP,
            Some(&Package::Compiled(Compiled::new(packages.clone()))),
            &person,
        )
        .await
        .expect("Clocks' notices read");
    assert!(
        notices.iter().any(|notice| notice.title == "Standup"),
        "the alarm's notice is Clocks' record: {notices:?}"
    );

    // A member revokes the grant, as the workspace's Permissions tab does.
    revoke_grant(&workspaces, &workspace, grant.clone())
        .await
        .expect("the owner revokes Clocks' grant");
    let revoked = policy(&workspaces, &workspace).await;
    assert!(!revoked.grants.iter().any(|held| held.id == grant));
    assert_eq!(app_may_change(&revoked, APP, &own), Answer::Deny);

    // The next write Clocks makes by itself is refused, and Clocks says why in its window.
    alarm(
        &workspaces,
        &workspace,
        &packages,
        &person,
        ("Gym", 7, 0, ""),
    )
    .await;
    let (ran, said) = ring_next(&workspaces, &workspace, &packages, &person).await;
    assert_eq!(ran, 0, "the workspace refuses what Clocks writes by itself");
    assert!(
        said.iter()
            .any(|text| text.contains("Clocks") && text.contains("can't keep its records")),
        "Clocks tells the person it can't keep its records: {said:?}"
    );

    // Installed again, it keeps its records again: the alarm that was refused rings.
    uninstall_app(&workspaces, &workspace, APP)
        .await
        .expect("the person uninstalls Clocks");
    assert!(
        !policy(&workspaces, &workspace)
            .await
            .grants
            .iter()
            .any(|held| held.id == grant),
        "uninstalling takes the grant with it"
    );
    install_app(&workspaces, &workspace, APP)
        .await
        .expect("the person installs Clocks again");
    assert_eq!(
        app_may_change(&policy(&workspaces, &workspace).await, APP, &own),
        Answer::Allow,
        "installing again gives the grant again"
    );
    let (ran, said) = ring_next(&workspaces, &workspace, &packages, &person).await;
    assert_eq!(ran, 1, "Clocks writes its own record again: {said:?}");
    assert!(said.is_empty(), "{said:?}");
}

// USER REQUIREMENT: what an app writes by itself in the background (a timer, a sync, anything
// nobody asked it for just then) is the app's doing, never mine because I happen to have its
// window open: it keeps only what I let the app keep, and stops when I take that back.
#[tokio::test]
async fn a_native_apps_own_write_is_the_apps_and_stops_when_its_grant_is_taken_back() {
    use collab_types::store::{Cell, Resource, Value, wire::Command};
    use tilo_app_host::services::ResourceRequestContext;
    use tilo_resource_store::Stamp;

    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let desk = test_host_holding(dir.path(), &key, &cert, Arc::new(TestGroupKeys::default())).await;
    let workspaces = desk.workspaces().clone();
    mount(&workspaces);
    let workspace = workspaces
        .create("Home")
        .await
        .expect("a workspace is made")
        .workspace;
    assert!(workspaces.join_keys(&workspace).await);
    let packages = built_in(APP).unwrap().expect("Clocks is built in");
    declare_app(
        &desk,
        APP,
        &packages.surface(APP).expect("Clocks declares its surface"),
    );
    install_app(&workspaces, &workspace, APP)
        .await
        .expect("the person installs Clocks");

    // Clocks' own process's route on this device, its person's window open: the caller the
    // device certifies is the person, through Clocks. No invocation is delivered to it.
    let route = ResourceRequestContext::authenticated(
        APP,
        workspace.clone(),
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(workspace.clone())
            .through_app(APP),
    )
    .of_app_process();
    let city = |id: &str, name: &str| Command::Put {
        kind: "tl.so.tilo.clocks.WorldClock".into(),
        id: id.into(),
        resource: Resource {
            readers: "*".into(),
            parent: None,
            order: None,
            fields: [
                ("city".to_owned(), Cell::One(Value::Text(name.into()))),
                (
                    "zone".to_owned(),
                    Cell::One(Value::Text("Asia/Tokyo".into())),
                ),
            ]
            .into(),
        },
        stamp: Stamp {
            verb: "$create".into(),
            invocation: format!("background-{id}"),
            ..Stamp::default()
        },
    };
    workspaces
        .resource_command(&route, city("tokyo", "Tokyo"))
        .await
        .expect("Clocks keeps its own record by itself");

    // Taken back, its next background write is refused, though the owner it would otherwise
    // pass for may write anything here.
    revoke_grant(&workspaces, &workspace, roles::app_records_grant_id(APP))
        .await
        .expect("the owner revokes Clocks' grant");
    let refused = workspaces
        .resource_command(&route, city("osaka", "Osaka"))
        .await
        .expect_err("the workspace refuses what Clocks writes by itself");
    assert!(
        refused.contains("Clocks") && refused.contains("can't keep its records"),
        "{refused}"
    );
}
