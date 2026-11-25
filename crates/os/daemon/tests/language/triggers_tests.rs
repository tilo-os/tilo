//! A trigger fires a saved shortcut once workspace-wide when what fires it is a change to the
//! records of a built-in app: the device whose turn it is starts it and writes where it got to in
//! the workspace, and no device starts it again. The apps' packages are the language runtime's,
//! so these run beside the daemon that links one.

use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tilo_actions::remote::ActionCallError;
use tilo_automation::TriggerSpec;
use tilo_automation::dispatch::DocumentEvent;
use tilo_workspace_service::apps::triggers::{Runner, StartRun};
use tilo_workspace_service::work::Order;
use tilo_workspace_service::workspaces::fixtures::{
    ActsAsScout, Started, declare_app, on_issue_change, save_signed,
};

use super::{Device, Write, mount, write};

/// Let the installed app `app` write and change the records it keeps in `workspace`, as the
/// person whose device `cert` names grants it: what a workspace grants an app it runs, so its
/// origin's writes are its own rather than one person's.
pub(super) async fn let_app_write(
    workspaces: &tilo_workspace_service::workspaces::Workspaces,
    workspace: &str,
    app: &str,
    cert: &tilo_user_identity::DeviceCert,
) {
    let grant = tilo_action_policy::Grant::new(
        format!("{app}-keeps-its-records"),
        tilo_action_policy::Grantee::app(app),
        tilo_action_policy::TreePath::workspace(workspace),
        tilo_action_policy::KindSet::UNDOABLE,
        tilo_action_policy::Answer::Allow,
        tilo_action_policy::Grantee::person(hex::encode(cert.user_public_key)),
    );
    workspaces
        .put_workspace_grants(workspace, cert, [23; 32], vec![grant])
        .await
        .unwrap();
}

