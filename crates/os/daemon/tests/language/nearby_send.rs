//! Nearby Send between the person's devices, carried by the device's daemon: one device offers a
//! file, the other device's person answers it through their asked record, and the bytes reach the
//! workspace's Files whole.
//!
//! The one daemon in these tests is the receiving device. The sending device is another of the
//! person's, a real action endpoint serving what a sending daemon serves: it takes the offer. The
//! content is read by its address through the workspace's one content path, so the bytes need no
//! route of their own; the offer alone crosses the action link.

use std::sync::Arc;
use std::time::Duration;

use runtime_collab::ShellClient;
use serde_json::{Value, json};
use tilo_actions::remote::{
    ActionCallError, ActionProvider, Caller, CataloguePage, CatalogueQuery, InvocationReceipt,
    InvocationState, InvokeArgument, InvokeRequest, Pending, ReceiptStatus, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, ActionOutput, InvocationId};
use tilo_runtime::device::nearby;
use tilo_session_client::daemon_documents::{DaemonShell, whole};

use crate::language::files_support::{self, Session, varied};
use crate::language::host::{self, Host};

/// The file the sending device offers.
const NAME: &str = "note.txt";

/// The sending device, as another of the person's devices answers: it takes an offer and hears
/// how the transfer ended. Its content is read through the workspace's own content path, so it
/// serves no bytes over the action link.
struct Sender {
    transfer: String,
    hash: String,
}

impl Sender {
    fn new(transfer: String, hash: String) -> Arc<Self> {
        Arc::new(Self { transfer, hash })
    }
}

impl ActionProvider for Sender {
    fn serves(&self, provider: &str) -> bool {
        provider == nearby::PROVIDER
    }

    fn invocation(
        &self,
        _address: &ActionAddress,
        _argument: Option<&str>,
    ) -> Option<tilo_actions::api::ActionDescriptor> {
        None
    }

    fn query(
        &self,
        _caller: &Caller,
        _query: CatalogueQuery,
    ) -> Pending<Result<CataloguePage, ActionCallError>> {
        Pending::ready(Ok(CataloguePage::default()))
    }

    fn invoke(
        &self,
        _caller: &Caller,
        request: InvokeRequest,
    ) -> Pending<Result<InvocationReceipt, ActionCallError>> {
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ActionOutput::default(),
            },
        }))
    }

    fn observe(&self, _caller: &Caller, _id: &InvocationId) -> Option<InvocationState> {
        None
    }

    fn cancel(&self, _caller: &Caller, _id: &InvocationId) -> Pending<Result<(), ActionCallError>> {
        Pending::ready(Ok(()))
    }
}

/// A transfer id, written as the reading names it.
fn transfer_id() -> String {
    hex::encode([0x7a; 16])
}

/// The shell's connection to the daemon.
fn shell(host: &Host) -> DaemonShell {
    let link = host
        .daemon()
        .shell_link()
        .expect("the daemon hands its shell a link");
    // The shell's side of the link is the daemon's own socket, made for a thread of its own: the
    // runtime takes it only once it is nonblocking.
    link.set_nonblocking(true)
        .expect("the link is made nonblocking");
    let client = ShellClient::open(tokio::net::UnixStream::from_std(link).expect("a socket")).0;
    DaemonShell::new(Ok(client), tokio::runtime::Handle::current())
}

/// Nearby Send's reading as the person's shell reads it in `workspace`.
async fn reading(host: &Host, workspace: &str) -> Value {
    let read = whole(workspace, "session", nearby::RESOURCE);
    let answer = shell(host)
        .read(read)
        .await
        .expect("the daemon reads its Nearby Send resource");
    answer["records"][0]["value"].clone()
}

