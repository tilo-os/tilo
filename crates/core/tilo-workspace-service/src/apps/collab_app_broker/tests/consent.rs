#![allow(
    clippy::similar_names,
    reason = "sk/pk and owner/editor pairings are the conventional, intentional names for a keypair and a two-role fixture"
)]
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::unreachable,
    clippy::panic_in_result_fn,
    clippy::arithmetic_side_effects,
    clippy::missing_errors_doc,
    clippy::missing_panics_doc,
    clippy::wildcard_imports,
    clippy::used_underscore_binding,
    clippy::needless_pass_by_value,
    clippy::too_many_lines,
    clippy::items_after_statements,
    clippy::significant_drop_tightening,
    clippy::future_not_send,
    clippy::large_futures,
    clippy::await_holding_lock,
    clippy::await_holding_refcell_ref,
    reason = "test scenarios and fixtures: a failed unwrap is the assertion, and each scenario keeps its setup, guards and awaits in one readable body"
)]

use super::*;

#[tokio::test(flavor = "current_thread")]
async fn approval_publication_is_target_scoped_and_rejects_republish_and_peer_reuse() {
    tokio::task::LocalSet::new()
        .run_until(async {
            for directory in [false, true] {
                let fixture = local_directory_fixture();
                let person = person_id_for_root([0; 32]);
                let mut other_workspace = crate::apps::test_support::Workspace::new(
                    crate::apps::test_support::WorkspaceId::from_u128(72),
                    "Other publication workspace",
                );
                other_workspace.policy.set_owner(person.clone());
                let other_workspace_id = other_workspace.id().to_string();
                {
                    let mut tree = fixture.membership.as_ref().unwrap().write();
                    tree.workspaces[0].policy.set_owner(person);
                    tree.workspaces.push(other_workspace);
                }
                let target = fixture.invocation_channel();
                approval_publish(&fixture, &target, directory).await;
                let request = tilo_actions::remote::InvokeRequest {
                    invocation: tilo_actions::InvocationId::new(),
                    target: tilo_actions::remote::RemoteActionTarget {
                        provider: fixture.peer_id.to_string(),
                        address: tilo_actions::ActionAddress::new("document", "open"),
                    },
                    argument: tilo_actions::remote::InvokeArgument::None,
                    confirmation: None,
                    context: None,
                    revision: Some("directory-r1".into()),
                    wait_ms: None,
                    appearance: None,
                    on_behalf_of: None,
                    signed: None,
                };
                let binding = || {
                    fixture.state.approval_binding(
                        &tilo_actions::MemberId::person("person"),
                        &fixture.cluster_id,
                        &request,
                    )
                };
                let original = binding();
                let token = fixture.state.approval_confirmation(&original).unwrap();
                // Both another provider in this workspace and another workspace may publish,
                // reregister and disconnect while the person's sheet is open.
                for workspace in [fixture.cluster_id.clone(), other_workspace_id] {
                    let other_peer = fixture.peer_id + 1;
                    let route = CrdtRoute::record("org.example.unrelated", workspace, "main");
                    let other = fixture.invocation_channel_for(other_peer, &route);
                    for other_directory in [false, true] {
                        approval_publish(&fixture, &other, other_directory).await;
                        approval_publish(&fixture, &other, other_directory).await;
                    }
                    fixture.state.remove_connection(other_peer);
                }
                assert!(
                    fixture.state.take_approval(&token, &binding()),
                    "unrelated churn preserves consent"
                );

                let token = fixture.state.approval_confirmation(&original).unwrap();
                approval_publish(&fixture, &target, directory).await;
                assert!(
                    !fixture.state.take_approval(&token, &binding()),
                    "identical target republication revokes consent"
                );

                let current = binding();
                let token = fixture.state.approval_confirmation(&current).unwrap();
                fixture.state.remove_connection(fixture.peer_id);
                assert!(
                    !fixture.state.take_approval(&token, &binding()),
                    "disconnect revokes consent"
                );
                let token = fixture.state.approval_confirmation(&current).unwrap();
                approval_publish(&fixture, &target, directory).await;
                assert!(
                    !fixture.state.take_approval(&token, &binding()),
                    "peer reuse cannot restore old consent"
                );
            }
        })
        .await;
}