/// An app's write of a record a saved shortcut watches fires it once, on the device that wrote
/// it; looking again at the same record fires nothing more. A write whose resource the
/// trigger's filter does not match, by the fields the write left it with, fires nothing.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_write_fires_the_shortcut_watching_it_once() {
    use tilo_resource_store::wire::Command;
    use tilo_resource_store::{Cell, Resource, Stamp, Value};

    let device = Device::start("Tasks", Some("so.tilo.tasks")).await;
    let (key, cert) = (&device.key, &device.cert);
    let workspaces = device.workspaces();
    let workspace = device.workspace.clone();
    let mut shortcut = on_issue_change();
    let Some(TriggerSpec::ResourceChanged { filter, .. }) = shortcut.triggers.first_mut() else {
        panic!("the shortcut watches a resource");
    };
    *filter = serde_json::from_value(json!([{ "field": "item.title", "is": "First" }])).unwrap();
    save_signed(workspaces, &workspace, shortcut, (&key, &cert)).await;

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();
    for (id, title) in [("i-1", "First"), ("i-2", "Other")] {
        workspaces
            .own_resource_command(
                &workspace,
                "so.tilo.tasks",
                Command::Put {
                    kind: "tl.so.tilo.tasks.Task".into(),
                    id: id.into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: [("title".to_owned(), Cell::One(Value::Text(title.into())))].into(),
                    },
                    stamp: Stamp {
                        verb: "$create".into(),
                        invoker: Stamp::by(&tilo_action_policy::Actor::Member(
                            tilo_action_policy::MemberId::person(hex::encode(cert.user_public_key)),
                        )),
                        invocation: id.into(),
                        ..Stamp::default()
                    },
                },
                Vec::new(),
            )
            .await
            .unwrap();
        let mut written = Vec::new();
        while let Ok(write) = writes.try_recv() {
            written.push(write);
        }
        assert!(!written.is_empty(), "the write is told to the runner");
        runner.pass(written.clone()).await;
        runner.pass(written).await;
    }
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "tidy");
    device.shutdown().await;
}
/// A shortcut is armed by following its save: a record written right after the shortcut is saved
/// fires it, while the runner runs as a device runs it, with no wait for a clock.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_record_written_right_after_its_shortcut_is_saved_fires_it() {
    use tilo_resource_store::wire::Command;
    use tilo_resource_store::{Cell, Resource, Stamp, Value};

    let device = Device::start("Tasks", Some("so.tilo.tasks")).await;
    let (key, cert) = (&device.key, &device.cert);
    let workspaces = device.workspaces();
    let workspace = device.workspace.clone();

    // The runner runs before the shortcut exists, with nothing to arm.
    let started = Arc::new(Started::default());
    let running = tokio::spawn(
        Runner::new(
            workspaces.clone(),
            Arc::clone(&started) as Arc<dyn StartRun>,
            Order::WORKSPACE,
        )
        .run(),
    );
    tokio::task::yield_now().await;

    let begun = std::time::Instant::now();
    save_signed(workspaces, &workspace, on_issue_change(), (&key, &cert)).await;
    workspaces
        .own_resource_command(
            &workspace,
            "so.tilo.tasks",
            Command::Put {
                kind: "tl.so.tilo.tasks.Task".into(),
                id: "i-1".into(),
                resource: Resource {
                    readers: "*".into(),
                    parent: None,
                    order: None,
                    fields: [("title".to_owned(), Cell::One(Value::Text("First".into())))].into(),
                },
                stamp: Stamp {
                    verb: "$create".into(),
                    invoker: Stamp::by(&tilo_action_policy::Actor::Member(
                        tilo_action_policy::MemberId::person(hex::encode(cert.user_public_key)),
                    )),
                    invocation: "i-1".into(),
                    ..Stamp::default()
                },
            },
            Vec::new(),
        )
        .await
        .unwrap();

    let fired = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if !started.0.lock().unwrap().is_empty() {
                break;
            }
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
    })
    .await;
    running.abort();
    assert!(
        fired.is_ok(),
        "the write right after the save fires the shortcut"
    );
    let firings = started.0.lock().unwrap().clone();
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, "tidy");
    assert!(
        begun.elapsed() < Duration::from_secs(1),
        "armed by its save, not by a clock: {:?}",
        begun.elapsed()
    );
    device.shutdown().await;
}
/// A turn starts only on a change the agent may read: a message that was removed before the
/// turn began, and one a broker refuses the agent, are both refused, in words the person who
/// asked sees.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_turn_that_cannot_read_what_triggered_it_is_refused() {
    use tilo_resource_store::wire::{Command, Edit};
    use tilo_resource_store::{Cell, Resource, Stamp, Value};
    use tilo_workspace_service::agents::triggered::{MayRead, Where, unreadable};

    const CHAT: &str = "so.tilo.chat";
    const MESSAGE: &str = "tl.so.tilo.chat.Message";
    let device = Device::start("Chat", Some(CHAT)).await;
    let cert = &device.cert;
    let workspaces = device.workspaces();
    let workspace = device.workspace.clone();
    let person = hex::encode(cert.user_public_key);
    // The id Chat's declaration keeps for `send`: what a change its program makes claims.
    let send = tilolang_workspace::records::built_in(CHAT)
        .expect("the built-in packages compile")
        .expect("Chat is built in")
        .action_id("send")
        .expect("Chat offers send");
    let stamp = Stamp {
        verb: send,
        invoker: person.clone(),
        invocation: "i-1".into(),
        at: jiff::Timestamp::now().to_string(),
        ..Stamp::default()
    };
    workspaces
        .own_resource_command(
            &workspace,
            CHAT,
            Command::Batch(vec![
                Command::Put {
                    kind: "tl.so.tilo.chat.TextChannel".into(),
                    id: "general".into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: [("name".to_owned(), Cell::One(Value::Text("general".into())))]
                            .into(),
                    },
                    stamp: stamp.clone(),
                },
                Command::Put {
                    kind: MESSAGE.into(),
                    id: "m-1".into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: Some("general".into()),
                        order: None,
                        fields: BTreeMap::new(),
                    },
                    stamp: stamp.clone(),
                },
                // What the message says is its rich body, which a change writes as content.
                Command::Edit {
                    kind: MESSAGE.into(),
                    id: "m-1".into(),
                    edits: vec![Edit::Set {
                        field: "body".into(),
                        value: json!({"blocks": [{"text": "hello", "marks": []}]}),
                    }],
                    stamp: stamp.clone(),
                },
            ]),
            Vec::new(),
        )
        .await
        .unwrap();
    let event: DocumentEvent = serde_json::from_value(json!({
        "workspace": workspace,
        "record": "record",
        "app": CHAT,
        "kind": MESSAGE,
        "change": "c0ffee",
        "actor": {"by": {"member": {"person": person}}},
        "did": {"did": "verb", "action": {"app": CHAT, "kind": MESSAGE, "action": "send"}},
        "item": {"kind": MESSAGE, "id": "m-1", "label": "hello"},
        "after": {"body": "hello", "$in": {"$id": "general", "$kind": "Direct"}},
    }))
    .unwrap();
    let place = Where::of(&event).unwrap();

    struct Allows;
    impl MayRead for Allows {
        fn may_read_resource(
            &self,
            _caller: &tilo_actions::remote::Caller,
            _workspace: &str,
            _target: (&str, &str, &str),
        ) -> Result<(), ActionCallError> {
            Ok(())
        }
    }
    struct Denies;
    impl MayRead for Denies {
        fn may_read_resource(
            &self,
            _caller: &tilo_actions::remote::Caller,
            _workspace: &str,
            _target: (&str, &str, &str),
        ) -> Result<(), ActionCallError> {
            Err(ActionCallError::Refused {
                reason: "not yours".into(),
            })
        }
    }

    assert_eq!(
        unreadable(
            workspaces,
            Some(&Allows as &dyn MayRead),
            "scout",
            &person,
            &event,
            &place
        )
        .await,
        None,
        "an agent that may read what triggered it takes the turn"
    );
    let refused = unreadable(
        workspaces,
        Some(&Denies as &dyn MayRead),
        "scout",
        &person,
        &event,
        &place,
    )
    .await
    .expect("a message the agent may not read is refused");
    assert!(refused.contains("may not read"), "{refused}");

    workspaces
        .own_resource_command(
            &workspace,
            CHAT,
            Command::Remove {
                kind: MESSAGE.into(),
                id: "m-1".into(),
                stamp,
            },
            Vec::new(),
        )
        .await
        .unwrap();
    let refused = unreadable(
        workspaces,
        Some(&Allows as &dyn MayRead),
        "scout",
        &person,
        &event,
        &place,
    )
    .await
    .expect("a message removed before the turn began is refused");
    assert!(refused.contains("no longer"), "{refused}");
    device.shutdown().await;
}
/// An agent's triggers fire on a message that mentions it, one in its direct conversation and
/// one an outside author's provider carried in, and each starts one turn of the agent, placed
/// once workspace-wide; what the agent writes itself, its answer, fires none of them.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn an_agents_triggers_start_one_turn_per_message_and_never_on_its_own_answer() {
    use tilo_action_policy::{Actor as Who, MemberId};
    use tilo_workspace_service::agents::triggered::{self, Where};

    const CHAT: &str = "so.tilo.chat";
    const MESSAGE: &str = "tl.so.tilo.chat.Message";
    let device = Device::start("Chat", Some(CHAT)).await;
    let (key, cert) = (&device.key, &device.cert);
    let workspaces = device.workspaces();
    workspaces.run_agents(Arc::new(ActsAsScout));
    let workspace = device.workspace.clone();
    save_signed(
        workspaces,
        &workspace,
        triggered::triggers("scout", "Scout"),
        (&key, &cert),
    )
    .await;
    // The workspace's record of its agents holds Scout acting for its owner: a direct
    // conversation the owner's `people` names the agent in is the owner's agent's, and it reads
    // the conversation as the person it works for.
    let owner = hex::encode(cert.user_public_key);
    workspaces
        .author_agents(&workspace, |doc| {
            use tilo_workspace_agent::{OWNER, WorkspaceAgents, workspace_agents};
            let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                .in_app("so.tilo.model", "Model")
                .as_author(&owner)
                .in_app(OWNER, "Workspace");
            made.apply(
                WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    avatar: None,
                    name: tilo_resource::NonBlank::parse("Scout").unwrap(),
                    id: Some("scout".into()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            std::mem::swap(doc, made.doc_mut());
            Ok(())
        })
        .await
        .expect("the owner adds their agent");
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    let person = tilo_actions::Acting::person(hex::encode(cert.user_public_key)).by;
    let scout = Who::member(MemberId::agent("scout"));
    let bridge = Who::member(MemberId::bot("bridge"));
    // The id Chat's declaration keeps for `send`: what a change its program makes claims.
    let send = tilolang_workspace::records::built_in(CHAT)
        .expect("the built-in packages compile")
        .expect("Chat is built in")
        .action_id("send")
        .expect("Chat offers send");
    // `by` puts the resource `id` of `kind` in `parent`, with `fields`, the members of its
    // `sets` and the `content` its fields hold, a message's rich body among them.
    let put = async |kind: &str,
                     id: &str,
                     parent: Option<&str>,
                     by: &Who,
                     fields: Vec<(&str, &str)>,
                     sets: Vec<(&str, Vec<serde_json::Value>)>,
                     content: Vec<(&str, serde_json::Value)>| {
        write(
            workspaces,
            &workspace,
            &cert,
            CHAT,
            Write {
                kind,
                id,
                parent,
                verb: &send,
                by,
                // A direct conversation is read by the people it names, so its write names no
                // readers.
                readers: if kind == "tl.so.tilo.chat.Direct" {
                    ""
                } else {
                    "*"
                },
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name, value.to_owned()))
                    .collect(),
                sets,
                edits: content,
            },
        )
        .await;
    };
    // Rich text whose words are `words`, mentioning the agent where `mention` says.
    let rich = |words: &str, mention: bool| {
        let marks = if mention {
            json!([{"start": 0, "end": 5, "name": "mention", "value": {"Str": "{\"agent\":\"scout\"}"}}])
        } else {
            json!([])
        };
        json!({"blocks": [{"text": words, "marks": marks}]})
    };
    // One member of a conversation, as the app keeps it in `people`.
    let actor = |who: &Who| json!({"actor": serde_json::to_value(who).unwrap()});
    // One member of a set field: the actor, written into the index as its JSON text.
    let member = |who: &Who| serde_json::to_value(who).unwrap();
    // The `people` of a direct conversation, as a list field of the app is written.
    let people = |members: Vec<serde_json::Value>| json!({"json": {"list": members}});
    // A message's `body`.
    let body = |words: &str, mention: bool| vec![("body", rich(words, mention))];
    put(
        "tl.so.tilo.chat.TextChannel",
        "general",
        None,
        &person,
        vec![("name", "general")],
        Vec::new(),
        Vec::new(),
    )
    .await;
    put(
        "tl.so.tilo.chat.Direct",
        "with-scout",
        None,
        &person,
        vec![("name", "Scout")],
        vec![("people", vec![member(&person), member(&scout)])],
        vec![("people", people(vec![actor(&person), actor(&scout)]))],
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "making the conversations asks nothing: {now:?}"
    );

    // A member mentions the agent: one firing, carrying the message.
    put(
        MESSAGE,
        "m-1",
        Some("general"),
        &person,
        vec![],
        Vec::new(),
        body("Scout, look at this", true),
    )
    .await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, triggered::shortcut_id("scout"));
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    let place = Where::of(&event).unwrap();
    assert_eq!(
        (
            place.app.as_str(),
            place.kind.as_str(),
            place.record.as_str(),
            place.item.as_str()
        ),
        (CHAT, MESSAGE, "general", "m-1"),
        "the answer goes to the message, in its conversation"
    );
    assert_eq!(
        place.inputs(),
        json!({"app": CHAT, "kind": MESSAGE, "resource": "m-1"}),
        "every device that saw the change names the same turn"
    );
    assert_eq!(
        triggered::asker(&event).as_deref(),
        Some(hex::encode(cert.user_public_key).as_str())
    );
    assert!(
        triggered::said(&event).contains("look at this"),
        "{event:?}"
    );
    // One device takes the turn, once.
    let turn = triggered::started_here(workspaces, "scout", &place);
    let claim = tilo_workspace_service::work::takes_turn(workspaces, &workspace, &turn).await;
    assert!(claim.is_ok(), "this device takes the agent's turn");
    tilo_workspace_service::work::keep_done(
        workspaces,
        &workspace,
        &turn,
        json!({"answered": true}),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            tilo_workspace_service::work::takes_turn(
                workspaces,
                &workspace,
                &triggered::started_here(workspaces, "scout", &place)
            )
            .await,
            Err(tilo_workspace_service::work::NotTaken::Done(_))
        ),
        "a turn answered is taken by no device again"
    );

    // The agent answers in the conversation, naming itself: nothing fires on its own words.
    put(
        MESSAGE,
        "m-2",
        Some("general"),
        &scout,
        vec![],
        Vec::new(),
        body("Scout here: done", true),
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(now.is_empty(), "the agent's answer fires nothing: {now:?}");

    // In its direct conversation — it is one of the conversation's people — a member's message
    // asks it without naming it; its reply there asks nothing.
    put(
        MESSAGE,
        "d-1",
        Some("with-scout"),
        &person,
        vec![],
        Vec::new(),
        body("Can you help?", false),
    )
    .await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    put(
        MESSAGE,
        "d-2",
        Some("with-scout"),
        &scout,
        vec![],
        Vec::new(),
        body("Sure.", false),
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "its reply in the direct conversation fires nothing: {now:?}"
    );

    // A thread in its direct conversation is nested in the conversation: a reply there, naming
    // nobody, asks it too, and the answer is sent into the thread.
    put(
        "tl.so.tilo.chat.Thread",
        "thread-1",
        Some("with-scout"),
        &person,
        vec![("name", "Plans")],
        Vec::new(),
        Vec::new(),
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(now.is_empty(), "opening a thread asks nothing: {now:?}");
    put(
        MESSAGE,
        "d-3",
        Some("thread-1"),
        &person,
        vec![],
        Vec::new(),
        body("In the thread?", false),
    )
    .await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    let place = Where::of(&event).unwrap();
    assert_eq!(
        (place.record.as_str(), place.item.as_str()),
        ("thread-1", "d-3"),
        "a reply in a thread is a message in the thread"
    );

    // A message an outside author wrote, carried in by a provider, mentioning the agent: it
    // fires, and the turn is for the person whose trigger it is.
    put(
        MESSAGE,
        "m-3",
        Some("general"),
        &bridge,
        vec![],
        Vec::new(),
        body("Scout, a question from outside", true),
    )
    .await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    assert_eq!(triggered::asker(&event), None, "no member asked");
    assert_eq!(firings[0].saver, hex::encode(cert.user_public_key));

    // A message nobody mentions it in fires nothing.
    put(
        MESSAGE,
        "m-4",
        Some("general"),
        &person,
        vec![],
        Vec::new(),
        body("Lunch?", false),
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "a message that names nobody fires nothing: {now:?}"
    );
    device.shutdown().await;
}
/// An agent's new-mail trigger fires once on a message the workspace received — written by
/// Mail's outside origin — and starts one turn of the agent, placed once workspace-wide. The
/// same kind of record written here, by a member, is no message anyone received, and an
/// account's own copy of what it sent is no new mail: neither fires anything.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn an_agents_new_mail_trigger_starts_one_turn_on_mail_that_arrived() {
    use tilo_action_policy::Actor as Who;
    use tilo_app_id::AppId;
    use tilo_resource_store::Stamp;
    use tilo_workspace_service::agents::triggered::{self, Where};

    const MAIL: &str = "so.tilo.mail";
    const THREAD: &str = "tl.so.tilo.mail.Thread";
    const MESSAGE: &str = "tl.so.tilo.mail.Message";
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(tilo_workspace_service::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = tilo_workspace_service::workspaces::fixtures::governor();
    let host = tilo_workspace_service::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        keys,
    )
    .await;
    let workspaces = host.workspaces();
    mount(workspaces);
    workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    workspaces.run_agents(Arc::new(ActsAsScout));
    // Mail is a package the daemon does not run itself: a test that writes its records compiles
    // its sources and declares the surface they make.
    let mail_package = tilolang_workspace::records::Packages::compile(&[
        (
            concat!(
                include_str!("../../../../../apps/tl/contacts/app.tl"),
                "\n",
                include_str!("../../../../../apps/tl/contacts/identities.tl")
            ),
            include_str!("../../../../../apps/tl/contacts/app.ids"),
        ),
        (
            include_str!("../../../../../apps/tl/mail/app.tl"),
            include_str!("../../../../../apps/tl/mail/app.ids"),
        ),
    ])
    .expect("Mail's sources compile");
    declare_app(
        &host,
        MAIL,
        &mail_package
            .surface(MAIL)
            .expect("Mail declares its surface"),
    );
    let delivered = mail_package
        .action_id("deliver_message")
        .expect("Mail offers deliver_message");
    let workspace = workspaces.create("Mail").await.unwrap().workspace;
    assert!(
        workspaces.join_keys(&workspace).await,
        "the workspace's keys open what its records are kept in"
    );
    let_app_write(workspaces, &workspace, MAIL, &cert).await;
    save_signed(
        workspaces,
        &workspace,
        triggered::triggers("scout", "Scout"),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    let person = tilo_actions::Acting::person(hex::encode(cert.user_public_key)).by;
    // Mail's outside origin, as the store stamps what its servers give it.
    let mail = Who::App(AppId::new(MAIL));
    // `by` writes the resource `id` of `kind` in `parent` with `fields`, stamped with `verb`
    // and by `by`.
    let put = async |kind: &str,
                     id: &str,
                     parent: Option<&str>,
                     verb: &str,
                     by: &Who,
                     fields: Vec<(&str, &str)>| {
        write(
            workspaces,
            &workspace,
            &cert,
            MAIL,
            Write {
                kind,
                id,
                parent,
                verb,
                by,
                readers: "*",
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name, value.to_owned()))
                    .collect(),
                sets: Vec::new(),
                edits: Vec::new(),
            },
        )
        .await;
    };

    put(
        THREAD,
        "thread-1",
        None,
        Stamp::ORIGIN_VERB,
        &mail,
        Vec::new(),
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "a conversation arriving asks nothing: {now:?}"
    );

    // A message the workspace's mailbox received: one firing, and one turn to take.
    put(
        MESSAGE,
        "message-1",
        Some("thread-1"),
        Stamp::ORIGIN_VERB,
        &mail,
        vec![("mailbox", "inbox"), ("subject", "Hello")],
    )
    .await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, triggered::shortcut_id("scout"));
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    assert!(event.incoming_message, "{event:?}");
    let place = Where::of(&event).unwrap();
    assert_eq!(
        (
            place.app.as_str(),
            place.record.as_str(),
            place.item.as_str()
        ),
        (MAIL, "thread-1", "message-1"),
        "the answer goes to the conversation the message arrived in"
    );
    assert_eq!(triggered::asker(&event), None, "no member sent it");
    let turn = triggered::started_here(workspaces, "scout", &place);
    assert!(
        tilo_workspace_service::work::takes_turn(workspaces, &workspace, &turn)
            .await
            .is_ok(),
        "this device takes the agent's turn"
    );
    tilo_workspace_service::work::keep_done(
        workspaces,
        &workspace,
        &turn,
        json!({"answered": true}),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            tilo_workspace_service::work::takes_turn(
                workspaces,
                &workspace,
                &triggered::started_here(workspaces, "scout", &place)
            )
            .await,
            Err(tilo_workspace_service::work::NotTaken::Done(_))
        ),
        "a turn answered is taken by no device again"
    );

    // The same kind written here, by a member delivering a message, is no mail that arrived.
    put(
        MESSAGE,
        "message-2",
        Some("thread-1"),
        &delivered,
        &person,
        vec![("mailbox", "inbox")],
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "a message written here fires nothing: {now:?}"
    );

    // The account's own copy of what it sent, synced back from its server, is no new mail.
    put(
        MESSAGE,
        "message-3",
        Some("thread-1"),
        Stamp::ORIGIN_VERB,
        &mail,
        vec![("mailbox", "sent")],
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "a copy of what the account sent fires nothing: {now:?}"
    );
    host.shutdown().await;
}

