//! The feedback triage agent in a public community room, over the
//! production broker.
//!
//! The team owns the room and runs Chat; a reporter joined it as a
//! Commenter; the triage agent is admitted as a Contributor and runs from a
//! team device, as its own Chat participant. A report gets a drafted issue,
//! a second report of the same problem gets a reply linking the first, the
//! draft is filed only after an Editor approves it and carries nothing
//! personal, and removing the agent stops it.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    clippy::unreachable,
    reason = "in a test these are the assertion, not a hazard"
)]
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use account_custody::forge::fake::{FakeForge, connect};
use ed25519_dalek::{Signer as _, SigningKey};
use shell::shell::remote_actions::BrokerActionHost;
use shell::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};
use tilo_actions::remote::{
    ActionHost as _, Caller, CatalogueQuery, InvokeArgument, InvokeRequest, PendingPoll,
    RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};
use tilo_agent::triage::participant::{self, Participant, Standing};
use tilo_agent::triage::{self, forge::Forge, think::NoModel};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0x7a1a_9e00_0000_4c0d_9a11_0000_0000_0077);
const APP: &str = "so.tilo.chat";
const AGENT: &str = shell::shell::workspace_settings::TRIAGE_AGENT;
const FORGE_ACCOUNT: &str = "tilo-forge";
/// The lease, timed for a test: a session claims within a fraction of a
/// second and a vanished holder's claim expires in two.
const FAST_LEASE: triage::lease::Timing = triage::lease::Timing {
    ttl: Duration::from_secs(2),
    renew: Duration::from_millis(300),
    observe: Duration::from_millis(700),
    settle: Duration::from_millis(400),
};

struct Person {
    root: SigningKey,
    device: SigningKey,
    label: &'static str,
}

impl Person {
    fn new(root: u8, device: u8, label: &'static str) -> Self {
        Self {
            root: SigningKey::from_bytes(&[root; 32]),
            device: SigningKey::from_bytes(&[device; 32]),
            label,
        }
    }

    fn principal(&self) -> String {
        let collab_types::Principal::Person(principal) = collab_types::Principal::person_from_key(
            collab_types::Ed25519PublicKey::from_bytes(self.root.verifying_key().to_bytes()),
        ) else {
            unreachable!("a person key names a person")
        };
        principal
    }

    fn caller(&self) -> Caller {
        Caller::authenticated(
            self.root.verifying_key().to_bytes(),
            self.device.verifying_key().to_bytes(),
        )
        .in_room(ROOM.to_string())
    }

    fn identity(&self) -> ui::collaboration::CertifiedEndpointIdentity {
        let mut certificate = tilo_user_identity::DeviceCert {
            user_public_key: self.root.verifying_key().to_bytes(),
            device_public_key: self.device.verifying_key().to_bytes(),
            device_label: self.label.to_owned(),
            capabilities: tilo_user_identity::DeviceCapabilities::primary(),
            issued_at_ns: 1,
            expires_at_ns: None,
            signature: [0; 64],
        };
        tilo_user_identity::sign_device_cert(&mut certificate, &self.root)
            .expect("root signs the device");
        let presentation = runtime_collab::CertifiedJoinIdentity {
            workspace_id: ROOM.to_string(),
            device_certificate: serde_json::to_vec(&certificate).expect("certificate JSON"),
            proof: self
                .device
                .sign(&runtime_identity::collaboration_join_signing_bytes(
                    &ROOM.to_string(),
                ))
                .to_bytes()
                .to_vec(),
        }
        .encode()
        .expect("certified join encodes");
        ui::collaboration::CertifiedEndpointIdentity::new(presentation, self.device.clone())
    }

    /// A live Chat for this person, on its own connection and record store.
    fn chat(&self, socket: &Path, state: &Path) -> ui::App {
        let stream = UnixStream::connect(socket).expect("connect to the broker");
        let manager = ui::collaboration::CollaborationManager::for_route_fd_with_identity(
            stream.into_raw_fd(),
            APP,
            ROOM,
            None,
            Some(self.label.to_owned()),
            self.identity(),
        )
        .expect("start the collaboration client");
        let comments = ui::record::RecordOwners::new(state.to_path_buf())
            .open(APP, &ROOM.to_string())
            .expect("a record store in the room");
        let chat = chat::ChatApp::for_participant(&self.principal(), self.label, comments);
        let mut app =
            ui::App::with_collaboration_manager(ui::State::new(), Box::new(chat), manager);
        chat::actions::register(app.action_registry_mut());
        app.with_records(
            ui::RecordSet::in_memory().with_home(ROOM.to_string()),
            automerge::AutoCommit::new(),
        )
    }
}

