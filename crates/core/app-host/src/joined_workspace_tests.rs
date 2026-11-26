//! A device that joined a workspace another device serves, as the apps it serves reach it: the
//! person's shell on a dispatch over the member copy, and an agent a program of theirs connects as.
#![allow(
    clippy::large_futures,
    clippy::significant_drop_tightening,
    reason = "a fixture failing is its test failing, and each test keeps the serving device to the end"
)]

use std::sync::Arc;

use tilo_user_identity::DeviceCert;
use tilo_workspace_service::workspaces::WorkspaceSummary;
use tilo_workspace_service::workspaces::fixtures::{TestHost, laptop, member_service, shared};

/// A shared workspace this device joined, and the person's shell on a dispatch over it.
async fn shell_on_a_joined_workspace(
    states: (&std::path::Path, &std::path::Path, &std::path::Path),
    seed: u8,
) -> (
    TestHost,
    TestHost,
    WorkspaceSummary,
    DeviceCert,
    runtime_collab::ShellClient,
    crate::collab_broker::BrokerHandle,
) {
    use runtime_collab::wire::shell::SHELL_MAGIC;
    use std::os::unix::fs::PermissionsExt as _;
    use tokio::io::AsyncWriteExt as _;
    let (serving_state, member_state, dir) = states;
    let (serving, device, created, invitation) = shared(serving_state, "Shared", seed).await;
    let member_cert = device.1.clone();
    let member = member_service(member_state, device).await;
    member.workspaces().join_member(&invitation).await.unwrap();

    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700)).unwrap();
    let socket = dir.join("collab.sock");
    let links = Arc::new(crate::shell::ShellLinks::default());
    let dispatch = crate::collab_broker::spawn_with_services(
        socket.clone(),
        None,
        crate::collab_outbound::PeerPublisher::disabled(),
        None,
        crate::services::ProcessServices {
            shell: Some(Arc::new(crate::shell::ShellServices {
                links: Arc::clone(&links),
                socket: socket.clone(),
                documents: Arc::new(crate::service_reads::ServiceDocuments::of(
                    member.workspaces().clone(),
                )),
                writes: Arc::new(crate::service_writes::ServiceWrites::of(
                    member.workspaces().clone(),
                )),
                document_writes: Arc::new({
                    let copies = member.workspaces().clone();
                    move || copies.document_writes()
                }),
                actions: Arc::new(NoActions),
                standing: Arc::new(NoStanding),
                published: None,
                workspaces: member.workspaces().clone(),
                identity: None,
                api: None,
                live: None,
                vpn: None,
                speaking: None,
            })),
            ..crate::services::ProcessServices::default()
        },
        Arc::default(),
        Arc::new(crate::collab_broker::FanoutState::new()),
    )
    .unwrap();
    let secret = links.make(crate::shell::ShellCaller {
        person: member_cert.user_public_key,
    });
    let mut link = tokio::net::UnixStream::connect(&socket).await.unwrap();
    let mut opening = SHELL_MAGIC.to_vec();
    opening.extend_from_slice(&secret);
    link.write_all(&opening).await.unwrap();
    let (shell, _) = runtime_collab::ShellClient::open(link);
    (serving, member, created, member_cert, shell, dispatch)
}

/// The person's shell records the receipt of an effect an action it ran had: the device keeps it
/// in its own slice of the member copy, as it keeps its own actions' receipts.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_receipt_the_shell_sends_is_kept_in_the_joined_workspace() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let (serving_state, member_state, dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (_serving, member, created, member_cert, shell, _dispatch) =
        shell_on_a_joined_workspace((serving_state.path(), member_state.path(), dir.path()), 13)
            .await;
    let ShellReply::Is(keeps) = shell
        .call(ShellCall::KeepsReceipts {
            workspace: created.workspace.clone(),
        })
        .await
        .unwrap()
    else {
        panic!("the daemon says whether it keeps receipts there");
    };
    assert!(keeps, "a member copy keeps this device's receipts");

    let invocation = uuid::Uuid::new_v4();
    let mut effect = tilo_actions::receipt::WorkspaceEffectReceipt::redacted(
        tilo_actions::InvocationId(invocation),
        &tilo_actions::MemberId::person(hex::encode(member_cert.user_public_key)).into(),
        tilo_actions::ActionContext::for_resource("layout", "desktop", "Layout"),
        tilo_actions::EffectReceiptStatus::Succeeded,
        tilo_clock::unix_now_i64(),
    );
    effect.completed_at = 100;
    let reply = shell
        .call(ShellCall::Receipt {
            workspace: created.workspace.clone(),
            redacted: serde_json::to_vec(&effect).unwrap(),
        })
        .await
        .unwrap();
    assert!(
        matches!(reply, ShellReply::Receipted(Ok(()))),
        "the daemon records the receipt: {reply:?}"
    );
    let state = member.workspaces().state(&created.workspace).await.unwrap();
    let kept = tilo_desktop_model::projection::workspace_effect_receipts_of(
        state.admitted_workspace().expect("the copy's governance"),
    );
    assert!(
        kept.iter().any(|kept| kept.invocation.0 == invocation),
        "the member copy keeps the shell's receipt"
    );
}

