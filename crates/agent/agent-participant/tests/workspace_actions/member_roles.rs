//! What the host runs for a member who is not its own person, and for that
//! member's agent, decided by the member's standing now: a viewer changes
//! nothing, a commenter comments and nothing else, an editor edits but is
//! asked first where the action asks, and a member removed or a device
//! revoked is refused whatever it still holds. The workspace's policy is
//! the fixture's to set; everything else is the host's own service.
use super::*;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tilo_actions::remote::{
    Caller, ConfirmationQuestion, ConfirmationReply, ContentPutAnswer, ContentPutStep, Pending,
};
use tilo_agent_participant::workspace_actions::{
    ViewReadPolicy, WorkspaceMember, WorkspaceRecordsLink,
};
use ui::permissions::Role;

const NOTES: &str = "so.tilo.notes";
const TASKS: &str = "so.tilo.tasks";

/// The workspace's word on one member: their role, whether their device is
/// revoked, and what they answer when the host asks them.
struct Membership {
    member: WorkspaceMember,
    person: [u8; 32],
    device: [u8; 32],
    role: Mutex<Option<Role>>,
    revoked: AtomicBool,
    answer: Mutex<Option<ConfirmationReply>>,
    asked: AtomicUsize,
    content: tilo_content::ContentStore,
}

impl Membership {
    fn standing(&self, caller: &Caller) -> Option<ui::permissions::Standing> {
        let role = (*self.role.lock().unwrap())?;
        (!self.revoked.load(Ordering::SeqCst)
            && caller.user_public_key() == Some(&self.person)
            && caller.device_public_key() == Some(&self.device)
            && matches!(
                caller.principal(),
                tilo_actions::remote::CallerPrincipal::Person(_)
            ))
        .then(|| {
            ui::permissions::PermissionContext::member_of(WORKSPACE, hex::encode(self.person), role)
                .standing()
                .clone()
        })
    }
}

impl WorkspaceRecordsLink for Membership {
    fn user_public_key(&self) -> [u8; 32] {
        self.member.endpoint.host_cert.user_public_key
    }
    fn member(&self) -> WorkspaceMember {
        self.member.clone()
    }
    fn admits(&self, _workspace: u64, caller: &Caller) -> bool {
        self.standing(caller).is_some()
    }
    fn view_read_policy(
        &self,
        _workspace: u64,
        caller: &Caller,
        _app: &str,
        _record: &str,
    ) -> Option<ViewReadPolicy> {
        Some(ViewReadPolicy {
            workspace: WORKSPACE.into(),
            standing: self.standing(caller)?,
            sandbox: Vec::new(),
            unix_time: tilo_clock::unix_now_ns() / 1_000_000_000,
        })
    }
    fn workspace_policy(&self, workspace: u64, caller: &Caller) -> Option<ViewReadPolicy> {
        self.view_read_policy(workspace, caller, "", "")
    }
    fn workspace_content(&self, _workspace: u64) -> Option<tilo_content::ContentStore> {
        Some(self.content.clone())
    }
    fn ask_person(
        &self,
        _workspace: u64,
        caller: &Caller,
        _question: ConfirmationQuestion,
    ) -> Option<Pending<Result<ConfirmationReply, ActionHostError>>> {
        assert_eq!(
            caller.user_public_key(),
            Some(&self.person),
            "the member is asked"
        );
        self.asked.fetch_add(1, Ordering::SeqCst);
        let answer = (*self.answer.lock().unwrap())?;
        Some(Pending::ready(Ok(answer)))
    }
}

/// What the service answers `caller` for `message`.
fn served(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    message: RemoteActionMessage,
) -> RemoteActionMessage {
    actions
        .serve(workspace, caller, message)
        .unwrap()
        .wait_timeout(tilo_test_env::hang_bound())
        .ok()
        .flatten()
        .unwrap()
}