/// The team's forge: a fake Forgejo repository behind the real accounts
/// service, with the forge account and its token in custody.
struct Filing {
    forge: FakeForge,
    custodian: account_custody::Custodian,
}

impl Filing {
    fn new(root: &Path) -> Self {
        let forge = FakeForge::start("team-forge-token");
        connect(root, FORGE_ACCOUNT, &forge);
        Self {
            forge,
            custodian: account_custody::Custodian::for_root(Some(root.to_path_buf())),
        }
    }

    /// The agent's view of the forge: the accounts service, reached as the
    /// launcher would connect it for tilo-agent.
    fn service(&self) -> triage::forge::AccountService {
        let custodian = self.custodian.clone();
        triage::forge::AccountService::with_connector(
            FORGE_ACCOUNT,
            Box::new(move || {
                let (mut app, service) = UnixStream::pair().ok()?;
                tilo_account_service::HANDOFF_FRAMING
                    .write(
                        &mut app,
                        &tilo_account_service::handoff("so.tilo.agent").ok()?,
                    )
                    .ok()?;
                let custodian = custodian.clone();
                std::thread::spawn(move || {
                    account_custody::service::serve_client(&custodian, service)
                });
                Some(tilo_account_service::Client::from_stream(app))
            }),
        )
    }
}

/// A forge whose filing reaches the forge and whose answer is lost, as when
/// the agent stops between filing and saying so.
struct LostAnswer(triage::forge::AccountService);

impl Forge for LostAnswer {
    fn find(&self, marker: &str) -> Result<Option<triage::forge::FiledIssue>, String> {
        self.0.find(marker)
    }

    fn file(
        &self,
        draft: &chat::feedback::IssueDraft,
        marker: &str,
    ) -> Result<triage::forge::FiledIssue, String> {
        self.0.file(draft, marker)?;
        Err("the agent stopped before it heard the answer".into())
    }
}

struct Room {
    editor: ui::App,
    author: ui::App,
    agent: Option<Participant>,
    standing: Standing,
}

impl Room {
    fn agent(&self) -> &Participant {
        self.agent.as_ref().expect("the agent has joined")
    }

    fn step(&mut self) {
        self.editor.step_headless();
        self.author.step_headless();
        if let Some(agent) = self.agent.as_mut() {
            self.standing = agent.step();
        }
        std::thread::sleep(Duration::from_millis(5));
    }

    fn until(&mut self, what: &str, mut done: impl FnMut(&mut Self) -> bool) {
        let started = Instant::now();
        while !done(self) {
            assert!(
                started.elapsed() < Duration::from_secs(30),
                "timed out waiting for {what}"
            );
            self.step();
        }
    }
}

fn catalogue_revision(
    host: &BrokerActionHost,
    person: &Person,
    provider: &str,
    room: &mut Room,
) -> String {
    let mut page = host.query(
        &person.caller(),
        CatalogueQuery {
            provider: Some(provider.to_owned()),
            max_results: 1,
            ..CatalogueQuery::default()
        },
    );
    let started = Instant::now();
    loop {
        match page.poll_ready() {
            PendingPoll::Ready(page) => return page.expect("the catalogue answers").revision,
            PendingPoll::Abandoned => panic!("the catalogue query was abandoned"),
            PendingPoll::Waiting => {}
        }
        assert!(started.elapsed() < Duration::from_secs(20), "no catalogue");
        room.step();
    }
}

fn invoke(
    host: &BrokerActionHost,
    person: &Person,
    provider: &str,
    resource: &str,
    action: &str,
    argument: InvokeArgument,
    room: &mut Room,
) -> Result<InvocationUpdate, String> {
    // Other participants' Chats republish as they work, so a revision can go
    // stale between listing and invoking; list again, as a caller would.
    for _ in 0..5 {
        match invoke_at_revision(
            host,
            person,
            provider,
            resource,
            action,
            argument.clone(),
            room,
        ) {
            Err(error) if error.contains("changed since that revision") => {}
            other => return other,
        }
    }
    invoke_at_revision(host, person, provider, resource, action, argument, room)
}