/// The transfer `handle` as the reading shows it, once one does.
async fn row(host: &Host, workspace: &str, handle: &str) -> Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let reading = reading(host, workspace).await;
        if let Some(row) = reading["transfers"]
            .as_array()
            .into_iter()
            .flatten()
            .find(|row| row["handle"] == handle)
        {
            return row.clone();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the reading never showed {handle}: {reading}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// The transfer `handle` once the content stopped coming, one way or another: the person's yes
/// starts it, and it ends when the file is kept or the transfer fails.
async fn settled(host: &Host, workspace: &str, handle: &str) -> Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let row = row(host, workspace, handle).await;
        if row["state"] != "sending" {
            return row;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the transfer never settled: {row}"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// `provider`'s `resource#action` with `arguments`.
fn request(provider: &str, resource: &str, action: &str, arguments: Value) -> InvokeRequest {
    let argument = match arguments {
        Value::Object(fields) => InvokeArgument::Json(fields),
        _ => InvokeArgument::None,
    };
    InvokeRequest {
        asks: false,
        invocation: InvocationId::new(),
        target: RemoteActionTarget {
            provider: provider.to_owned(),
            address: ActionAddress::new(resource, action),
        },
        argument,
        context: None,
        confirmation: None,
        revision: None,
        on_behalf_of: None,
        wait_ms: None,
        appearance: None,
        signed: None,
    }
}

/// Put `bytes` into `workspace`'s Files as a file of chunks, and answer its content address.
async fn put_chunks(host: &Host, workspace: &str, bytes: &[u8]) -> String {
    let key = host.key("Nearby");
    let session = Session::Key(&key);
    let content = files_support::put(&session, workspace, bytes).await;
    files_support::files(
        &session,
        workspace,
        "open",
        json!({"content": content, "name": NAME}),
    )
    .await;
    files_support::keeps(&session, host, workspace, ("", NAME), bytes).await
}

/// Offer the content `hash` from another device of the person's to this one, as the sending
/// daemon does.
async fn offered(
    sender: &Arc<Sender>,
    member: &host::Member,
) -> Result<tilo_actions::InvocationUpdate, ActionCallError> {
    member
        .call(
            nearby::PROVIDER,
            ActionAddress::new(nearby::RESOURCE, nearby::RECEIVE),
            &json!({
                "transfer": sender.transfer.clone(),
                "device": hex::encode(member.device()),
                "name": NAME,
                "hash": sender.hash.clone(),
                "label": "phone",
            }),
        )
        .await
}

/// The asked record the accept call waits in: its id in the workspace's invocations.
async fn waiting_record(host: &Host, invocation: InvocationId) -> String {
    let (dispatch, caller) = (host.dispatch(), host.caller());
    let deadline = tilo_test_env::hang_deadline();
    loop {
        if let Some(state) = dispatch.observe(&caller, &invocation)
            && let tilo_actions::InvocationUpdate::Progress { phase, message, .. } = &state.update
            && phase == "asked"
            && let Some(at) = message.as_deref()
            && let Some((_, record)) = at.split_once('/')
        {
            return record.to_owned();
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the accept never waited for the person's yes"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
}

/// Accept `transfer` as the person, answering their asked record with a yes at `tilo`'s own
/// prompt: the CLI on a connection the kernel says is theirs. A program making the same answer
/// on a plain connection is refused, and the transfer still waits for the person.
async fn keep(host: &Host, transfer: &str) {
    let (dispatch, caller) = (host.dispatch(), host.caller());
    let accept = request(
        nearby::PROVIDER,
        nearby::RESOURCE,
        nearby::ACCEPT,
        json!({"transfer": transfer}),
    );
    let invocation = accept.invocation;
    let answered = dispatch
        .invoke(&caller, accept)
        .await
        .expect("the accept is admitted")
        .expect("the accept is admitted");
    assert!(
        matches!(answered.status, ReceiptStatus::Accepted),
        "the accept waits for the person's yes: {answered:?}"
    );
    let record = waiting_record(host, invocation).await;
    let resolve = || {
        request(
            "tilo.invocations",
            &record,
            "resolve",
            json!({"resolution": "approve"}),
        )
    };
    let by_a_program = dispatch.invoke(&caller, resolve()).await;
    assert!(
        matches!(
            &by_a_program,
            Some(Err(ActionCallError::Refused { reason }))
                if reason == tilo_actions::remote::asked::CONFIRMED_BY_THE_SYSTEM
        ),
        "a program does not answer for the person: {by_a_program:?}"
    );
    let (own, person) = host
        .daemon()
        .own_shell_in(host.workspace())
        .expect("the person's shell reaches the workspace");
    let answered = own
        .invoke(&person, resolve())
        .await
        .expect("the person's yes is admitted")
        .expect("the person's yes is admitted");
    assert!(
        answered.status.is_terminal(),
        "the person's yes is answered: {answered:?}"
    );
}

/// A file another device offered is kept once the person says yes: its content is read whole by
/// the address offered through the workspace's one content path, and lands in their Files at the
/// folder they chose.
#[test]
fn a_file_offered_by_another_device_is_kept_through_the_persons_yes() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let bytes = varied(700_000, 8);
        let hash = put_chunks(&host, &workspace, &bytes).await;
        let sender = Sender::new(transfer_id(), hash);
        let member = host
            .member_serving("phone", &workspace, Arc::clone(&sender) as _)
            .await;
        offered(&sender, &member).await.expect("the offer is taken");

        let offer = row(&host, &workspace, &sender.transfer).await;
        assert_eq!(offer["state"], "offered", "{offer}");
        assert_eq!(offer["incoming"], true, "{offer}");

        keep(&host, &sender.transfer).await;

        let done = settled(&host, &workspace, &sender.transfer).await;
        assert_eq!(done["state"], "done", "{done}");
        let key = host.key("Reader");
        let session = Session::Key(&key);
        files_support::keeps(&session, &host, &workspace, ("Downloads", NAME), &bytes).await;
    });
}

/// A file another device offered is turned down without a byte moving.
#[test]
fn a_file_offered_by_another_device_is_turned_down() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let sender = Sender::new(transfer_id(), hex::encode([0x33; 32]));
        let member = host
            .member_serving("phone", &workspace, Arc::clone(&sender) as _)
            .await;
        offered(&sender, &member).await.expect("the offer is taken");

        let decline = request(
            nearby::PROVIDER,
            nearby::RESOURCE,
            nearby::DECLINE,
            json!({"transfer": sender.transfer}),
        );
        let _ = host
            .dispatch()
            .invoke(&host.caller(), decline)
            .await
            .expect("the decline is admitted");

        let row = row(&host, &workspace, &sender.transfer).await;
        assert_eq!(row["state"], "declined", "{row}");
        let tree = files_support::tree_if_kept(&host, &workspace).await;
        assert!(
            tree.and_then(|tree| files_support::content_at(&tree, "Downloads", NAME))
                .is_none(),
            "nothing lands in the person's Files"
        );
    });
}

/// An offer from a device that asked as somebody else's is refused.
#[test]
fn an_offer_naming_another_device_than_the_one_that_asked_is_refused() {
    host::run(|host| async move {
        let workspace = host.workspace().to_owned();
        let sender = Sender::new(transfer_id(), hex::encode([0x22; 32]));
        let member = host
            .member_serving("phone", &workspace, Arc::clone(&sender) as _)
            .await;
        let answered = member
            .call(
                nearby::PROVIDER,
                ActionAddress::new(nearby::RESOURCE, nearby::RECEIVE),
                &json!({
                    "transfer": sender.transfer.clone(),
                    "device": hex::encode([0x11; 32]),
                    "name": NAME,
                    "hash": sender.hash.clone(),
                }),
            )
            .await;
        assert!(
            matches!(answered, Err(ActionCallError::Refused { .. })),
            "{answered:?}"
        );
    });
}