/// The status `caller`'s `request` is answered with.
fn invoked(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    request: &InvokeRequest,
) -> ReceiptStatus {
    // A host still settling the workspace answers that it is unavailable, or
    // that a save is to be tried again: asked again under a new id.
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let mut attempt = request.clone();
        attempt.invocation = InvocationId::new();
        let status = match served(
            actions,
            workspace,
            caller,
            RemoteActionMessage::Invoke(attempt),
        ) {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("expected a receipt, got {other:?}"),
        };
        let again = matches!(
            &status,
            ReceiptStatus::Rejected {
                error: ActionHostError::Unavailable { .. }
            }
        ) || matches!(&status, ReceiptStatus::Failed { error } if error.retryable);
        if !again || Instant::now() >= deadline {
            return status;
        }
        std::thread::sleep(Duration::from_millis(50));
    }
}

/// `bytes` put into the workspace's store by `caller`: its reference.
#[expect(
    clippy::panic_in_result_fn,
    reason = "a reply of another kind is the failure the test reports"
)]
fn put(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    bytes: &[u8],
) -> Result<String, ActionHostError> {
    let step = |step| match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::ContentPutRequest {
            correlation: 1,
            step,
        },
    ) {
        RemoteActionMessage::ContentPutResponse { result, .. } => result,
        other => panic!("expected an upload's answer, got {other:?}"),
    };
    let ContentPutAnswer::Begun { upload } = step(ContentPutStep::Begin {
        size: bytes.len() as u64,
    })?
    else {
        panic!("an upload begins")
    };
    step(ContentPutStep::append(upload, 0, bytes))?;
    match step(ContentPutStep::Finish { upload })? {
        ContentPutAnswer::Finished { content } => Ok(content),
        other => panic!("expected the content, got {other:?}"),
    }
}

/// The bytes of `content` `caller` gets from the workspace's store.
#[expect(
    clippy::panic_in_result_fn,
    reason = "a reply of another kind is the failure the test reports"
)]
fn got(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    content: &str,
) -> Result<Vec<u8>, ActionHostError> {
    match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::ContentGetRequest {
            correlation: 1,
            content: content.to_owned(),
            offset: 0,
            length: 0,
        },
    ) {
        RemoteActionMessage::ContentGetResponse { result, .. } => {
            Ok(result?.bytes().expect("well-formed bytes"))
        }
        other => panic!("expected a piece, got {other:?}"),
    }
}

/// The actions `caller` is listed on the record `provider` names.
fn listed(
    actions: &tilo_agent_participant::WorkspaceActions,
    workspace: u64,
    caller: &Caller,
    provider: &str,
) -> Vec<tilo_actions::ActionDescriptor> {
    let query = CatalogueQuery {
        provider: Some(provider.to_owned()),
        max_results: 500,
        ..CatalogueQuery::default()
    };
    match served(
        actions,
        workspace,
        caller,
        RemoteActionMessage::CatalogueRequest {
            correlation: 1,
            query,
        },
    ) {
        RemoteActionMessage::CatalogueResponse { result, .. } => result
            .unwrap()
            .rows
            .into_iter()
            .filter_map(|row| row.action)
            .collect(),
        other => panic!("expected a catalogue, got {other:?}"),
    }
}

fn json(value: serde_json::Value) -> InvokeArgument {
    let serde_json::Value::Object(map) = value else {
        panic!("arguments are an object")
    };
    InvokeArgument::Json(map)
}

/// The titles of the notes the record at `notes` holds, as `browser` has
/// received it.
fn note_titles(browser: &mut Browser, notes: &DocumentRoute) -> Vec<String> {
    browser
        .mount(notes)
        .read(|doc| {
            <notes::Notebook as ui::tilo_resource::Resource>::read(doc)
                .map(|notebook| {
                    notebook
                        .pages
                        .iter()
                        .map(|(_, page)| page.title.clone())
                        .collect()
                })
                .unwrap_or_default()
        })
        .unwrap_or_default()
}

