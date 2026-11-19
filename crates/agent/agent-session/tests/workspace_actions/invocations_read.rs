//! A workspace's runs, as the host lists them to a member who may not read
//! every record: of each run, only what lies in records they may read now.
//!
//! The workspace's policy is the fixture's to set (the link the host asks,
//! as `member_roles` sets it): an editor who may read every record lists the
//! runs that changed them whole; once their Read on one record is gone, the
//! next listing says nothing of the runs there, not their record, their
//! words nor their count, while the host's own person still lists them. A
//! call of theirs waiting for the person on a record they can no longer read
//! is not listed to them, nor counted as waiting. And where the record's
//! policy admits them but a grant of their own takes Read there, the listing
//! answers as the read would: nothing of it.
use super::*;
use std::collections::HashSet;
use std::sync::Mutex;
use tilo_actions::remote::{
    Caller, Completer, ConfirmationQuestion, ConfirmationReply, Pending, pending,
    workspace_invocations,
};
use tilo_agent_session::workspace_actions::{HostDevice, ViewReadPolicy, WorkspaceDocumentsLink};
use tilo_server_protocol::WorkspaceId;
use ui::permissions::Role;
use ui::permissions::policy::{ActionKind, Answer, Grant, Grantee, KindSet, TreePath};

const NOTES: &str = "so.tilo.notes";
const TASKS: &str = "so.tilo.tasks";

/// The workspace's word on one editor: which apps' records they may not
/// read now, and the questions put to them, which nobody answers.
struct Reading {
    member: HostDevice,
    workspaces: tilo_server::workspaces::Workspaces,
    person: [u8; 32],
    device: [u8; 32],
    unreadable: Mutex<HashSet<String>>,
    /// Apps whose records the policy admits them to, while a grant of their
    /// own denies them Read there.
    bound: Mutex<HashSet<String>>,
    asked: Mutex<Vec<Completer<Result<ConfirmationReply, ActionCallError>>>>,
}

impl Reading {
    fn policy(&self, caller: &Caller) -> Option<ViewReadPolicy> {
        if caller.user_public_key() != Some(&self.person)
            || caller.device_public_key() != Some(&self.device)
            || !caller.acting().is_person()
        {
            return None;
        }
        let person = hex::encode(self.person);
        let access = ui::permissions::PermissionContext::member_of(
            WORKSPACE,
            &ui::permissions::MemberId::person(person.clone()),
            Role::Editor,
        )
        .access()
        .clone();
        let denied: Vec<Grant> = self
            .bound
            .lock()
            .unwrap()
            .iter()
            .map(|app| {
                let grantee = Grantee::person(person.clone());
                Grant::new(
                    format!("no-read:{app}"),
                    grantee.clone(),
                    access.node(&TreePath::root().app(app)),
                    KindSet::of(ActionKind::Read),
                    Answer::Deny,
                    grantee,
                )
            })
            .collect();
        Some(ViewReadPolicy {
            workspace: WORKSPACE.into(),
            access: access.granted(denied),
            sandbox: Vec::new(),
            unix_time: tilo_clock::unix_now_ns() / 1_000_000_000,
            agent_bound: Vec::new(),
        })
    }
}

impl WorkspaceDocumentsLink for Reading {
    fn user_public_key(&self) -> [u8; 32] {
        self.member.endpoint.server_cert.user_public_key
    }
    fn own_device(&self) -> HostDevice {
        self.member.clone()
    }
    fn admits(&self, _workspace: WorkspaceId, caller: &Caller) -> bool {
        self.policy(caller).is_some()
    }
    fn view_read_policy(
        &self,
        _workspace: WorkspaceId,
        caller: &Caller,
        app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        if self.unreadable.lock().unwrap().contains(app) {
            return None;
        }
        self.policy(caller)
    }
    fn workspace_policy(&self, _workspace: WorkspaceId, caller: &Caller) -> Option<ViewReadPolicy> {
        self.policy(caller)
    }
    fn host_keys(
        &self,
        route: &DocumentRoute,
        create: bool,
    ) -> Option<Result<key_group::ContentKeys, String>> {
        self.member
            .runtime
            .block_on(self.workspaces.own_route_keys(route, create))
    }
    fn ask_person(
        &self,
        _workspace: WorkspaceId,
        _caller: &Caller,
        _question: ConfirmationQuestion,
    ) -> Option<Pending<Result<ConfirmationReply, ActionCallError>>> {
        let (asked, answer) = pending();
        self.asked.lock().unwrap().push(asked);
        Some(answer)
    }
}