/// What an agent the person's shell admitted reads is decided at the record, for its certificate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_daemon_decides_a_read_for_the_certificate_the_shell_names() {
    use runtime_collab::wire::shell::{ShellCall, ShellReply};
    let (serving_state, member_state, dir) = (
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
        tempfile::tempdir().unwrap(),
    );
    let (_serving, _member, created, member_cert, shell, _dispatch) =
        shell_on_a_joined_workspace((serving_state.path(), member_state.path(), dir.path()), 41)
            .await;
    let authorizes = |certificate: &DeviceCert| ShellCall::Authorizes {
        workspace: created.workspace.clone(),
        app: "so.tilo.notes".to_owned(),
        resource: "n1".to_owned(),
        certificate: serde_json::to_string(certificate).unwrap(),
    };
    let reply = shell.call(authorizes(&member_cert)).await.unwrap();
    assert!(matches!(reply, ShellReply::Authorized(Ok(()))), "{reply:?}");
    let reply = shell.call(authorizes(&laptop(99).1)).await.unwrap();
    assert!(
        matches!(&reply, ShellReply::Authorized(Err(why)) if why.contains("certifies")),
        "a device the workspace does not certify reads nothing: {reply:?}"
    );
}

struct NoActions;

impl crate::shell::ShellActions for NoActions {
    fn dispatch(
        &self,
        _: &crate::shell::ShellCaller,
        _: Option<String>,
        _: Option<runtime_collab::wire::shell::ShellPress>,
    ) -> crate::shell::ShellDispatch {
        Box::pin(std::future::ready(Err("no actions here".to_owned())))
    }
}

struct NoStanding;

impl crate::services::PublishedResources for NoStanding {
    fn snapshot(
        &self,
        _: &crate::services::Caller,
        _: &str,
    ) -> tokio::sync::oneshot::Receiver<Result<crate::services::Reading, String>> {
        tokio::sync::oneshot::channel().1
    }
    fn watch(
        &self,
        _: &crate::services::Caller,
        _: &str,
        _: tokio::sync::mpsc::UnboundedSender<runtime_collab::wire::Event>,
    ) -> Result<crate::services::ResourceWatch, String> {
        Err("nothing stands here".to_owned())
    }
    fn unwatch(&self, _: crate::services::ResourceWatch) {}
    fn invoke(
        &self,
        _: &crate::services::Caller,
        _: &str,
        _: &str,
        _: Vec<u8>,
    ) -> tokio::sync::oneshot::Receiver<runtime_collab::wire::Receipt> {
        tokio::sync::oneshot::channel().1
    }
}

/// A computer whose person's workspace another device serves (their server, say) holds a member
/// copy of it: the agent a program of theirs connects as is listed in the workspace's record of
/// its agents all the same, written on that copy and taken by the serving device: the record that
/// admits the agent's session names it on both. Another person's agent is never listed there.
#[cfg(feature = "agents")]
#[tokio::test]
async fn a_member_copy_lists_its_persons_computer_agent() {
    let serving_state = tempfile::tempdir().unwrap();
    let member_state = tempfile::tempdir().unwrap();
    let (serving, device, created, invitation) = shared(serving_state.path(), "Theirs", 23).await;
    let (person, computer) = (device.1.user_public_key, device.1.device_public_key);
    let member = member_service(member_state.path(), device).await;
    let workspace = member
        .workspaces()
        .join_member(&invitation)
        .await
        .expect("the invitation is redeemed");
    assert_eq!(workspace.to_string(), created.workspace);
    assert!(
        member.workspaces().hosted_copy(workspace).await.is_none(),
        "the computer keeps the workspace as a member copy, hosted elsewhere"
    );

    let name = "mcp";
    let agent_id = tilo_actions::remote::device_agent_id(&person, name);
    let agent = tilo_actions::remote::Caller::delegated_agent(agent_id.clone(), person, [77; 32])
        .in_workspace(created.workspace.clone())
        .named(name);
    let as_person = tilo_actions::remote::Caller::authenticated(person, computer)
        .in_workspace(created.workspace.clone());
    crate::agents::workspace_agent(member.workspaces(), workspace, &agent, &as_person).await;
    assert_eq!(
        member
            .workspaces()
            .agent_name(&created.workspace, &agent_id)
            .await
            .as_deref(),
        Some(name),
        "the member copy lists the computer's agent"
    );
    let serving = serving.workspaces().clone();
    let (listed, agent_id) = (created.workspace.clone(), agent_id.clone());
    tilo_test_env::within("the serving device lists the agent", async move {
        while serving.agent_name(&listed, &agent_id).await.is_none() {
            tokio::time::sleep(std::time::Duration::from_millis(50)).await;
        }
    })
    .await;

    // Another person's agent is never listed through this person's copy.
    let stranger = tilo_actions::remote::device_agent_id(&[99; 32], name);
    let theirs =
        tilo_actions::remote::Caller::delegated_agent(stranger.clone(), [99; 32], [78; 32])
            .in_workspace(created.workspace.clone())
            .named(name);
    crate::agents::workspace_agent(member.workspaces(), workspace, &theirs, &as_person).await;
    assert!(
        member
            .workspaces()
            .agent_name(&created.workspace, &stranger)
            .await
            .is_none(),
        "another person's agent is not listed"
    );
}