/// Wait until the host's own person's note `marker` reached `browser`, then
/// the titles it holds: everything the host committed before it has too.
async fn titles_after(browser: &mut Browser, notes: &DocumentRoute, marker: &str) -> Vec<String> {
    let provider = record_provider(notes);
    let made = browser
        .invoke(request(
            &provider,
            ActionAddress::new("notes.notebook", "new-note"),
            json(serde_json::json!({"title": marker})),
        ))
        .await;
    assert!(
        matches!(made.status, ReceiptStatus::Completed { .. }),
        "{made:?}"
    );
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let titles = note_titles(browser, notes);
        if titles.iter().any(|title| title == marker) {
            return titles;
        }
        assert!(
            Instant::now() < deadline,
            "the marker never arrived: {titles:?}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn each_member_runs_what_their_standing_lets_them_and_nothing_else() {
    let (root, _) = tilo_user_identity::UserIdentity::generate("Ada", 1);
    let (guest, _) = tilo_user_identity::UserIdentity::generate("Grace", 1);
    let host = enrol(&root, "home-server", DeviceCapabilities::host());
    let browser_device = enrol(&root, "browser", DeviceCapabilities::member());
    let guest_device = enrol(&guest, "guest-browser", DeviceCapabilities::member());
    let state = support::scratch_dir();
    let hosted = start(&root, &host, state.path()).await;
    let _participant = participant::Participant::start(&hosted.relay, &root).await;
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
    // The host's own person files an issue for members to comment on.
    let filed = browser
        .invoke(request(
            &tasks_provider,
            ActionAddress::new("", "create"),
            json(serde_json::json!({"title": "Ship the launch"})),
        ))
        .await;
    assert!(
        matches!(filed.status, ReceiptStatus::Completed { .. }),
        "{filed:?}"
    );

    let membership = Arc::new(Membership {
        member: WorkspaceMember {
            endpoint: tilo_agent_participant::HostEndpoint::iroh(
                hosted.relay.node_addr(),
                host.cert.clone(),
            ),
            key: host.key.clone(),
            runtime: tokio::runtime::Handle::current(),
        },
        person: guest_device.cert.user_public_key,
        device: guest_device.cert.device_public_key,
        role: Mutex::new(Some(Role::Viewer)),
        revoked: AtomicBool::new(false),
        answer: Mutex::new(None),
        asked: AtomicUsize::new(0),
        content: tilo_content::ContentStore::in_dir(state.path().join("member-content")),
    });
    let actions = tilo_agent_participant::WorkspaceActions::new(Arc::new(common::catalogue));
    actions.attach(membership.clone()).unwrap();
    let workspace = browser.workspace;
    let member = Caller::authenticated(
        guest_device.cert.user_public_key,
        guest_device.cert.device_public_key,
    )
    .in_workspace(WORKSPACE);
    let new_note = |title: &str| {
        request(
            &notes_provider,
            ActionAddress::new("notes.notebook", "new-note"),
            json(serde_json::json!({"title": title})),
        )
    };

    // A viewer: the edit the record publishes is refused as not theirs,
    // never as unknown, and nothing of it is saved.
    let viewer = invoked(&actions, workspace, &member, &new_note("By the viewer"));
    assert_eq!(
        viewer,
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );
    assert!(
        !listed(&actions, workspace, &member, &notes_provider)
            .iter()
            .any(|action| action.address.action == "new-note"),
        "a viewer is not listed what they cannot run"
    );

    // A commenter: commenting is theirs, an edit of the body is not.
    *membership.role.lock().unwrap() = Some(Role::Commenter);
    let comment = listed(&actions, workspace, &member, &tasks_provider)
        .into_iter()
        .find(|action| {
            action.address.action == "comment"
                && !action.params.iter().any(|param| param.name == "issue")
        })
        .expect("a commenter is listed Comment on the issue");
    let commented = invoked(
        &actions,
        workspace,
        &member,
        &request(
            &tasks_provider,
            comment.address.clone(),
            json(serde_json::json!({"text": "Looks good"})),
        ),
    );
    assert!(
        matches!(commented, ReceiptStatus::Completed { .. }),
        "a commenter comments: {commented:?}"
    );
    assert_eq!(
        invoked(&actions, workspace, &member, &new_note("By the commenter")),
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );

    // An editor: the body edit is saved, by them.
    *membership.role.lock().unwrap() = Some(Role::Editor);
    let edited = invoked(&actions, workspace, &member, &new_note("By the editor"));
    assert!(
        matches!(edited, ReceiptStatus::Completed { .. }),
        "{edited:?}"
    );
    // An editor opens a new record in the workspace, and their agent does
    // too: each is listed the open, and each record is begun by them.
    let agent = member.clone().delegated_to("Claude Code");
    let workspace_provider = format!("workspace:{WORKSPACE}");
    let open = || {
        request(
            &workspace_provider,
            ActionAddress::new(format!("app.{NOTES}"), "open"),
            InvokeArgument::None,
        )
    };
    for (caller, who) in [(&member, "the editor"), (&agent, "their agent")] {
        assert!(
            listed(&actions, workspace, caller, &workspace_provider)
                .iter()
                .any(|action| action.address.action == "open"),
            "{who} is listed the open"
        );
        let opened = invoked(&actions, workspace, caller, &open());
        assert!(
            matches!(opened, ReceiptStatus::Completed { .. }),
            "{who} opens a record: {opened:?}"
        );
    }
    // An editor and their agent put files into the workspace and get them
    // back; a viewer only gets them, and opens nothing.
    let by_editor = put(&actions, workspace, &member, b"%PDF-1.7 editor").unwrap();
    let by_agent = put(&actions, workspace, &agent, b"%PDF-1.7 agent").unwrap();
    assert_eq!(
        got(&actions, workspace, &agent, &by_editor).unwrap(),
        b"%PDF-1.7 editor"
    );
    *membership.role.lock().unwrap() = Some(Role::Viewer);
    assert_eq!(
        got(&actions, workspace, &member, &by_agent).unwrap(),
        b"%PDF-1.7 agent",
        "a viewer reads the workspace's files"
    );
    for caller in [&member, &agent] {
        assert_eq!(
            put(&actions, workspace, caller, b"by a viewer"),
            Err(ActionHostError::NotPermitted),
            "a viewer, and their agent, puts nothing"
        );
        assert_eq!(
            invoked(&actions, workspace, caller, &open()),
            ReceiptStatus::Rejected {
                error: ActionHostError::NotPermitted
            },
            "a viewer, and their agent, opens nothing"
        );
        assert!(
            !listed(&actions, workspace, caller, &workspace_provider)
                .iter()
                .any(|action| action.address.action == "open"),
            "a viewer is not listed the open"
        );
    }
    *membership.role.lock().unwrap() = None;
    assert_eq!(
        got(&actions, workspace, &member, &by_agent),
        Err(ActionHostError::NotPermitted),
        "someone the workspace no longer admits gets nothing"
    );
    *membership.role.lock().unwrap() = Some(Role::Editor);
    // An action people alone are offered is listed to the editor and not to
    // their agent, which runs only what it is listed.
    let people_only = |caller: &Caller| {
        listed(&actions, workspace, caller, &tasks_provider)
            .into_iter()
            .filter(|action| !action.contract.reaches_agents())
            .count()
    };
    assert!(
        people_only(&member) > 0,
        "the editor is offered what people alone are"
    );
    assert_eq!(people_only(&agent), 0, "their agent is not");
    let by_agent = invoked(&actions, workspace, &agent, &new_note("By the agent"));
    assert!(
        matches!(by_agent, ReceiptStatus::Completed { .. }),
        "{by_agent:?}"
    );
    // Everything the agent is listed reaches the door that runs it: asked
    // without the argument it requires (so nothing runs), each is refused
    // for that argument, never as unknown or not theirs.
    let listed_to_agent = listed(&actions, workspace, &agent, &notes_provider);
    assert!(listed_to_agent.len() > 10, "{listed_to_agent:?}");
    for action in listed_to_agent {
        if !action.params.iter().any(|param| param.required) {
            continue;
        }
        let status = match served(
            &actions,
            workspace,
            &agent,
            RemoteActionMessage::Invoke(request(
                &notes_provider,
                action.address.clone(),
                InvokeArgument::None,
            )),
        ) {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("expected a receipt, got {other:?}"),
        };
        assert!(
            matches!(
                status,
                ReceiptStatus::Rejected {
                    error: ActionHostError::InvalidRequest { .. }
                        | ActionHostError::Unavailable { .. }
                }
            ),
            "listed {:?}, answered: {status:?}",
            action.address
        );
    }

    // Removing a book asks first: the editor's own word, or a token the host
    // never issued, confirms nothing; nobody to ask leaves it refused.
    let remove = || {
        request(
            &books_provider,
            ActionAddress::new("", "remove-from-library"),
            json(serde_json::json!({"book": "book:dusk"})),
        )
    };
    for (confirmation, what) in [
        (Confirmation::Confirmed, "their own word"),
        (Confirmation::Witnessed("x".into()), "a forged token"),
    ] {
        let refused = invoked(
            &actions,
            workspace,
            &member,
            &InvokeRequest {
                confirmation: Some(confirmation),
                ..remove()
            },
        );
        assert!(
            matches!(&refused, ReceiptStatus::Rejected { error: ActionHostError::Refused { reason } } if reason.contains("person")),
            "{what} confirms nothing: {refused:?}"
        );
    }
    assert_eq!(
        invoked(&actions, workspace, &member, &remove()),
        ReceiptStatus::Rejected {
            error: ActionHostError::ConfirmationRequired
        },
        "with nobody to ask, it stays refused"
    );
    // Asked, the member allows it: it runs on their answer.
    *membership.answer.lock().unwrap() = Some(ConfirmationReply::AllowOnce);
    let asked = InvokeRequest {
        invocation: InvocationId::new(),
        ..remove()
    };
    let first = match served(
        &actions,
        workspace,
        &member,
        RemoteActionMessage::Invoke(asked.clone()),
    ) {
        RemoteActionMessage::Receipt(receipt) => receipt.status,
        other => panic!("{other:?}"),
    };
    assert_eq!(first, ReceiptStatus::Accepted, "it waits for the member");
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let state = match served(
            &actions,
            workspace,
            &member,
            RemoteActionMessage::Invoke(asked.clone()),
        ) {
            RemoteActionMessage::Receipt(receipt) => receipt.status,
            other => panic!("{other:?}"),
        };
        match state {
            ReceiptStatus::Completed { .. } => break,
            ReceiptStatus::Accepted => {}
            other => panic!("allowed, yet it did not run: {other:?}"),
        }
        assert!(Instant::now() < deadline, "their answer never ran it");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(membership.asked.load(Ordering::SeqCst) >= 1);

    // Removed from the workspace, or their device revoked: refused, though
    // the same caller asks as before.
    *membership.role.lock().unwrap() = None;
    assert_eq!(
        invoked(&actions, workspace, &member, &new_note("By the removed")),
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );
    *membership.role.lock().unwrap() = Some(Role::Editor);
    membership.revoked.store(true, Ordering::SeqCst);
    assert_eq!(
        invoked(&actions, workspace, &member, &new_note("By the revoked")),
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );
    assert_eq!(
        invoked(
            &actions,
            workspace,
            &agent,
            &new_note("By the revoked agent")
        ),
        ReceiptStatus::Rejected {
            error: ActionHostError::NotPermitted
        }
    );

    // What was saved, in order: the editor's and the agent's notes, and no
    // note of anyone refused.
    actions.shutdown();
    let titles = titles_after(&mut browser, &notes, "Marker").await;
    for saved in ["By the editor", "By the agent"] {
        assert!(titles.iter().any(|title| title == saved), "{titles:?}");
    }
    for refused in [
        "By the viewer",
        "By the commenter",
        "By the removed",
        "By the revoked",
        "By the revoked agent",
    ] {
        assert!(!titles.iter().any(|title| title == refused), "{titles:?}");
    }
    // The editor's note is theirs, the agent's the agent's for them.
    browser.pump();
    let changes = browser.documents.get(&notes).cloned().unwrap_or_default();
    let mut doc = automerge::AutoCommit::new();
    for (_, bytes) in &changes {
        let _ = tilo_automerge::ApplyIncoming::load_incoming(&mut doc, bytes);
    }
    let invokers: Vec<tilo_actions::ActionInvoker> = doc
        .get_changes(&[])
        .iter()
        .filter_map(|change| change.message())
        .filter_map(tilo_actions::ActionContext::from_change_message)
        .filter_map(|context| context.invoker)
        .collect();
    let person = hex::encode(guest_device.cert.user_public_key);
    assert!(
        invokers
            .iter()
            .any(|invoker| invoker.actor == person
                && invoker.kind == tilo_actions::ActorKind::Person),
        "{invokers:?}"
    );
    assert!(
        invokers
            .iter()
            .any(|invoker| invoker.actor == "agent:Claude Code"
                && invoker.on_behalf_of.as_deref() == Some(person.as_str())),
        "{invokers:?}"
    );
    browser.peer.close();
    hosted.agents.shutdown();
    hosted.relay.shutdown().await;
}