fn json(value: serde_json::Value) -> InvokeArgument {
    let serde_json::Value::Object(map) = value else {
        panic!("arguments are an object")
    };
    InvokeArgument::Json(map)
}

/// The receipt `caller`'s `request` is answered with at once, asked again
/// while the host still settles the workspace.
fn receipt(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    request: &InvokeRequest,
) -> ReceiptStatus {
    receipt_by_id(actions, workspace, caller, request).1
}

/// The same, with the id the answered attempt went by.
fn receipt_by_id(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    request: &InvokeRequest,
) -> (InvocationId, ReceiptStatus) {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = InvocationId::new();
        let invocation = attempt.invocation;
        let answered = actions
            .serve(workspace, caller, RemoteActionMessage::Invoke(attempt))
            .unwrap()
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
            .unwrap();
        let RemoteActionMessage::Receipt(receipt) = answered else {
            panic!("expected a receipt, got {answered:?}")
        };
        let again = matches!(
            &receipt.status,
            ReceiptStatus::Rejected {
                error: ActionCallError::Unavailable { .. }
            }
        ) || matches!(&receipt.status, ReceiptStatus::Failed { error } if error.retryable);
        if again && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(50));
            continue;
        }
        return (invocation, receipt.status);
    }
}

/// How `invocation` stands, as `caller` is answered when they ask; `None` once it is
/// no longer kept.
fn observed(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    invocation: InvocationId,
) -> Option<tilo_actions::InvocationUpdate> {
    let answered = actions
        .serve(
            workspace,
            caller,
            RemoteActionMessage::Observe {
                invocation,
                wait_ms: None,
                seen: None,
            },
        )
        .unwrap()
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .unwrap();
    let RemoteActionMessage::Update { state, .. } = answered else {
        panic!("expected how it stands, got {answered:?}")
    };
    state.map(|state| state.update)
}

/// Whether `update` says that its call's person is asked to allow it on their devices, and
/// that nothing of it happened yet: under way by the protocol's own word for a confirmation
/// that is required, so whoever follows the call can tell it from one that is merely slow.
fn says_asked(update: Option<&tilo_actions::InvocationUpdate>) -> bool {
    matches!(
        update,
        Some(tilo_actions::InvocationUpdate::Progress { phase, message, .. })
            if phase == tilo_actions::remote::CONFIRMATION_REQUIRED_CODE
                && message.as_deref().is_some_and(|said| {
                    said.contains("on their devices") && said.contains("Nothing happened yet")
                })
    )
}

/// Every run of the workspace `caller` is listed.
fn invocations(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
) -> serde_json::Value {
    invocations_of(actions, workspace, caller, None)
}

/// The runs of the member `by`, or of everyone when it is `None`.
fn invocations_of(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    by: Option<&str>,
) -> serde_json::Value {
    let listing = workspace_invocations::target();
    let mut query = serde_json::json!({"limit": workspace_invocations::MOST});
    if let Some(by) = by {
        query["by"] = by.into();
    }
    let status = receipt(
        actions,
        workspace,
        caller,
        &request(&listing.provider, listing.address, json(query)),
    );
    let ReceiptStatus::Completed { output } = status else {
        panic!("the runs are listed: {status:?}")
    };
    output.value.unwrap()
}