fn invoke_at_revision(
    host: &BrokerActionHost,
    person: &Person,
    provider: &str,
    resource: &str,
    action: &str,
    argument: InvokeArgument,
    room: &mut Room,
) -> Result<InvocationUpdate, String> {
    let revision = catalogue_revision(host, person, provider, room);
    let invocation = InvocationId::new();
    let mut pending = host.invoke(
        &person.caller(),
        InvokeRequest {
            invocation,
            target: RemoteActionTarget {
                provider: provider.to_owned(),
                address: ActionAddress::new(resource, action),
            },
            argument,
            context: None,
            confirmation: None,
            revision: Some(revision),
        },
    );
    let started = Instant::now();
    loop {
        match pending.poll_ready() {
            PendingPoll::Ready(result) => {
                result.map_err(|error| error.to_string())?;
                break;
            }
            PendingPoll::Abandoned => return Err("the invocation was abandoned".into()),
            PendingPoll::Waiting => {}
        }
        if started.elapsed() > Duration::from_secs(20) {
            return Err(format!("{resource}/{action} unanswered"));
        }
        room.step();
    }
    loop {
        if let Some(state) = host.observe(&person.caller(), &invocation)
            && state.is_terminal()
        {
            return Ok(state.update);
        }
        if started.elapsed() > Duration::from_secs(20) {
            return Err(format!("{resource}/{action} never finished"));
        }
        room.step();
    }
}

fn succeeded(result: Result<InvocationUpdate, String>, what: &str) {
    match result {
        Ok(InvocationUpdate::Succeeded { .. }) => {}
        other => panic!("{what} did not succeed: {other:?}"),
    }
}

