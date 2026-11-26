#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::wildcard_imports,
    reason = "a test fixture: a failed unwrap is the assertion"
)]

//! A call made from a resource (`$from`) is admitted only for a caller who may read it.

use super::*;
use crate::test_support::{Grantee, KindSet, Role, TreePath};
use tilo_actions::params::{FROM_ARGUMENT, MadeFrom};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, RemoteActionTarget,
};

const CHAT: &str = "so.tilo.lang.chat";
const MESSAGE: &str = "tl.so.tilo.lang.chat.Message";
const TASKS: &str = "so.tilo.lang.tasks";
const TASK: &str = "tl.so.tilo.lang.tasks.Task";

/// A call of the fixture's app, made from `resource` of `kind`.
fn made_from(fixture: &LocalDirectoryFixture, kind: &str, resource: &str) -> InvokeRequest {
    let mut arguments = serde_json::Map::new();
    arguments.insert(
        FROM_ARGUMENT.to_owned(),
        MadeFrom {
            resource: resource.to_owned(),
            kind: kind.to_owned(),
        }
        .to_json(),
    );
    InvokeRequest {
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: fixture.peer_id.to_string(),
            address: tilo_actions::ActionAddress::new("workspace:atlas", "open"),
        },
        argument: InvokeArgument::Json(arguments),
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Bo may read the workspace's tasks but not its conversations: a call made from a message is
/// refused before it reaches any app, saying why, one made from a task is admitted past it, and
/// one whose kind names no app is refused.
#[tokio::test(flavor = "current_thread")]
async fn a_call_made_from_what_its_caller_may_not_read_is_refused() {
    tokio::task::LocalSet::new()
        .run_until(async {
            let fixture = local_directory_fixture();
            {
                let mut tree = fixture.membership.as_ref().unwrap().write();
                let workspace = &mut tree.workspaces[0];
                workspace.policy.add_member("bo".to_owned(), Role::Editor);
                workspace.deny(
                    Grantee::person("bo"),
                    &TreePath::root().app(CHAT),
                    KindSet::READ,
                );
            }
            let bo = Caller::local_person("bo").in_workspace(fixture.cluster_id.clone());

            let message = made_from(&fixture, MESSAGE, "m1");
            assert_eq!(message.made_from().unwrap().app(), Some(CHAT));
            let refused = fixture
                .drive(fixture.state.action_invoke(&bo, None, None, &message))
                .await;
            assert!(
                matches!(
                    &refused,
                    Err(ActionCallError::Refused { reason })
                        if reason == super::super::resource_read::MADE_FROM_UNREADABLE
                ),
                "{refused:?}"
            );

            let task = made_from(&fixture, TASK, "t1");
            assert_eq!(task.made_from().unwrap().app(), Some(TASKS));
            assert_eq!(
                fixture.state.may_make_from(&bo, &task),
                Ok(()),
                "a task Bo reads is his to make a call from"
            );
            let admitted = fixture
                .drive(fixture.state.action_invoke(&bo, None, None, &task))
                .await;
            assert!(
                !matches!(
                    &admitted,
                    Err(ActionCallError::Refused { reason })
                        if reason == super::super::resource_read::MADE_FROM_UNREADABLE
                ),
                "{admitted:?}"
            );

            let unnamed = made_from(&fixture, "a1b2c3", "m1");
            assert_eq!(
                fixture.state.may_make_from(&bo, &unnamed),
                Err(ActionCallError::Refused {
                    reason: super::super::resource_read::MADE_FROM_UNNAMED.into()
                }),
                "a resource named without its app is no one's to decide, so nothing runs"
            );
        })
        .await;
}
