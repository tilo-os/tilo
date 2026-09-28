//! A public feedback channel end to end, over the production broker.
//!
//! Two people run Chat against one room: the team's Editor, who owns it, and
//! a Commenter who joined it. Everything goes through the published action
//! surface as tilo-control or an agent would: the Editor makes `#feedback`
//! a feedback channel, the Commenter posts a report, replies and votes —
//! each written to a comment record the Commenter's device owns, found by
//! the Editor through the room's index and followed — and the Editor sets
//! the report's status, which a Commenter may not.
#![allow(
    clippy::significant_drop_tightening,
    reason = "test fixtures transitively own long-lived permits and lock guards by design"
)]

use std::os::fd::IntoRawFd as _;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::time::{Duration, Instant};

use ed25519_dalek::{Signer as _, SigningKey};
use shell::shell::remote_actions::BrokerActionHost;
use shell::shell::state::{RoomId, Workspace, WorkspaceId, WorkspaceTree};
use tilo_actions::remote::{
    ActionHost as _, ActionHostError, Caller, CatalogueQuery, InvokeArgument, InvokeRequest,
    PendingPoll, RemoteActionTarget,
};
use tilo_actions::{ActionAddress, InvocationId, InvocationUpdate};

const ROOM: uuid::Uuid = uuid::Uuid::from_u128(0xfeed_bac4_0000_4c0d_9a11_0000_0000_0042);
const APP: &str = "so.tilo.chat";

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
    /// What it may do is the broker's to say.
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

fn step(apps: &mut [&mut ui::App]) {
    for app in apps.iter_mut() {
        app.step_headless();
    }
    std::thread::sleep(Duration::from_millis(5));
}

fn until(apps: &mut [&mut ui::App], what: &str, mut done: impl FnMut(&mut [&mut ui::App]) -> bool) {
    let started = Instant::now();
    while !done(apps) {
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "timed out waiting for {what}"
        );
        step(apps);
    }
}

/// The revision of `provider`'s live catalogue that `person` sees — what
/// a caller that knows its target's address chooses it at; the provider
/// resolves the address at that revision when the invocation arrives.
fn catalogue_revision(
    host: &BrokerActionHost,
    person: &Person,
    provider: &str,
    apps: &mut [&mut ui::App],
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
        assert!(
            started.elapsed() < Duration::from_secs(20),
            "the catalogue never answered"
        );
        step(apps);
    }
}

/// Invoke through the broker as `person` and run the apps until the
/// invocation ends. The answer is the terminal update. Like any caller, it
/// lists the catalogue again when the provider moved on from the revision the
/// action was chosen at — another member's change arriving in between.
fn invoke(
    host: &BrokerActionHost,
    person: &Person,
    provider: &str,
    resource: &str,
    action: &str,
    argument: &InvokeArgument,
    apps: &mut [&mut ui::App],
) -> Result<InvocationUpdate, String> {
    let started = Instant::now();
    let invocation = 'admitted: loop {
        let revision = catalogue_revision(host, person, provider, apps);
        let invocation = InvocationId::new();
        let mut pending = host.invoke(
            &person.caller(),
            InvokeRequest {
                invocation,
                target: RemoteActionTarget {
                    provider: provider.to_owned(),
                    address: ActionAddress::new(resource, action),
                },
                argument: argument.clone(),
                context: None,
                confirmation: None,
                revision: Some(revision),
            },
        );
        loop {
            match pending.poll_ready() {
                PendingPoll::Ready(Err(ActionHostError::StaleRevision))
                    if started.elapsed() < Duration::from_secs(20) =>
                {
                    continue 'admitted;
                }
                PendingPoll::Ready(result) => {
                    result.map_err(|error| error.to_string())?;
                    break 'admitted invocation;
                }
                PendingPoll::Abandoned => return Err("the invocation was abandoned".into()),
                PendingPoll::Waiting => {}
            }
            if started.elapsed() > Duration::from_secs(20) {
                return Err(format!("{resource}/{action} unanswered"));
            }
            step(apps);
        }
    };
    loop {
        if let Some(state) = host.observe(&person.caller(), &invocation)
            && state.is_terminal()
        {
            return Ok(state.update);
        }
        if started.elapsed() > Duration::from_secs(20) {
            return Err(format!("{resource}/{action} never finished"));
        }
        step(apps);
    }
}

fn succeeded(result: Result<InvocationUpdate, String>, what: &str) {
    match result {
        Ok(InvocationUpdate::Succeeded { .. }) => {}
        other => panic!("{what} did not succeed: {other:?}"),
    }
}