/// The provider token under which `person` sees a Chat other than `others`.
fn chat_provider(
    host: &BrokerActionHost,
    person: &Person,
    others: &[&str],
    room: &mut Room,
) -> String {
    let mut provider = None;
    room.until("Chat's published surface", |room| {
        let mut page = host.query(
            &person.caller(),
            CatalogueQuery {
                needle: "chat.workspace".into(),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        let _ = room;
        if let PendingPoll::Ready(Ok(page)) = page.poll_ready() {
            provider = page
                .rows
                .iter()
                .find(|row| row.app_id == APP && !others.contains(&row.provider.as_str()))
                .map(|row| row.provider.clone());
        }
        provider.is_some()
    });
    provider.expect("found")
}

/// The provider token of `person`'s own Chat: the only one whose posting
/// actions the broker offers them, since nobody posts through another
/// participant's Chat.
fn own_chat(host: &BrokerActionHost, person: &Person, room: &mut Room) -> String {
    let conversation = format!("chat.channel:{}", tilo_reference::ROOM_CONVERSATION);
    let mut provider = None;
    room.until("the person's own Chat", |_| {
        let mut page = host.query(
            &person.caller(),
            CatalogueQuery {
                needle: "remark".into(),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
        if let PendingPoll::Ready(Ok(page)) = page.poll_ready() {
            let mut own = page
                .rows
                .iter()
                .filter(|row| {
                    row.app_id == APP
                        && row.action.as_ref().is_some_and(|action| {
                            action.address.resource == conversation
                                && action.address.action == "remark"
                        })
                })
                .map(|row| row.provider.clone());
            provider = own.next().filter(|_| own.next().is_none());
        }
        provider.is_some()
    });
    provider.expect("found")
}

fn may(app: &ui::App, verb: ui::permissions::Verb) -> bool {
    app.permission_ctx()
        .check(&ui::permissions::PermissionScope::workspace(verb))
}

/// A remark about the Mail To field, as the reporter's composer posts one.
fn remark(text: &str, id: &str) -> InvokeArgument {
    InvokeArgument::Json(serde_json::Map::from_iter([
        ("text".to_string(), serde_json::Value::String(text.into())),
        (
            "reference".to_string(),
            serde_json::json!({"kind": "mail.field", "id": "mail.field:to", "label": "To", "span": null}),
        ),
        ("message".to_string(), serde_json::Value::String(id.into())),
    ]))
}

/// The agent's replies under `report`, as the team's Chat holds them.
fn agent_replies(room: &Room, report: &str) -> Vec<chat::model::Message> {
    let me = room.agent().me().to_string();
    room.agent()
        .hand()
        .messages(tilo_reference::FEEDBACK_CHANNEL)
        .into_iter()
        .filter(|m| m.author == me && m.parent.as_deref() == Some(report))
        .collect()
}

/// Whether the team's Chat has followed a record of the agent's whose
/// message text contains `needle`.
fn team_sees(room: &Room, needle: &str) -> bool {
    let Some(joined) = room.editor.root_widget_for_test().joined_records() else {
        return false;
    };
    joined.joined().iter().any(|checkout| {
        checkout.kind.as_str() == chat::comments::MESSAGE_KIND
            && joined
                .read(&checkout.id, chat::comments::message_text_of)
                .flatten()
                .is_some_and(|text| text.contains(needle))
    })
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario across the team, a reporter and the agent, read top to bottom"
)]
fn the_triage_agent_links_a_repeat_files_only_what_an_editor_approved_and_stops_when_removed() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new(
            "info,tilo_agent=debug,ui_core::app=debug",
        ))
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().expect("scratch");
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let socket = dir.path().join("collab.sock");
    let team = Person::new(81, 82, "team");
    let reporter = Person::new(91, 92, "reporter");
    // The agent runs from a team device: only an administrator may launch
    // a delegated agent session.
    let agent_device = Person::new(81, 83, "triage");

    let mut workspace = Workspace::new(WorkspaceId(1), "Tilo Community");
    workspace.room = RoomId::parse(&ROOM.to_string()).expect("room id");
    workspace.channel_class = collab_types::ChannelClass::Public;
    workspace.set_owner(team.principal());
    workspace.grant_capability(collab_types::CapOverride::membership(
        collab_types::Subject::person(reporter.principal()),
        ROOM.to_string(),
        collab_types::Verb::Comment,
    ));
    // The triage agent's Contributor grants, as Workspace Settings adds them.
    for cap in shell::shell::workspace_settings::triage_agent_grants(&ROOM.to_string(), AGENT) {
        workspace.grant_capability(cap);
    }
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let tree = tree.shared();
    let handle = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        tree.clone(),
        vec![
            team.device.verifying_key().to_bytes(),
            reporter.device.verifying_key().to_bytes(),
            agent_device.device.verifying_key().to_bytes(),
        ],
    )
    .expect("bind the broker");
    let host = BrokerActionHost::new(handle.app_action_broker().expect("typed broker"));

    let editor = team.chat(&socket, &dir.path().join("team"));
    let author = reporter.chat(&socket, &dir.path().join("reporter"));
    let mut room = Room {
        editor,
        author,
        agent: None,
        standing: Standing::Waiting,
    };

    // The team seeds the conversation and makes #feedback.
    room.until("the owner's grant and the conversation", |room| {
        may(&room.editor, ui::permissions::Verb::Write)
            && chat::doc::ChatDoc::is_seeded(&room.editor.state_mut_for_test().doc)
    });
    let workspace_id = chat::doc::ChatDoc::list_workspaces(&room.editor.state_mut_for_test().doc)
        .first()
        .expect("a starter workspace")
        .id
        .clone();
    let team_chat = own_chat(&host, &team, &mut room);
    let reporter_chat = own_chat(&host, &reporter, &mut room);

    // The agent joins as its own Chat participant. Everything it keeps
    // lives under `agent/`, so a restart finds it again.
    let filing = Filing::new(&dir.path().join("accounts"));
    let join = |forge: Box<dyn Forge>| {
        Participant::connect(
            participant::Config {
                room: ROOM,
                name: AGENT.into(),
                scope: triage::Scope::community(),
                endpoint: Some(socket.clone()),
                identity: Some(agent_device.identity()),
                records: ui::record::RecordOwners::new(dir.path().join("agent"))
                    .open(APP, &ROOM.to_string())
                    .expect("the agent's record store"),
                intents: triage::forge::Intents::at(dir.path().join("agent/intents.json")),
                lease: FAST_LEASE,
            },
            Box::new(NoModel),
            forge,
        )
        .expect("the agent joins")
    };
    room.agent = Some(join(Box::new(LostAnswer(filing.service()))));
    let feedback = tilo_reference::FEEDBACK_CHANNEL;
    succeeded(
        invoke(
            &host,
            &team,
            &team_chat,
            &format!("chat.workspace:{workspace_id}"),
            "add-feedback-channel",
            InvokeArgument::None,
            &mut room,
        ),
        "adding #feedback",
    );
    succeeded(
        invoke(
            &host,
            &team,
            &team_chat,
            &format!("chat.channel:{feedback}"),
            "mark-feedback",
            InvokeArgument::None,
            &mut room,
        ),
        "marking #feedback",
    );
    room.until("the reporter's grant", |room| {
        may(&room.author, ui::permissions::Verb::Comment)
    });
    room.until("the agent's grant", |room| {
        room.standing != Standing::Waiting
    });
    room.until("the agent to see #feedback", |room| {
        room.agent()
            .hand()
            .channels()
            .iter()
            .any(|c| c.id == feedback && c.feedback)
    });
    assert!(
        !room
            .agent()
            .hand()
            .permits(&ui::permissions::PermissionScope::action(
                APP,
                "approve-issue",
                ui::permissions::Verb::Write
            )),
        "the agent is a Contributor, never an Editor"
    );

    // A report, with a person and an address in its words.
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{feedback}"),
            "remark",
            remark(
                "Send does nothing when the To field ends with a comma, \
                 e.g. writing to ada.lovelace@example.com,",
                "m_first",
            ),
            &mut room,
        ),
        "posting the first report",
    );
    room.until("the agent to draft an issue for it", |room| {
        agent_replies(room, "m_first")
            .iter()
            .any(|m| m.attachment.as_ref().is_some_and(|a| a.issue().is_some()))
    });
    let draft_reply = agent_replies(&room, "m_first")
        .into_iter()
        .find(|m| m.attachment.as_ref().is_some_and(|a| a.issue().is_some()))
        .expect("the draft");
    let draft = draft_reply
        .attachment
        .as_ref()
        .and_then(chat::model::Attachment::issue)
        .cloned()
        .expect("an issue");
    assert!(
        !format!("{}{}", draft.title, draft.body).contains("ada.lovelace@example.com"),
        "the draft carries nothing personal: {draft:?}"
    );
    room.until("the team to follow the draft", |room| {
        team_sees(room, "drafted a forge issue")
    });
    assert!(filing.forge.issues().is_empty(), "nothing is filed yet");

    // The same problem again: a reply linking the first report.
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{feedback}"),
            "remark",
            remark(
                "Pressing Send does nothing if the To field has a trailing comma",
                "m_second",
            ),
            &mut room,
        ),
        "posting the second report",
    );
    room.until("the agent to link the earlier report", |room| {
        agent_replies(room, "m_second").iter().any(|m| {
            m.reference
                .as_ref()
                .is_some_and(|r| r.kind == "chat.message" && r.id == "chat.message:m_first")
        })
    });
    assert!(
        agent_replies(&room, "m_second")
            .iter()
            .all(|m| m.attachment.is_none()),
        "a repeat gets no issue of its own"
    );
    room.until("the team to see the suggestion", |room| {
        team_sees(room, "same problem as an earlier report")
    });
    assert!(
        chat::doc::ChatDoc::comment_report(&room.editor.state_mut_for_test().doc, "m_second")
            .is_none(),
        "the agent suggests; it never sets a status"
    );

    // Nobody speaks through another participant's Chat: the reporter's
    // remark addressed to the agent's Chat is refused, and nothing is
    // written under the agent's device.
    let agent_chat = chat_provider(&host, &team, &[&team_chat, &reporter_chat], &mut room);
    let before = agent_replies(&room, "m_first").len();
    let spoofed = invoke(
        &host,
        &reporter,
        &agent_chat,
        &format!("chat.channel:{feedback}"),
        "remark",
        remark("Written in the agent's name", "m_spoofed"),
        &mut room,
    );
    assert!(
        !matches!(spoofed, Ok(InvocationUpdate::Succeeded { .. })),
        "a remark through another participant's Chat is refused: {spoofed:?}"
    );
    for _ in 0..20 {
        room.step();
    }
    assert!(
        !team_sees(&room, "Written in the agent's name"),
        "nothing was written under the agent's device"
    );
    assert_eq!(agent_replies(&room, "m_first").len(), before);

    // Only an Editor approves. The reporter cannot.
    let refused = invoke(
        &host,
        &reporter,
        &reporter_chat,
        &format!("chat.message:{}", draft_reply.id),
        "approve-issue",
        InvokeArgument::None,
        &mut room,
    );
    assert!(
        !matches!(refused, Ok(InvocationUpdate::Succeeded { .. })),
        "a Commenter does not approve: {refused:?}"
    );
    for _ in 0..40 {
        room.step();
    }
    assert!(filing.forge.issues().is_empty(), "no approval, no issue");
    succeeded(
        invoke(
            &host,
            &team,
            &team_chat,
            &format!("chat.message:{}", draft_reply.id),
            "approve-issue",
            InvokeArgument::None,
            &mut room,
        ),
        "the Editor approving",
    );
    // The agent files, and stops before it can say so in the thread.
    room.until("the agent to file", |_| !filing.forge.issues().is_empty());
    for _ in 0..20 {
        room.step();
    }
    let said_filed = |room: &Room| {
        agent_replies(room, "m_first").iter().any(|m| {
            m.reference
                .as_ref()
                .is_some_and(|r| r.kind == triage::ISSUE_KIND)
        })
    };
    assert!(!said_filed(&room), "the answer was lost");
    room.agent = None;
    for _ in 0..20 {
        room.step();
    }

    // Restarted, it finds its intent and the issue it filed, and only says
    // so: the forge has exactly the approved draft, once.
    room.agent = Some(join(Box::new(filing.service())));
    room.until("the restarted agent to say where it filed", |room| {
        said_filed(room)
    });
    for _ in 0..40 {
        room.step();
    }
    let issues = filing.forge.issues();
    assert_eq!(issues.len(), 1, "filed once across the restart: {issues:?}");
    assert_eq!(issues[0].title, draft.title);
    assert!(issues[0].body.starts_with(draft.body.trim_end()));
    assert!(
        !issues[0].body.contains("ada.lovelace@example.com"),
        "the filed issue carries nothing personal"
    );

    // The team removes the agent. It hears so and stops.
    let revoked = tree
        .write()
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room.to_string() == ROOM.to_string())
        .expect("the room")
        .revoke_matching(|capability| capability.subject == collab_types::Subject::agent(AGENT));
    assert!(!revoked.is_empty(), "the agent's grants were revoked");
    room.until("the agent to stop", |room| {
        room.standing == Standing::Revoked
    });
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{feedback}"),
            "remark",
            remark("The To field also rejects semicolons", "m_third"),
            &mut room,
        ),
        "posting after the agent left",
    );
    room.until("the team to see the third report", |room| {
        team_sees(room, "rejects semicolons")
    });
    for _ in 0..60 {
        room.step();
    }
    assert_eq!(room.standing, Standing::Revoked);
    assert!(
        agent_replies(&room, "m_third").is_empty(),
        "a removed agent does nothing"
    );
    drop(handle);
}