/// An agent's own mail asks nothing of it: the message it sent, as its account's server keeps
/// it, and a copy of a message the workspace already holds are no mail that arrived. A message
/// the origin writes into the inbox is, and fires once.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn an_agents_new_mail_trigger_ignores_its_own_sent_mail_and_a_copy() {
    use tilo_action_policy::Actor as Who;
    use tilo_app_id::AppId;
    use tilo_resource_store::Stamp;
    use tilo_workspace_service::agents::triggered;

    const MAIL: &str = "so.tilo.mail";
    const THREAD: &str = "tl.so.tilo.mail.Thread";
    const MESSAGE: &str = "tl.so.tilo.mail.Message";
    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(tilo_workspace_service::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = tilo_workspace_service::workspaces::fixtures::governor();
    let host = tilo_workspace_service::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        keys,
    )
    .await;
    let workspaces = host.workspaces();
    mount(workspaces);
    workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    workspaces.run_agents(Arc::new(ActsAsScout));
    // Mail is a package the daemon does not run itself: a test that writes its records compiles
    // its sources and declares the surface they make.
    let mail_package = tilolang_workspace::records::Packages::compile(&[
        (
            concat!(
                include_str!("../../../../../apps/tl/contacts/app.tl"),
                "\n",
                include_str!("../../../../../apps/tl/contacts/identities.tl")
            ),
            include_str!("../../../../../apps/tl/contacts/app.ids"),
        ),
        (
            include_str!("../../../../../apps/tl/mail/app.tl"),
            include_str!("../../../../../apps/tl/mail/app.ids"),
        ),
    ])
    .expect("Mail's sources compile");
    declare_app(
        &host,
        MAIL,
        &mail_package
            .surface(MAIL)
            .expect("Mail declares its surface"),
    );
    let workspace = workspaces.create("Mail").await.unwrap().workspace;
    assert!(
        workspaces.join_keys(&workspace).await,
        "the workspace's keys open what its records are kept in"
    );
    let_app_write(workspaces, &workspace, MAIL, &cert).await;
    save_signed(
        workspaces,
        &workspace,
        triggered::triggers("scout", "Scout"),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    let mail = Who::App(AppId::new(MAIL));
    // `by` writes the resource `id` of `kind` in `parent` with `fields`, stamped with `verb`.
    let put = async |kind: &str,
                     id: &str,
                     parent: Option<&str>,
                     verb: &str,
                     by: &Who,
                     fields: Vec<(&str, &str)>| {
        write(
            workspaces,
            &workspace,
            &cert,
            MAIL,
            Write {
                kind,
                id,
                parent,
                verb,
                by,
                readers: "*",
                fields: fields
                    .into_iter()
                    .map(|(name, value)| (name, value.to_owned()))
                    .collect(),
                sets: Vec::new(),
                edits: Vec::new(),
            },
        )
        .await;
    };

    put(
        THREAD,
        "thread-1",
        None,
        Stamp::ORIGIN_VERB,
        &mail,
        Vec::new(),
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "a conversation arriving asks nothing: {now:?}"
    );

    // A message the workspace's mailbox received asks the agent once.
    put(
        MESSAGE,
        "arrived",
        Some("thread-1"),
        Stamp::ORIGIN_VERB,
        &mail,
        vec![
            ("mailbox", "inbox"),
            ("subject", "Hello"),
            ("from", "ada@example.org"),
            ("account", "my-mail"),
        ],
    )
    .await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    // What the message says is the trigger's input: who wrote it, what about, the account it
    // came to, and the conversation it is in.
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    let after = event.after.as_ref().expect("the message the change made");
    assert_eq!(after["subject"], "Hello");
    assert_eq!(after["from"], "ada@example.org");
    assert_eq!(after["account"], "my-mail");
    assert_eq!(after["$in"]["$id"], "thread-1");

    // A copy of a message the workspace already holds, its fields synced: no message arrived.
    put(
        MESSAGE,
        "arrived",
        Some("thread-1"),
        Stamp::ORIGIN_VERB,
        &mail,
        vec![("mailbox", "inbox"), ("subject", "Hello again")],
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "a copy of a message already held fires nothing: {now:?}"
    );

    // The agent's own message, as its account's server keeps it in the sent mailbox: no mail
    // that arrived.
    put(
        MESSAGE,
        "sent-1",
        Some("thread-1"),
        Stamp::ORIGIN_VERB,
        &mail,
        vec![("mailbox", "sent"), ("subject", "Re: Hello")],
    )
    .await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "the agent's own sent mail fires nothing: {now:?}"
    );
    host.shutdown().await;
}
/// A Moderator's chat watch fires once on a message a member posts, as the agent's one turn on
/// it, and never on what the agent writes back: its own removal or reply is no message that
/// fires again. The watch leaves out both the agent's own changes (`by`) and a copy of them
/// carried back from elsewhere (`origin`), as the agent's own triggers do.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[allow(
    clippy::too_many_lines,
    reason = "one scenario, end to end, reads as one test"
)]
async fn a_moderation_trigger_fires_once_on_a_members_message_and_not_on_the_agents_own_write() {
    use tilo_action_policy::{Actor as Who, MemberId};
    use tilo_resource_store::wire::{Command, Edit};
    use tilo_resource_store::{Resource, Stamp};
    use tilo_workspace_service::agents::triggered::{self, CHAT, MESSAGE, MODERATION, Where};

    let device = Device::start("Moderation", Some(CHAT)).await;
    let (key, cert) = (&device.key, &device.cert);
    let workspaces = device.workspaces();
    workspaces.run_agents(Arc::new(ActsAsScout));
    let workspace = device.workspace.clone();
    // The Moderator's one shortcut: the chat watch, not the agent's own triggers.
    save_signed(
        workspaces,
        &workspace,
        triggered::moderates("scout", "Scout"),
        (&key, &cert),
    )
    .await;
    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();

    let person = tilo_actions::Acting::person(hex::encode(cert.user_public_key)).by;
    let scout = Who::member(MemberId::agent("scout"));
    // The id Chat's declaration keeps for `send`: what a change its program makes claims.
    let send = tilolang_workspace::records::built_in(CHAT)
        .expect("the built-in packages compile")
        .expect("Chat is built in")
        .action_id("send")
        .expect("Chat offers send");
    // `put` writes the resource `id` of `kind` as `by`, as the app's write for an action that
    // was delivered to it.
    let put = async |kind: &str,
                     id: &str,
                     parent: Option<&str>,
                     by: &Who,
                     verb: &str,
                     fields: Vec<(&str, String)>| {
        write(
            workspaces,
            &workspace,
            &cert,
            CHAT,
            Write {
                kind,
                id,
                parent,
                verb,
                by,
                readers: "*",
                fields,
                sets: Vec::new(),
                edits: Vec::new(),
            },
        )
        .await;
    };
    // A member posts a message in the channel: what the watch fires on.
    let post = async |id: &str, by: &Who| {
        put(MESSAGE, id, Some("general"), by, &send, Vec::new()).await;
    };
    // A report filed against a message, on the workspace's moderation record. The record holds
    // its reports keyed by their ids, as the record's package declares them: the write makes the
    // record, in a workspace that has filed none yet, and fills its `reports`.
    let record = collab_types::workspace_document_id(MODERATION);
    let report = async |id: &str, by: &Who, reason: &str| {
        let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        let acted_for = matches!(by, Who::Member(MemberId::Agent(_)))
            .then(|| hex::encode(cert.user_public_key));
        let stamp = Stamp {
            verb: "report-message".into(),
            invoker: Stamp::by(by),
            acted_for,
            invocation: invocation.0.to_string(),
            at: jiff::Timestamp::now().to_string(),
            ..Stamp::default()
        };
        let reporter = by.member_id().expect("a report is a member's");
        let reporter = serde_json::to_string(reporter).unwrap();
        let mut reports = serde_json::Map::new();
        reports.insert(
            id.to_owned(),
            json!({
                "id": id,
                "reporter": reporter,
                "person": reporter,
                "app": CHAT,
                "record": "general",
                "item": "m-1",
                "reason": reason,
                "at_ms": 0,
                "decision": null,
            }),
        );
        let commands = Command::Batch(vec![
            Command::Put {
                kind: MODERATION.into(),
                id: record.clone(),
                resource: Resource {
                    readers: "*".into(),
                    parent: None,
                    order: None,
                    fields: std::collections::BTreeMap::new(),
                },
                stamp: stamp.clone(),
            },
            Command::Edit {
                kind: MODERATION.into(),
                id: record.clone(),
                edits: vec![Edit::Set {
                    field: "reports".into(),
                    value: json!({"reports": reports}),
                }],
                stamp,
            },
        ]);
        let caller = tilo_actions::remote::Caller::authenticated(
            cert.user_public_key,
            cert.device_public_key,
        )
        .in_workspace(&workspace);
        let context =
            tilo_workspace_service::apps::services::ResourceRequestContext::authenticated(
                CHAT, &workspace, caller,
            )
            .serving(vec![(invocation, by.clone())]);
        workspaces
            .resource_command(&context, commands)
            .await
            .unwrap();
    };

    // A member posts: one firing, and the turn it names is the agent's answer there.
    post("m-1", &person).await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(
        firings[0].trigger,
        triggered::moderates("scout", "Scout").triggers[0],
        "the watch's message trigger fired"
    );
    assert_eq!(firings[0].saver, hex::encode(cert.user_public_key));
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    assert_eq!(
        triggered::asker(&event),
        Some(hex::encode(cert.user_public_key)),
        "the turn answers whoever posted it"
    );
    let place = Where::of(&event).unwrap();
    assert_eq!(
        (
            place.app.as_str(),
            place.record.as_str(),
            place.item.as_str()
        ),
        (CHAT, "general", "m-1"),
        "the answer goes to the message, in its channel"
    );
    let turn = triggered::started_here(workspaces, "scout", &place);
    assert!(
        tilo_workspace_service::work::takes_turn(workspaces, &workspace, &turn)
            .await
            .is_ok(),
        "this device takes the agent's one turn"
    );
    tilo_workspace_service::work::keep_done(
        workspaces,
        &workspace,
        &turn,
        json!({"answered": true}),
    )
    .await
    .unwrap();
    assert!(
        matches!(
            tilo_workspace_service::work::takes_turn(
                workspaces,
                &workspace,
                &triggered::started_here(workspaces, "scout", &place)
            )
            .await,
            Err(tilo_workspace_service::work::NotTaken::Done(_))
        ),
        "a turn answered is taken by no device again"
    );

    // The agent writes the message again as its removal: its own change fires nothing.
    post("m-1", &scout).await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(
        now.is_empty(),
        "the agent's own write fires nothing: {now:?}"
    );

    // A message it writes itself, as its warning reply, fires nothing either.
    post("m-2", &scout).await;
    let now = super::fired(&mut runner, &mut writes, &started).await;
    assert!(now.is_empty(), "the agent's reply fires nothing: {now:?}");

    // A report filed against a message is the watch's other place: one firing, answered on the
    // moderation record, the same report the agent reads.
    report("report-1", &person, "spam").await;
    let firings = super::fired(&mut runner, &mut writes, &started).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(
        firings[0].trigger,
        triggered::moderates("scout", "Scout").triggers[1],
        "the watch's report trigger fired"
    );
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    let place = Where::of(&event).expect("a report is a place the agent answers");
    assert_eq!(place.app, MODERATION, "answered on the record itself");
    assert_eq!(
        triggered::asker(&event),
        Some(hex::encode(cert.user_public_key)),
        "the turn answers whoever filed it"
    );

    // Turning the watch off is removing its shortcut: nothing fires any more.
    workspaces
        .change_own_shortcuts(
            &workspace,
            tilo_automation::ShortcutChange::Delete(triggered::moderation_shortcut_id("scout")),
        )
        .await
        .unwrap();
    let started = Arc::new(Started::default());
    let mut again = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    again.arm().await;
    post("m-3", &person).await;
    let mut since = Vec::new();
    while let Ok(write) = writes.try_recv() {
        since.push(write);
    }
    again.pass(since).await;
    assert!(
        started.0.lock().unwrap().is_empty(),
        "a watch turned off fires nothing"
    );
    device.shutdown().await;
}

