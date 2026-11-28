//! A resource's action is decided as whoever asks for it: the person, or the agent of theirs their
//! shell names as the one asking, bounded as that agent is, never as the person alone.

use serde_json::json;
use tilo_action_policy::agent_answers::{self, AnswerScope};
use tilo_action_policy::{Answer, Grant, Grantee, TreePath};
use tilo_action_types::{ActionKind, KindSet};
use tilo_actions::remote::{
    ActionCallError, Caller, InvokeArgument, InvokeRequest, ReceiptStatus, RemoteActionTarget,
};

use crate::workspaces::OwnWorkspaceOffer;
use crate::workspaces::fixtures::{answer_grant, hosting_device, record_grants, test_host};

/// The contact directory's type, as every caller names its record.
const CONTACTS: &str = "tilo.contacts.identities";

/// A contact added to the workspace's directory as `invoker` asks, where one does.
fn add_contact(label: &str, invoker: Option<tilo_actions::ActionInvoker>) -> InvokeRequest {
    let arguments = json!({"person": uuid::Uuid::new_v4().to_string(), "label": label});
    let address = tilo_actions::ActionAddress::new(CONTACTS, "project-contact");
    call(tilo_app_id::apps::CONTACTS, address, &arguments, invoker)
}

/// The call of `address` in `app`'s records with `arguments`, as `invoker` asks, where one does.
fn call(
    app: &str,
    address: tilo_actions::ActionAddress,
    arguments: &serde_json::Value,
    invoker: Option<tilo_actions::ActionInvoker>,
) -> InvokeRequest {
    InvokeRequest {
        asks: false,
        invocation: tilo_actions::InvocationId::new(),
        target: RemoteActionTarget {
            provider: app.to_owned(),
            address,
        },
        argument: InvokeArgument::Json(arguments.as_object().cloned().unwrap_or_default()),
        context: invoker
            .map(|invoker| tilo_actions::ActionContext::default().with_invoker(invoker)),
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// How `request`, asked by `caller` on this device, ended.
async fn status(
    workspaces: &crate::workspaces::Workspaces,
    caller: &Caller,
    request: InvokeRequest,
) -> ReceiptStatus {
    workspaces
        .providers()
        .invoke(caller, request)
        .expect("the device applies the directory's actions")
        .await
        .expect("the action answers")
        .expect("the action was taken")
        .status
}

fn agent(id: &str, person: &str) -> tilo_actions::ActionInvoker {
    tilo_actions::ActionInvoker::member(tilo_actions::MemberId::agent(id, person))
}

fn refused(status: &ReceiptStatus) -> bool {
    matches!(
        status,
        ReceiptStatus::Rejected {
            error: ActionCallError::NotPermitted
        }
    )
}

#[tokio::test(flavor = "multi_thread")]
async fn an_agent_the_shell_names_is_decided_as_itself_not_as_its_person() {
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    workspaces
        .offer_own_workspaces(OwnWorkspaceOffer::free("tilo.test"))
        .unwrap();
    let workspace = workspaces.create("Kitchen").await.unwrap().workspace;
    super::register(workspaces);
    let person = hex::encode(cert.user_public_key);
    let shell = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(workspace.clone());

    let own = status(workspaces, &shell, add_contact("Grace", None)).await;
    assert!(
        matches!(own, ReceiptStatus::Completed { .. }),
        "the person adds a contact: {own:?}"
    );

    // The workspace keeps Contacts from the agent: refused outright.
    record_grants(
        workspaces,
        &workspace,
        &[Grant::new(
            "scribe-contacts",
            Grantee::agent("scribe", person.clone()),
            TreePath::workspace(workspace.clone()).app(tilo_app_id::apps::CONTACTS),
            KindSet::ALL,
            Answer::Deny,
            Grantee::person(person.clone()),
        )],
    )
    .await;
    let denied = status(
        workspaces,
        &shell,
        add_contact("Ada", Some(agent("scribe", &person))),
    )
    .await;
    assert!(
        refused(&denied),
        "a rule on Contacts' node refuses the agent: {denied:?}"
    );

    // Its person narrowed another agent to Notes: it takes only what they answered.
    answer_grant(
        workspaces,
        &workspace,
        agent_answers::grant(
            &workspace,
            ("ranger", &person),
            (&AnswerScope::app("so.tilo.notes"), ActionKind::Outside),
            Answer::Allow,
            Grantee::person(person.clone()),
        ),
    )
    .await
    .expect("the person answers for their agent");
    let narrowed = status(
        workspaces,
        &shell,
        add_contact("Ada", Some(agent("ranger", &person))),
    )
    .await;
    assert!(
        refused(&narrowed),
        "an agent its person narrowed to Notes is refused Contacts: {narrowed:?}"
    );

    // Someone the shell names who is neither the person nor an agent of theirs asks nothing.
    let other = Some(tilo_actions::ActionInvoker::member(
        tilo_actions::MemberId::agent("scribe", "b0b"),
    ));
    let posing = status(workspaces, &shell, add_contact("Ada", other)).await;
    assert!(refused(&posing), "another person's agent: {posing:?}");

    let own = status(workspaces, &shell, add_contact("Lin", None)).await;
    assert!(
        matches!(own, ReceiptStatus::Completed { .. }),
        "the person's own change still goes: {own:?}"
    );
    server.shutdown().await;
}

/// A record is called by its app and the path its kind publishes, as every caller names one: the
/// person's devices are `so.tilo.devices` `tilo.devices#name-device`, which changes the record.
/// An action the record does not have is no such action, and a caller the person's workspace does
/// not admit is refused before anything of it is read.
#[tokio::test(flavor = "multi_thread")]
async fn a_record_is_called_by_its_app_and_the_path_it_publishes() {
    use crate::workspaces::person_workspace::{
        self,
        devices::{DEVICES, Devices},
    };
    let dir = tempfile::tempdir().unwrap();
    let (key, cert) = hosting_device();
    let server = test_host(dir.path(), &key, &cert).await;
    let workspaces = server.workspaces();
    super::register(workspaces);
    let own = person_workspace::found(workspaces).await.unwrap();
    person_workspace::apply_when_held(workspaces, DEVICES, Devices::keeping(&cert).unwrap())
        .await
        .unwrap();
    let device = hex::encode(cert.device_public_key);
    let named = json!({"device": device, "name": "Hallway server"});
    let call = |action: &str| {
        let address = tilo_actions::ActionAddress::new("tilo.devices", action);
        call(DEVICES.0, address, &named, None)
    };
    let person = Caller::authenticated(cert.user_public_key, cert.device_public_key)
        .in_workspace(own.clone());

    let named = status(workspaces, &person, call("name-device")).await;
    assert!(
        matches!(named, ReceiptStatus::Completed { .. }),
        "the person names their device by the record's app and path: {named:?}"
    );
    let record = person_workspace::read::<Devices>(workspaces, DEVICES)
        .await
        .unwrap();
    assert!(
        record
            .devices
            .iter()
            .any(|(_, kept)| kept.name.as_deref() == Some("Hallway server")),
        "the record calls the device what the person named it"
    );

    let unknown = status(workspaces, &person, call("explode")).await;
    assert!(
        matches!(
            unknown,
            ReceiptStatus::Rejected {
                error: ActionCallError::UnknownAction
            }
        ),
        "an action the record does not have is no such action: {unknown:?}"
    );

    let (_, stranger) = crate::workspaces::fixtures::guest_device();
    let outsider = Caller::authenticated(stranger.user_public_key, stranger.device_public_key)
        .in_workspace(own);
    let refused_outsider = status(workspaces, &outsider, call("name-device")).await;
    assert!(
        refused(&refused_outsider),
        "someone the workspace does not admit is refused: {refused_outsider:?}"
    );
    server.shutdown().await;
}