/// In a direct room the agent is a Contributor too: it may not write the
/// conversation, so its reply to the reporter's private report is its own
/// comment record in the room's ordinary conversation channel, which the
/// team follows like any other message.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario across the team, a reporter and the agent, read top to bottom"
)]
fn the_triage_agent_answers_a_private_report_in_a_direct_room() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info,tilo_agent=debug"))
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().expect("scratch");
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let socket = dir.path().join("collab.sock");
    let team = Person::new(81, 82, "team");
    let reporter = Person::new(91, 92, "reporter");
    let agent_device = Person::new(81, 83, "triage");

    let mut workspace = Workspace::new(WorkspaceId(1), "Reporter and the Tilo team");
    workspace.room = RoomId::parse(&ROOM.to_string()).expect("room id");
    workspace.set_owner(team.principal());
    workspace.grant_capability(collab_types::CapOverride::membership(
        collab_types::Subject::person(reporter.principal()),
        ROOM.to_string(),
        collab_types::Verb::Write,
    ));
    for cap in shell::shell::workspace_settings::triage_agent_grants(&ROOM.to_string(), AGENT) {
        workspace.grant_capability(cap);
    }
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let handle = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        tree.shared(),
        vec![
            team.device.verifying_key().to_bytes(),
            reporter.device.verifying_key().to_bytes(),
            agent_device.device.verifying_key().to_bytes(),
        ],
    )
    .expect("bind the broker");
    let host = BrokerActionHost::new(handle.app_action_broker().expect("typed broker"));
    let mut room = Room {
        editor: team.chat(&socket, &dir.path().join("team")),
        author: reporter.chat(&socket, &dir.path().join("reporter")),
        agent: None,
        standing: Standing::Waiting,
    };
    room.until("the conversation", |room| {
        may(&room.editor, ui::permissions::Verb::Write)
            && chat::doc::ChatDoc::is_seeded(&room.editor.state_mut_for_test().doc)
    });
    let reporter_chat = own_chat(&host, &reporter, &mut room);
    room.agent = Some(
        Participant::connect(
            participant::Config {
                room: ROOM,
                name: AGENT.into(),
                scope: triage::Scope::direct(),
                endpoint: Some(socket),
                identity: Some(agent_device.identity()),
                records: ui::record::RecordOwners::new(dir.path().join("agent"))
                    .open(APP, &ROOM.to_string())
                    .expect("the agent's record store"),
                intents: triage::forge::Intents::in_memory(),
                lease: FAST_LEASE,
            },
            Box::new(NoModel),
            Box::new(triage::forge::Unconnected),
        )
        .expect("the agent joins"),
    );
    room.until("the agent's grant", |room| {
        room.standing != Standing::Waiting
    });
    let conversation = tilo_reference::ROOM_CONVERSATION;

    // The reporter's private report is a row of the conversation: they may
    // write it.
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{conversation}"),
            "remark",
            remark(
                "Send does nothing when the To field ends with a comma",
                "m_private",
            ),
            &mut room,
        ),
        "posting the private report",
    );
    room.until("the agent to draft an issue for it", |room| {
        let me = room.agent().me().to_string();
        room.agent().hand().messages(conversation).iter().any(|m| {
            m.author == me
                && m.parent.as_deref() == Some("m_private")
                && m.attachment.as_ref().is_some_and(|a| a.issue().is_some())
        })
    });
    room.until("the team to see the agent's reply", |room| {
        team_sees(room, "drafted a forge issue")
    });
    let me = room.agent().me().to_string();
    assert!(
        chat::doc::ChatDoc::list_messages(&room.editor.state_mut_for_test().doc, conversation)
            .iter()
            .all(|m| m.author != me),
        "the agent wrote nothing into the conversation document"
    );
    drop(handle);
}