/// The runs of `listed` that changed a record of `app`, or whose call asked
/// one of it.
fn in_app<'a>(listed: &'a serde_json::Value, app: &str) -> Vec<&'a serde_json::Value> {
    let asked = format!("{}#", app.rsplit('.').next().unwrap());
    listed["items"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|run| {
            run["records"]
                .as_array()
                .into_iter()
                .flatten()
                .any(|record| record["app_id"] == app)
                || run["action"]
                    .as_str()
                    .is_some_and(|action| action.starts_with(&asked))
        })
        .collect()
}

/// Whether `listed` says anything of `app`'s record or what was written there.
fn names(listed: &serde_json::Value, app: &str, words: &str) -> bool {
    let text = listed.to_string();
    let path = app.rsplit('.').next().unwrap();
    text.contains(app)
        || text.contains(&format!("\"{path}#"))
        || text.contains(&format!("\"app\":\"{path}\""))
        || text.contains(words)
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_member_lists_only_the_invocations_in_records_they_may_read_now() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let notes = browser.create(NOTES);
    let tasks = browser.create(TASKS);
    let books = browser.create(EBOOK);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let (notes_provider, tasks_provider, books_provider) = (
        record_provider(&notes),
        record_provider(&tasks),
        record_provider(&books),
    );

    let reading = Arc::new(Reading {
        member: HostDevice {
            endpoint: tilo_agent_session::HostEndpoint::iroh(
                hosted.server.node_addr(),
                host.cert.clone(),
            ),
            key: host.key.clone(),
            runtime: tokio::runtime::Handle::current(),
        },
        workspaces: hosted.server.workspaces().unwrap().clone(),
        person: guest_device.cert.user_public_key,
        device: guest_device.cert.device_public_key,
        unreadable: Mutex::default(),
        bound: Mutex::default(),
        asked: Mutex::default(),
    });
    let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
    actions.attach(reading.clone()).unwrap();
    let workspace = browser.workspace;
    let member = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let owner = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser_device.cert.device_public_key,
    );

    // The editor files a task and writes a note, each a run of its own.
    let filed = receipt(
        &actions,
        workspace,
        &member,
        &request(
            &tasks_provider,
            ActionAddress::new(
                ui::resource::chain_resource_id(&ui::resource::DocumentId::derived(
                    "issues",
                    &ui::resource::DocumentId::workspace_default(),
                )),
                "create",
            ),
            json(serde_json::json!({"title": "The secret plan"})),
        ),
    );
    assert!(
        matches!(
            filed,
            ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted
        ),
        "{filed:?}"
    );
    let noted = receipt(
        &actions,
        workspace,
        &member,
        &request(
            &notes_provider,
            ActionAddress::new(notes::note_list::chain_resource(), "new-note"),
            json(serde_json::json!({"title": "A note for all"})),
        ),
    );
    assert!(
        matches!(
            noted,
            ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted
        ),
        "{noted:?}"
    );

    // Reading every record, they list both runs, whole.
    let deadline = tilo_test_env::hang_deadline();
    let whole = loop {
        let listed = invocations(&actions, workspace, &member);
        let text = listed.to_string();
        if [TASKS, NOTES]
            .iter()
            .all(|app| text.contains(&format!("\"app_id\":\"{app}\"")))
        {
            break listed;
        }
        assert!(
            Instant::now() < deadline,
            "both runs are listed to a reader of both: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    };
    let in_tasks = in_app(&whole, TASKS).len() as u64;

    // Asked for one member's runs, the listing answers theirs alone: here
    // the editor's own, and nobody's under another id.
    let editor = member.acting().person_id().unwrap_or_default().to_owned();
    let own = invocations_of(&actions, workspace, &member, Some(&editor));
    assert_eq!(
        own["total"].as_u64().unwrap(),
        whole["total"].as_u64().unwrap(),
        "every run here is the editor's: {own}"
    );
    let nobody = invocations_of(&actions, workspace, &member, Some("nobody"));
    assert_eq!(nobody["total"].as_u64().unwrap(), 0, "{nobody}");
    assert!(
        nobody["items"].as_array().unwrap().is_empty(),
        "another member's runs are none of theirs: {nobody}"
    );

    // Their Read on the tasks is gone: the next listing says nothing of the
    // runs there, and counts only what is left.
    reading.unreadable.lock().unwrap().insert(TASKS.to_owned());
    let without = invocations(&actions, workspace, &member);
    assert!(
        !names(&without, TASKS, "secret plan"),
        "nothing of the tasks is said: {without}"
    );
    assert_eq!(
        in_app(&without, NOTES).len(),
        in_app(&whole, NOTES).len(),
        "the note's run is still theirs to read: {without}"
    );
    assert_eq!(
        without["total"].as_u64().unwrap(),
        whole["total"].as_u64().unwrap().saturating_sub(in_tasks),
        "the total counts only what they may read: {whole} {without}"
    );
    assert_eq!(
        without["items"].as_array().unwrap().len() as u64,
        without["total"].as_u64().unwrap()
    );
    // The host's own person lists them whole.
    let by_owner = invocations(&actions, workspace, &owner);
    assert_eq!(
        in_app(&by_owner, TASKS).len() as u64,
        in_tasks,
        "{by_owner}"
    );

    // A removal of theirs waits for the person; once they may no longer
    // read the books, it is neither listed to them nor counted as waiting.
    let removal_asked = request(
        &books_provider,
        ActionAddress::new("", "remove-from-library"),
        json(serde_json::json!({"book": "book:dusk"})),
    );
    let (removal_id, removal) = receipt_by_id(&actions, workspace, &member, &removal_asked);
    assert_eq!(removal, ReceiptStatus::Accepted, "it waits for the person");
    // Whoever follows the call is told that its person is asked, and need not wait for them.
    let waiting = observed(&actions, workspace, &member, removal_id);
    assert!(
        says_asked(waiting.as_ref()),
        "its person is asked, and how it stands says so: {waiting:?}"
    );
    let asking = invocations(&actions, workspace, &member);
    assert_eq!(asking["waiting"], 1, "{asking}");
    reading.unreadable.lock().unwrap().insert(EBOOK.to_owned());
    let hidden = invocations(&actions, workspace, &member);
    assert_eq!(hidden["waiting"], 0, "{hidden}");
    assert!(
        !names(&hidden, EBOOK, "remove-from-library"),
        "nothing of the books is said: {hidden}"
    );
    assert_eq!(
        hidden["total"].as_u64().unwrap(),
        asking["total"].as_u64().unwrap().saturating_sub(1),
        "{asking} {hidden}"
    );
    assert_eq!(invocations(&actions, workspace, &owner)["waiting"], 1);
    // The person refuses: the call ends, and says that.
    let question = reading.asked.lock().unwrap().pop().unwrap();
    question.complete(Ok(ConfirmationReply::Deny));
    let refused = observed(&actions, workspace, &member, removal_id);
    assert!(
        matches!(
            &refused,
            Some(tilo_actions::InvocationUpdate::Failed { error, .. })
                if error.message.contains("did not allow")
        ),
        "{refused:?}"
    );

    // The record's policy admits them to the notes, while a grant of their
    // own takes Read there: what a read decides, the listing decides too.
    let before = invocations(&actions, workspace, &member);
    let in_notes = in_app(&before, NOTES).len() as u64;
    assert!(in_notes > 0, "{before}");
    reading.bound.lock().unwrap().insert(NOTES.to_owned());
    let bound = invocations(&actions, workspace, &member);
    assert!(in_app(&bound, NOTES).is_empty(), "{bound}");
    assert_eq!(
        bound["total"].as_u64().unwrap(),
        before["total"].as_u64().unwrap().saturating_sub(in_notes),
        "{before} {bound}"
    );
    assert_eq!(
        in_app(&invocations(&actions, workspace, &owner), NOTES).len() as u64,
        in_notes,
        "the host's own person still lists them"
    );

    // Asked once more, and allowed: from their answer on it is no longer said to wait
    // for them.
    reading.unreadable.lock().unwrap().remove(EBOOK);
    let (again_id, again) = receipt_by_id(&actions, workspace, &member, &removal_asked);
    assert_eq!(again, ReceiptStatus::Accepted, "it waits for the person");
    let waiting = observed(&actions, workspace, &member, again_id);
    assert!(says_asked(waiting.as_ref()), "{waiting:?}");
    let question = reading.asked.lock().unwrap().pop().unwrap();
    question.complete(Ok(ConfirmationReply::AllowOnce));
    let allowed = observed(&actions, workspace, &member, again_id);
    assert!(!says_asked(allowed.as_ref()), "{allowed:?}");
}

/// The runs of `listed` in `app`'s records, once `caller`'s listing holds a
/// record of it: what the host lists once it read the record's history.
async fn listed_with_document(
    actions: &tilo_agent_session::WorkspaceActions,
    workspace: WorkspaceId,
    caller: &Caller,
    app: &str,
) -> serde_json::Value {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let listed = invocations(actions, workspace, caller);
        if listed
            .to_string()
            .contains(&format!("\"app_id\":\"{app}\""))
        {
            return listed;
        }
        assert!(
            Instant::now() < deadline,
            "the record's run is listed: {listed}"
        );
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_own_change_is_one_invocation_also_after_a_restart() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::member());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = crate::support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let mut browser = Browser::open(&hosted, &browser_device, &host).await;
    let notes = browser.create(NOTES);
    browser.peer.barrier().unwrap().recv().unwrap().unwrap();
    let notes_provider = record_provider(&notes);
    let reading = Arc::new(Reading {
        member: HostDevice {
            endpoint: tilo_agent_session::HostEndpoint::iroh(
                hosted.server.node_addr(),
                host.cert.clone(),
            ),
            key: host.key.clone(),
            runtime: tokio::runtime::Handle::current(),
        },
        workspaces: hosted.server.workspaces().unwrap().clone(),
        person: guest_device.cert.user_public_key,
        device: guest_device.cert.device_public_key,
        unreadable: Mutex::default(),
        bound: Mutex::default(),
        asked: Mutex::default(),
    });
    let serving = || {
        let actions = tilo_agent_session::WorkspaceActions::new(Arc::new(crate::common::catalogue));
        actions.attach(reading.clone()).unwrap();
        actions
    };
    let actions = serving();
    let workspace = browser.workspace;
    // A person acting as themselves: no key, no agent, no step of their own.
    let person = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let owner = Caller::authenticated(
        root.verifying_key().to_bytes(),
        browser_device.cert.device_public_key,
    );

    let noted = receipt(
        &actions,
        workspace,
        &person,
        &request(
            &notes_provider,
            ActionAddress::new(notes::note_list::chain_resource(), "new-note"),
            json(serde_json::json!({"title": "Grace's own note"})),
        ),
    );
    assert!(
        matches!(
            noted,
            ReceiptStatus::Completed { .. } | ReceiptStatus::Accepted
        ),
        "{noted:?}"
    );

    // Every reader of the notes lists the one act as one run, its call and
    // its change together.
    for reader in [&person, &owner] {
        let listed = listed_with_document(&actions, workspace, reader, NOTES).await;
        assert_eq!(listed["total"], 1, "one act is one run: {listed}");
        let run = &listed["items"][0];
        assert_eq!(run["by"]["kind"], "person", "{run}");
        assert_eq!(
            (&run["recorded"], &run["remembered"]),
            (&serde_json::json!(true), &serde_json::json!(true)),
            "{run}"
        );
        assert!(run["id"].is_string() && run["step"].is_string(), "{run}");
    }

    // After a restart the host remembers no call: the record alone lists it,
    // still once.
    actions.shutdown();
    drop(actions);
    let restarted = serving();
    for reader in [&person, &owner] {
        let listed = listed_with_document(&restarted, workspace, reader, NOTES).await;
        assert_eq!(listed["total"], 1, "{listed}");
        let run = &listed["items"][0];
        assert_eq!(run["remembered"], false, "{run}");
        assert!(run.get("id").is_none(), "{run}");
    }
}