/// The provider token under which `person` sees a Chat other than the
/// `others` already found — the app that joined last.
fn chat_provider(
    host: &BrokerActionHost,
    person: &Person,
    others: &[&str],
    apps: &mut [&mut ui::App],
) -> String {
    let mut provider = None;
    until(apps, "Chat's published surface", |_| {
        let mut page = host.query(
            &person.caller(),
            CatalogueQuery {
                needle: "chat.workspace".into(),
                max_results: tilo_actions::remote::MAX_CATALOGUE_PAGE,
                ..CatalogueQuery::default()
            },
        );
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

/// Whether `app` may do `verb` in its room, as its broker last said.
fn may(app: &ui::App, verb: ui::permissions::Verb) -> bool {
    app.permission_ctx()
        .check(&ui::permissions::PermissionScope::workspace(verb))
}

fn conversation_has(app: &mut ui::App, test: impl Fn(&automerge::AutoCommit) -> bool) -> bool {
    test(&app.state_mut_for_test().doc)
}

#[test]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario across two people, read top to bottom"
)]
fn a_commenter_reports_votes_and_replies_and_an_editor_follows_and_triages() {
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

    let mut workspace = Workspace::new(WorkspaceId(1), "Tilo Community");
    workspace.room = RoomId::parse(&ROOM.to_string()).expect("room id");
    workspace.channel_class = collab_types::ChannelClass::Public;
    workspace.set_owner(team.principal());
    workspace.grant_capability(collab_types::CapOverride::membership(
        collab_types::Subject::person(reporter.principal()),
        ROOM.to_string(),
        collab_types::Verb::Comment,
    ));
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
        ],
    )
    .expect("bind the broker");
    let host = BrokerActionHost::new(handle.app_action_broker().expect("typed broker"));

    // The team's Chat is told by the broker that it owns the room, and
    // seeds the room's conversation.
    let mut editor = team.chat(&socket, &dir.path().join("team"));
    until(&mut [&mut editor], "the owner's grant", |apps| {
        may(apps[0], ui::permissions::Verb::Write)
    });
    until(&mut [&mut editor], "the conversation to exist", |apps| {
        conversation_has(apps[0], chat::doc::ChatDoc::is_seeded)
    });
    let workspace_id = chat::doc::ChatDoc::list_workspaces(&editor.state_mut_for_test().doc)
        .first()
        .expect("a starter workspace")
        .id
        .clone();
    let team_chat = chat_provider(&host, &team, &[], &mut [&mut editor]);
    succeeded(
        invoke(
            &host,
            &team,
            &team_chat,
            &format!("chat.workspace:{workspace_id}"),
            "add-feedback-channel",
            &InvokeArgument::None,
            &mut [&mut editor],
        ),
        "adding #feedback",
    );
    let feedback = tilo_reference::FEEDBACK_CHANNEL;
    succeeded(
        invoke(
            &host,
            &team,
            &team_chat,
            &format!("chat.channel:{feedback}"),
            "mark-feedback",
            &InvokeArgument::None,
            &mut [&mut editor],
        ),
        "marking #feedback",
    );

    // The reporter joins, a Commenter by the broker's word: its Chat reads
    // the team's conversation and writes nothing into it.
    let mut author = reporter.chat(&socket, &dir.path().join("reporter"));
    until(
        &mut [&mut editor, &mut author],
        "the reporter's grant",
        |apps| may(apps[1], ui::permissions::Verb::Comment),
    );
    assert!(
        !may(&author, ui::permissions::Verb::Write),
        "a Commenter is not told it may write"
    );
    until(
        &mut [&mut editor, &mut author],
        "the reporter to see #feedback",
        |apps| {
            conversation_has(apps[1], |doc| {
                chat::doc::ChatDoc::is_feedback_channel(doc, feedback)
            })
        },
    );
    let reporter_chat = chat_provider(
        &host,
        &reporter,
        &[&team_chat],
        &mut [&mut editor, &mut author],
    );

    // The report, a Commenter's comment record, never the conversation.
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.channel:{feedback}"),
            "post",
            &InvokeArgument::Text("Send does nothing when the To field ends with a comma".into()),
            &mut [&mut editor, &mut author],
        ),
        "posting a report",
    );
    until(
        &mut [&mut editor, &mut author],
        "the report record",
        |apps| {
            apps[1]
                .root_widget_for_test()
                .joined_records()
                .is_some_and(|joined| !joined.is_empty())
        },
    );
    let own = author
        .root_widget_for_test()
        .joined_records()
        .expect("chat joins records")
        .joined();
    let report_record = own.first().expect("the report").id.clone();
    assert_eq!(
        report_record.comment_author(),
        Some(reporter.device.verifying_key().to_bytes()),
        "the report is the reporter device's own record"
    );
    assert!(
        chat::doc::ChatDoc::list_messages(&author.state_mut_for_test().doc, feedback).is_empty(),
        "nothing was written into the conversation"
    );
    let report_id = author
        .root_widget_for_test()
        .joined_records()
        .and_then(|joined| {
            joined.read(&report_record, |doc| {
                chat::comments::message_id_of(doc).expect("a message")
            })
        })
        .expect("readable");

    // The team finds it through the room's index and follows it.
    until(
        &mut [&mut editor, &mut author],
        "the team to follow the report",
        |apps| {
            apps[0]
                .root_widget_for_test()
                .joined_records()
                .and_then(|joined| joined.read(&report_record, chat::comments::message_text_of))
                .flatten()
                .is_some_and(|text| text.contains("To field"))
        },
    );

    // A vote and a reply, each the reporter's own record too.
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.message:{report_id}"),
            "vote",
            &InvokeArgument::None,
            &mut [&mut editor, &mut author],
        ),
        "voting",
    );
    succeeded(
        invoke(
            &host,
            &reporter,
            &reporter_chat,
            &format!("chat.message:{report_id}"),
            "reply",
            &InvokeArgument::Text("It also happens with a trailing space".into()),
            &mut [&mut editor, &mut author],
        ),
        "replying",
    );
    until(
        &mut [&mut editor, &mut author],
        "the team to see the vote and the reply",
        |apps| {
            let Some(joined) = apps[0].root_widget_for_test().joined_records() else {
                return false;
            };
            let records = joined.joined();
            let voted = records.iter().any(|c| {
                c.kind.as_str() == chat::comments::REACTIONS_KIND
                    && joined
                        .read(&c.id, |doc| {
                            chat::comments::reacts_with(doc, chat::feedback::VOTE_EMOJI)
                        })
                        .unwrap_or(false)
            });
            let replied = records.iter().any(|c| {
                c.id.comment_parent().as_ref() == Some(&report_record)
                    && c.kind.as_str() == chat::comments::MESSAGE_KIND
                    && joined
                        .read(&c.id, chat::comments::message_text_of)
                        .flatten()
                        .is_some_and(|text| text.contains("trailing space"))
            });
            voted && replied
        },
    );

    // Status is the team's: the reporter is refused, the Editor is not.
    let refused = invoke(
        &host,
        &reporter,
        &reporter_chat,
        &format!("chat.message:{report_id}"),
        "set-status",
        &InvokeArgument::Json(serde_json::Map::from_iter([(
            "status".to_string(),
            serde_json::Value::String("wont_fix".into()),
        )])),
        &mut [&mut editor, &mut author],
    );
    assert!(
        !matches!(refused, Ok(InvocationUpdate::Succeeded { .. })),
        "a Commenter does not set status: {refused:?}"
    );
    for _ in 0..20 {
        step(&mut [&mut editor, &mut author]);
    }
    assert!(
        [&mut editor, &mut author].into_iter().all(|app| {
            !conversation_has(app, |doc| {
                chat::doc::ChatDoc::comment_report(doc, &report_id).is_some()
            })
        }),
        "the refused status reached nobody"
    );
    succeeded(
        invoke(
            &host,
            &team,
            &team_chat,
            &format!("chat.message:{report_id}"),
            "set-status",
            &InvokeArgument::Json(serde_json::Map::from_iter([
                (
                    "status".to_string(),
                    serde_json::Value::String("fixed".into()),
                ),
                ("generation".to_string(), serde_json::Value::from(121)),
            ])),
            &mut [&mut editor, &mut author],
        ),
        "setting the status",
    );
    until(
        &mut [&mut editor, &mut author],
        "the reporter to see the status",
        |apps| {
            conversation_has(apps[1], |doc| {
                chat::doc::ChatDoc::comment_report(doc, &report_id).is_some_and(|state| {
                    state.status == chat::feedback::ReportStatus::Fixed { generation: 121 }
                })
            })
        },
    );

    // The team takes the reporter's membership away; the reporter's Chat
    // hears it from the broker while it runs, and may no longer comment.
    let reporter_principal = reporter.principal();
    let revoked = tree
        .write()
        .desktop
        .workspaces
        .iter_mut()
        .find(|workspace| workspace.room.to_string() == ROOM.to_string())
        .expect("the room")
        .revoke_matching(|capability| {
            capability.subject == collab_types::Subject::person(reporter_principal.clone())
        });
    assert!(!revoked.is_empty(), "the reporter's membership was revoked");
    until(
        &mut [&mut editor, &mut author],
        "the reporter to hear of the revocation",
        |apps| !may(apps[1], ui::permissions::Verb::Comment),
    );
    assert!(
        may(&editor, ui::permissions::Verb::Write),
        "the team keeps what it holds"
    );
    drop(handle);
}