/// Each device's triage session, restarted after it yields, as its
/// desktop's triage runtime restarts it.
struct TeamDevice {
    person: Person,
    agent: Option<Participant>,
    ended: Option<Instant>,
    records: std::path::PathBuf,
}

/// Two team devices both run the triage agent in the community room, the
/// way each team desktop keeps it running. Their sessions settle on one
/// holder through the room lease, so a report is answered once; when the
/// holder's device goes away, the other device's next session takes over.
#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario across two team devices, read top to bottom"
)]
fn two_team_devices_run_one_triage_session_and_fail_over() {
    let _ = tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::new("info,tilo_agent=trace"))
        .with_test_writer()
        .try_init();
    let dir = tempfile::tempdir().expect("scratch");
    // The broker binds only inside an owner-only directory.
    std::fs::set_permissions(
        dir.path(),
        std::os::unix::fs::PermissionsExt::from_mode(0o700),
    )
    .expect("owner-only socket directory");
    let socket = dir.path().join("collab.sock");
    let team = Person::new(81, 82, "team");
    let reporter = Person::new(91, 92, "reporter");
    let mut devices = vec![
        TeamDevice {
            person: Person::new(81, 83, "triage-a"),
            agent: None,
            ended: None,
            records: dir.path().join("device-a"),
        },
        TeamDevice {
            person: Person::new(81, 84, "triage-b"),
            agent: None,
            ended: None,
            records: dir.path().join("device-b"),
        },
    ];

    let mut workspace = Workspace::new(WorkspaceId(1), "Tilo Community");
    workspace.room = RoomId::parse(&ROOM.to_string()).expect("room id");
    workspace.channel_class = collab_types::ChannelClass::Public;
    workspace.set_owner(team.principal());
    workspace.grant_capability(collab_types::CapOverride::membership(
        collab_types::Subject::person(reporter.principal()),
        ROOM.to_string(),
        collab_types::Verb::Comment,
    ));
    for cap in shell::shell::workspace_settings::triage_agent_grants(&ROOM.to_string(), AGENT) {
        workspace.grant_capability(cap);
    }
    let mut tree = WorkspaceTree::default();
    tree.desktop.active = workspace.id;
    tree.desktop.workspaces.push(workspace);
    let handle = shell::shell::collab_broker::spawn_native_harness(
        socket.clone(),
        tree.shared(),
        vec![
            team.device.verifying_key().to_bytes(),
            reporter.device.verifying_key().to_bytes(),
            devices[0].person.device.verifying_key().to_bytes(),
            devices[1].person.device.verifying_key().to_bytes(),
        ],
    )
    .expect("bind the broker");
    let host = BrokerActionHost::new(handle.app_action_broker().expect("typed broker"));
    let mut room = Room {
        editor: team.chat(&socket, &dir.path().join("team")),
        author: reporter.chat(&socket, &dir.path().join("reporter")),
        agent: None,
        standing: Standing::Waiting,
    };
    room.until("the conversation", |room| {
        may(&room.editor, ui::permissions::Verb::Write)
            && chat::doc::ChatDoc::is_seeded(&room.editor.state_mut_for_test().doc)
    });
    let workspace_id = chat::doc::ChatDoc::list_workspaces(&room.editor.state_mut_for_test().doc)
        .first()
        .expect("a starter workspace")
        .id
        .clone();
    let team_chat = own_chat(&host, &team, &mut room);
    let reporter_chat = own_chat(&host, &reporter, &mut room);
    let feedback = tilo_reference::FEEDBACK_CHANNEL;
    for (resource, action) in [
        (
            format!("chat.workspace:{workspace_id}"),
            "add-feedback-channel",
        ),
        (format!("chat.channel:{feedback}"), "mark-feedback"),
    ] {
        succeeded(
            invoke(
                &host,
                &team,
                &team_chat,
                &resource,
                action,
                InvokeArgument::None,
                &mut room,
            ),
            action,
        );
    }
    room.until("the reporter's grant", |room| {
        may(&room.author, ui::permissions::Verb::Comment)
    });

    let restart_after = Duration::from_millis(500);
    let connect = |device: &TeamDevice| {
        Participant::connect(
            participant::Config {
                room: ROOM,
                name: AGENT.into(),
                scope: triage::Scope::community(),
                endpoint: Some(socket.clone()),
                identity: Some(device.person.identity()),
                records: ui::record::RecordOwners::new(device.records.clone())
                    .open(APP, &ROOM.to_string())
                    .expect("the device's record store"),
                intents: triage::forge::Intents::in_memory(),
                lease: FAST_LEASE,
            },
            Box::new(NoModel),
            Box::new(triage::forge::Unconnected),
        )
        .expect("the agent joins")
    };
    // Both devices start at once.
    for device in &mut devices {
        device.agent = Some(connect(device));
    }
    let step = |room: &mut Room, devices: &mut [TeamDevice], online: &[bool; 2]| {
        room.step();
        for (device, online) in devices.iter_mut().zip(online) {
            if !online {
                device.agent = None;
                continue;
            }
            match device.agent.as_mut().map(Participant::step) {
                Some(Standing::Yielded | Standing::Revoked) => {
                    device.agent = None;
                    device.ended = Some(Instant::now());
                }
                Some(_) => {}
                None => {
                    if device
                        .ended
                        .is_none_or(|ended| ended.elapsed() >= restart_after)
                    {
                        device.agent = Some(connect(device));
                    }
                }
            }
        }
    };
    let holders = |devices: &[TeamDevice]| {
        devices
            .iter()
            .enumerate()
            .filter(|(_, device)| {
                device
                    .agent
                    .as_ref()
                    .is_some_and(Participant::claimed_lease)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>()
    };
    let replies = |room: &Room, report: &str| {
        let me = participant::participant_id(AGENT);
        room.editor
            .root_widget_for_test()
            .joined_records()
            .map_or(0, |joined| {
                joined
                    .joined()
                    .iter()
                    .filter(|checkout| {
                        checkout.kind.as_str() == chat::comments::MESSAGE_KIND
                            && joined
                                .read(&checkout.id, chat::comments::message_of)
                                .flatten()
                                .is_some_and(|m| {
                                    m.author == me && m.parent.as_deref() == Some(report)
                                })
                    })
                    .count()
            })
    };

    // One holder settles.
    let both = [true, true];
    let started = Instant::now();
    loop {
        step(&mut room, &mut devices, &both);
        if holders(&devices).len() == 1 && started.elapsed() > FAST_LEASE.ttl {
            break;
        }
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no single holder: {:?}",
            holders(&devices)
        );
    }
    let holder = holders(&devices)[0];

    // A report is answered once.
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{feedback}"),
            "remark",
            remark(
                "Send does nothing when the To field ends with a comma",
                "m_first",
            ),
            &mut room,
        ),
        "posting the first report",
    );
    let started = Instant::now();
    while replies(&room, "m_first") == 0 {
        step(&mut room, &mut devices, &both);
        assert!(started.elapsed() < Duration::from_secs(30), "no answer");
    }
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(3) {
        step(&mut room, &mut devices, &both);
    }
    assert_eq!(replies(&room, "m_first"), 1, "answered by one session only");
    assert_eq!(holders(&devices), vec![holder], "the holder kept the room");

    // The holder's device goes away; the other device takes over.
    let mut online = [true, true];
    online[holder] = false;
    let other = 1 - holder;
    let started = Instant::now();
    loop {
        step(&mut room, &mut devices, &online);
        if holders(&devices) == vec![other] {
            break;
        }
        assert!(started.elapsed() < Duration::from_secs(30), "no failover");
    }
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{feedback}"),
            "remark",
            remark("The sidebar font is blurry at 150% scale", "m_second"),
            &mut room,
        ),
        "posting the second report",
    );
    let started = Instant::now();
    while replies(&room, "m_second") == 0 {
        step(&mut room, &mut devices, &online);
        assert!(
            started.elapsed() < Duration::from_secs(30),
            "no answer after failover"
        );
    }
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(2) {
        step(&mut room, &mut devices, &online);
    }
    assert_eq!(replies(&room, "m_second"), 1);
    drop(handle);
}