/// An agent's record wears a persona — a name and a picture — and a member mentions it in a
/// conversation: the mention starts one turn for that agent, naming the message it answers, and
/// the record the author is drawn from keeps the name and the picture a face reads.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_mention_of_an_agent_with_a_persona_is_answered_under_it() {
    use tilo_action_policy::{Actor as Who, MemberId};
    use tilo_resource_store::wire::{Command, Edit};
    use tilo_resource_store::{Cell, Resource, Stamp, Value};
    use tilo_workspace_agent::{WorkspaceAgents, workspace_agents};
    use tilo_workspace_service::agents::triggered::{self, Where};

    const CHAT: &str = "so.tilo.chat";
    const MESSAGE: &str = "tl.so.tilo.chat.Message";
    /// A picture a persona may carry: a 1-by-1 PNG.
    const PICTURE: &str = "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAMAAAAoyzS7AAAAA1BMVEX///+nxBvIAAAAAXRSTlMAQObYZgAAAAxJREFUeNoBgAEAAP//AwABWgH+AAAAAElFTkSuQmCC";

    let dir = tempfile::tempdir().unwrap();
    let keys = Arc::new(tilo_workspace_service::workspaces::fixtures::TestGroupKeys::default());
    let (key, cert) = tilo_workspace_service::workspaces::fixtures::governor();
    let host = tilo_workspace_service::workspaces::fixtures::test_host_holding(
        dir.path(),
        &key,
        &cert,
        keys,
    )
    .await;
    let workspaces = host.workspaces();
    workspaces.set_member_device(Arc::new(tilo_workspace_service::workspaces::MemberDevice {
        cert: cert.clone(),
        signer: Arc::new(key.clone()),
    }));
    // The language the daemon links, mounted as its program mounts it: Chat's types and verbs
    // are what the store judges the conversation's writes by.
    mount(workspaces);
    workspaces.run_agents(Arc::new(ActsAsScout));
    let workspace = workspaces.create("Chat").await.unwrap().workspace;
    let route = super::record_route(&workspace, CHAT);
    let _ = workspaces.own_route_keys(&route, true).await.unwrap();
    save_signed(
        workspaces,
        &workspace,
        triggered::triggers("scout", "Scout"),
        (&key, &cert),
    )
    .await;

    // The record makes the agent whose words a reply carries: its name and its picture, made
    // through the record's own action.
    let person = hex::encode(cert.user_public_key);
    workspaces
        .author_agents(&workspace, |doc| {
            let mut made = tilo_resource::Replica::<WorkspaceAgents>::from_doc(std::mem::take(doc))
                .as_author(&person)
                .in_app(tilo_workspace_agent::OWNER, "Workspace");
            made.apply(
                WorkspaceAgents::CREATE_AGENT.with(workspace_agents::CreateAgent {
                    avatar: Some(PICTURE.to_owned()),
                    name: tilo_resource::NonBlank::parse("Scout").unwrap(),
                    id: Some("scout".into()),
                    model: None,
                    mcp_client: None,
                }),
            )
            .map_err(|error| error.to_string())?;
            std::mem::swap(doc, made.doc_mut());
            Ok(())
        })
        .await
        .unwrap();

    let started = Arc::new(Started::default());
    let mut runner = Runner::new(
        workspaces.clone(),
        Arc::clone(&started) as Arc<dyn StartRun>,
        Order::WORKSPACE,
    );
    runner.arm().await;
    let mut writes = workspaces.document_writes();
    let caller =
        tilo_actions::remote::Caller::authenticated(cert.user_public_key, cert.device_public_key)
            .in_workspace(&workspace);
    // `by` writes the message `id` in `parent`, mentioning the agent: the app's own write for an
    // action that was delivered to it.
    let send = async |id: &str, parent: &str, by: &Who| {
        let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        let stamp = Stamp {
            verb: "$create".into(),
            invoker: Stamp::by(by),
            invocation: invocation.0.to_string(),
            at: jiff::Timestamp::now().to_string(),
            ..Stamp::default()
        };
        let commands = vec![
            Command::Put {
                kind: MESSAGE.into(),
                id: id.into(),
                resource: Resource {
                    readers: "*".into(),
                    parent: Some(parent.to_owned()),
                    order: None,
                    fields: std::collections::BTreeMap::new(),
                },
                stamp: stamp.clone(),
            },
            Command::Edit {
                kind: MESSAGE.into(),
                id: id.into(),
                edits: vec![Edit::Set {
                    field: "body".into(),
                    value: json!({"blocks": [{"text": "Scout, look at this", "marks": [
                        {"start": 0, "end": 5, "name": "mention", "value": {"Str": "{\"agent\":\"scout\"}"}}
                    ]}]}),
                }],
                stamp,
            },
        ];
        let context =
            tilo_workspace_service::apps::services::ResourceRequestContext::authenticated(
                CHAT,
                &workspace,
                caller.clone(),
            )
            .serving(vec![(invocation, by.clone())]);
        workspaces
            .resource_command(&context, Command::Batch(commands))
            .await
            .unwrap();
    };
    let put_channel = async {
        let invocation = tilo_actions::InvocationId(uuid::Uuid::new_v4());
        let stamp = Stamp {
            verb: "$create".into(),
            invoker: Stamp::by(&Who::member(MemberId::person(&person))),
            invocation: invocation.0.to_string(),
            at: jiff::Timestamp::now().to_string(),
            ..Stamp::default()
        };
        let context =
            tilo_workspace_service::apps::services::ResourceRequestContext::authenticated(
                CHAT,
                &workspace,
                caller.clone(),
            )
            .serving(vec![(invocation, Who::member(MemberId::person(&person)))]);
        workspaces
            .resource_command(
                &context,
                Command::Put {
                    kind: "tl.so.tilo.chat.TextChannel".into(),
                    id: "general".into(),
                    resource: Resource {
                        readers: "*".into(),
                        parent: None,
                        order: None,
                        fields: [("name".to_owned(), Cell::One(Value::Text("general".into())))]
                            .into_iter()
                            .collect(),
                    },
                    stamp,
                },
            )
            .await
            .unwrap();
    };
    put_channel.await;
    // What the runner fires on the records written since it last looked.
    let mut fired = async |runner: &mut Runner| {
        let mut since = Vec::new();
        while let Ok(write) = writes.try_recv() {
            since.push(write);
        }
        let before = started.0.lock().unwrap().len();
        runner.pass(since).await;
        started.0.lock().unwrap()[before..].to_vec()
    };
    let now = fired(&mut runner).await;
    assert!(
        now.is_empty(),
        "opening a conversation asks nothing: {now:?}"
    );

    // A member mentions Scout: one turn for the agent, naming the message it answers.
    send("m-1", "general", &Who::member(MemberId::person(&person))).await;
    let firings = fired(&mut runner).await;
    assert_eq!(firings.len(), 1, "{firings:?}");
    assert_eq!(firings[0].shortcut, triggered::shortcut_id("scout"));
    let event: DocumentEvent =
        serde_json::from_value(firings[0].inputs[tilo_automation::EVENT_INPUT].clone()).unwrap();
    let place = Where::of(&event).unwrap();
    assert_eq!(
        (place.record.as_str(), place.item.as_str()),
        ("general", "m-1"),
        "the answer goes to the message, in its conversation"
    );

    // The persona the answer is drawn under is the record's: the name and the picture, as the
    // one value a face reads.
    let record = collab_types::workspace_document_id(tilo_workspace_agent::KIND);
    let kept = workspaces
        .read_resource::<WorkspaceAgents>(
            &workspace,
            (tilo_workspace_agent::OWNER, record.as_str()),
        )
        .await
        .unwrap()
        .expect("the record is kept");
    let scout = kept
        .agents
        .iter()
        .find(|agent| agent.id.0 == "scout")
        .expect("the record keeps the agent");
    assert_eq!(scout.name, "Scout");
    assert_eq!(scout.avatar.as_deref(), Some(PICTURE));
    assert!(
        collab_types::avatar_of(PICTURE).is_some(),
        "the picture is one a face draws"
    );
    host.shutdown().await;
}
