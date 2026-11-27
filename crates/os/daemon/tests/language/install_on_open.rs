//! Opening an app this computer holds but the workspace runs no version of installs it there,
//! as the person opening it, through the workspace's own `install`, and then opens it.

use serde_json::Value;
use tilo_action_policy::{Answer, Authority, Context, TreePath, roles};
use tilo_action_types::ActionKind;
use tilo_actions::ActionAddress;
use tilo_actions::remote::{
    APPLICATION_OPEN_ACTION, APPLICATION_RESOURCE_PREFIX, RemoteActionTarget, workspace_provider,
};
use tilo_member_types::Acting;

use crate::language::host::{self, Host};

/// Gallery: an app the computer holds that no workspace starts with.
const APP: &str = "so.tilo.lang.gallery";

/// The open of `APP` in `workspace`, as the shell and an agent's path ask it.
fn open(workspace: &str) -> RemoteActionTarget {
    RemoteActionTarget {
        provider: workspace_provider(workspace),
        address: ActionAddress::new(
            format!("{APPLICATION_RESOURCE_PREFIX}{APP}"),
            APPLICATION_OPEN_ACTION,
        ),
    }
}

/// Whether the workspace runs a version of `APP`, and the grant installing it gave, if any.
async fn installed(host: &Host) -> (bool, Option<tilo_action_policy::Grant>) {
    let daemon = host.daemon();
    let workspaces = daemon.workspaces();
    let runs = workspaces
        .app_versions(host.workspace())
        .await
        .expect("the workspace's apps read")
        .iter()
        .any(|installed| installed.release.app == APP);
    let grant = workspaces
        .policy_of(host.workspace())
        .await
        .expect("the workspace's policy is held here")
        .grants
        .into_iter()
        .find(|grant| grant.id == roles::app_records_grant_id(APP));
    (runs, grant)
}

// USER REQUIREMENT: an app my computer has opens in my workspace even though the workspace never
// ran it: opening it adds it, as me, and it works (its first write is kept).
#[test]
fn opening_an_app_the_workspace_does_not_run_installs_it_as_the_person() {
    host::run_with(&["gallery"], |host| async move {
        let workspace = host.workspace().to_owned();
        assert_eq!(
            installed(&host).await,
            (false, None),
            "no workspace starts with Gallery"
        );

        let held = package_store::Catalog::from_env()
            .expect("the device's package store reads")
            .find(APP)
            .expect("the store lists its packages");
        assert!(
            tilo_workspace_service::workspaces::held_release(APP).is_some(),
            "the computer holds a release of Gallery to install: {held:?}"
        );
        assert!(host.caller().acting().is_person(), "the person opens it");

        // The person opens it: it is installed as them, then opens.
        // What the open installed is checked before whether it opened: installing comes first.
        let opened = host::output(&host, open(&workspace), Value::Null).await;
        let (runs, grant) = installed(&host).await;
        assert!(runs, "the workspace runs Gallery now");
        let grant = grant.expect("installing gave Gallery its own records");
        assert_eq!(
            grant.granted_by.person_id(),
            Some(host.person()),
            "the workspace's history names who installed it"
        );
        let policy = host
            .daemon()
            .workspaces()
            .policy_of(&workspace)
            .await
            .expect("the workspace's policy is held here");
        assert_eq!(
            Authority::of_acting(&Acting::by_app(APP), &policy, &[])
                .decide(
                    &TreePath::workspace(&workspace).app(APP),
                    ActionKind::Undoable,
                    &Context::at_time(tilo_clock::wall()),
                )
                .answer,
            Answer::Allow,
            "Gallery's own first write is admitted"
        );

        opened.expect("the person's open of Gallery installs and opens it");

        // It runs in the workspace and publishes from its instance there. (Its first content is
        // made when its window draws its root, which a computer with no display never does: what
        // that write is decided by is the grant checked above.)
        let deadline = tilo_test_env::hang_deadline();
        loop {
            let page = host::listed(&host, APP).await.expect("Gallery lists");
            if host::running_record(&page, &workspace, APP).as_deref() == Some("") {
                break;
            }
            assert!(
                std::time::Instant::now() < deadline,
                "Gallery runs in the workspace on its own record: {:?}",
                page.providers
            );
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    });
}
