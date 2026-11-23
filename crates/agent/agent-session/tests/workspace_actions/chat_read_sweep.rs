//! Every other way an agent comes to what Chat keeps, each asked by a test of its own: a
//! thread, a view, what a message carries, a read mark, a kept summary, a mention, the list of
//! conversations with the rows of a listing, the feed of the workspace's changes, a question
//! that waits, an action that takes a message's or a conversation's id as an argument, and a
//! question about an action on a kept message. Each is
//! asked by the agent of a viewer, of an editor and of the owner, on a turn and outside one, and
//! held to the one rule: what is said in a direct conversation is given to an agent only where
//! the person it acts for is one of the people it is between, and an agent whose person is one
//! of them is given it as before.
use super::*;

/// What is written last, in the open chat: a reader that holds it read all written before it.
const LAST: &str = "The last word here is wren.";
/// Who each reader is, in the order the readers stand in.
const AGENTS: usize = 6;

/// A workspace of Ada's with three things said in it, and who reads it.
struct Swept {
    workspace: String,
    page: Browser,
    /// Bo, who reads and nothing else, and Cy, who edits: each one's root key and device.
    bo: (SigningKey, crate::support::Device),
    cy: (SigningKey, crate::support::Device),
    /// For a viewer, an editor and the owner, their agent on a turn and outside one.
    readers: Vec<(&'static str, Caller)>,
    /// Ada's conversation with her agent, Cy's with the same agent, the open chat.
    places: [Id; 3],
    /// What was said in each: Ada's sentence, Cy's, the open chat's.
    said: [Id; 3],
}

/// Whether the person reader `reader` acts for takes part in conversation `conversation`:
/// Ada's with her agent (0), Cy's with it (1), the open chat (2).
fn takes_part(reader: usize, conversation: usize) -> bool {
    match conversation {
        0 => reader >= 4,
        1 => reader == 2 || reader == 3,
        _ => true,
    }
}

/// Seconds since the Unix epoch, now.
fn now_s() -> i64 {
    i64::try_from(tilo_clock::unix_now_ns() / 1_000_000_000).unwrap()
}

/// As [`three_things_said`], with the people's keys kept: a later step writes as one of them.
async fn swept(served: &mut Served) -> Swept {
    let (workspace, page, hers) = served.workspace(|_| Vec::new()).await;
    let person = |name: &str| {
        let (root, _) = tilo_user_identity::UserIdentity::generate(name, 1);
        let device = enrol(&root, "browser", DeviceCapabilities::member());
        (root, device)
    };
    let (bo, cy) = (person("Bo"), person("Cy"));
    admit_as(served, &workspace, &bo.1, Role::Viewer).await;
    admit_as(served, &workspace, &cy.1, Role::Editor).await;
    let now = now_s();
    let with_agent = conversation_with(&cy.0, SCOUT);
    let his = served
        .said_at(&workspace, &cy.0, &with_agent, (CYS, now - 120))
        .await;
    let open = open_chat(served, &workspace).await;
    let everyones = served
        .said_at(&workspace, &served.ada, &open, (OPEN, now - 60))
        .await;
    let asked = Id::from_raw("m_asked_elsewhere".to_owned());
    let elsewhere = |who: &(SigningKey, crate::support::Device)| {
        acting_for(
            &Served::person(&who.0, &who.1, &workspace),
            KEPT_ELSEWHERE,
            (&conversation_with(&who.0, KEPT_ELSEWHERE), &asked),
        )
    };
    let [bo_turn, bo_outside] = elsewhere(&bo);
    let [cy_turn, cy_outside] = elsewhere(&cy);
    let [ada_turn, ada_outside] = acting_for(
        &Served::person(&served.ada, &served.devices.1, &workspace),
        SCOUT,
        (&conversation_with(&served.ada, SCOUT), &hers),
    );
    let readers = vec![
        ("a viewer's agent on a turn", bo_turn),
        ("a viewer's agent outside a turn", bo_outside),
        ("an editor's agent on a turn", cy_turn),
        ("an editor's agent outside a turn", cy_outside),
        ("the owner's agent on a turn", ada_turn),
        ("the owner's agent outside a turn", ada_outside),
    ];
    assert_eq!(readers.len(), AGENTS);
    let places = [conversation_with(&served.ada, SCOUT), with_agent, open];
    Swept {
        workspace,
        page,
        bo,
        cy,
        readers,
        places,
        said: [hers, his, everyones],
    }
}

impl Swept {
    /// Chat's own document in the workspace.
    fn chat(&self) -> DocumentRoute {
        DocumentRoute {
            app_id: CHAT.into(),
            cluster_id: self.workspace.clone(),
            document: String::new(),
        }
    }

    /// Write [`LAST`] in the open chat and have every reader read it: what each then reads of
    /// Chat's messages, read from everything written before.
    async fn all_read(&self, served: &Served) -> Vec<String> {
        served
            .said_at(
                &self.workspace,
                &served.ada,
                &self.places[2],
                (LAST, now_s()),
            )
            .await;
        let mut reads = Vec::new();
        for (_, caller) in &self.readers {
            reads.push(messages_read(served, &self.workspace, caller, LAST).await);
        }
        reads
    }
}

/// What an action of Chat's takes to name the conversation `place`, with `more` beside it.
fn in_place(place: &str, more: serde_json::Value) -> serde_json::Value {
    let mut named = saying(&Id::from_raw(place.to_owned()), "");
    let (Some(fields), serde_json::Value::Object(more)) = (named.as_object_mut(), more) else {
        panic!("an action takes an object of arguments");
    };
    fields.remove("text");
    fields.extend(more);
    named
}

/// The resource Chat's messages are actions of.
fn messages() -> String {
    ui::resource::chain_resource_id(&chat::parts::chain())
}

/// `text` as it stands inside a JSON string.
fn escaped(text: &str) -> String {
    let quoted = serde_json::to_string(text).unwrap();
    quoted[1..quoted.len() - 1].to_owned()
}

/// Whether `answer` names `marker`: as it is, or as a JSON string inside one or two others.
fn names(answer: &str, marker: &str) -> bool {
    let once = escaped(marker);
    answer.contains(marker) || answer.contains(&once) || answer.contains(&escaped(&once))
}

/// The first of `answer`, enough to say what it was.
fn cut(answer: &str) -> String {
    let short: String = answer.chars().take(1500).collect();
    short.replace('\n', " / ")
}

/// What the readers were answered, held to the one rule as each answer comes.
#[derive(Default)]
struct Judged {
    /// Where an answer broke the rule, a line each.
    faults: Vec<String>,
    /// What each was answered, cut short.
    said: Vec<String>,
}

impl Judged {
    /// `answer` is what `probe` gave the reader `reader`. Each of `markers` is something of the
    /// conversation it stands with: none may be in an answer to a reader whose person takes no
    /// part in that conversation, and with `own` each is in the answer to one whose person does.
    fn answer(
        &mut self,
        (reader, who): (usize, &str),
        probe: &str,
        answer: &str,
        (markers, own): (&[(usize, &str)], bool),
    ) {
        for (conversation, marker) in markers {
            let (given, part) = (names(answer, marker), takes_part(reader, *conversation));
            if given && !part {
                self.faults.push(format!(
                    "GIVEN: {who}, {probe}: holds {marker:?} of a conversation its person takes no part in"
                ));
            }
            if own && part && !given {
                self.faults.push(format!(
                    "NOT GIVEN: {who}, {probe}: lacks {marker:?} of a conversation its person takes part in"
                ));
            }
        }
        self.said.push(format!("{who} | {probe} | {}", cut(answer)));
    }

    /// What `probe` answered `who` for something kept from it and for something that does not
    /// exist: the two must be one answer, so that nothing tells them apart.
    fn alike(&mut self, who: &str, probe: &str, kept: &str, none: &str) {
        if kept != none {
            self.faults.push(format!(
                "TOLD APART: {who}, {probe}: what is kept answers {}; what does not exist answers {}",
                cut(kept),
                cut(none)
            ));
        }
    }

    /// End the test: nothing broke the rule.
    fn settle(self) {
        assert!(
            self.faults.is_empty(),
            "{} answers broke the rule:\n{}\nWHAT EACH WAS ANSWERED:\n{}",
            self.faults.len(),
            self.faults.join("\n"),
            self.said.join("\n")
        );
    }
}

/// What the agent tool `name` answers `caller` for `arguments`, with whether it failed.
async fn tool(
    served: &Served,
    swept: &Swept,
    caller: &Caller,
    call: (&str, serde_json::Value),
) -> String {
    let (text, failed) = served.tool(&swept.workspace, caller, call.0, call.1).await;
    if failed {
        format!("FAILED: {text}")
    } else {
        text
    }
}

/// What Chat's `action` on `resource` answers `caller` for `arguments`, through the service.
async fn tried(
    served: &Served,
    swept: &Swept,
    caller: &Caller,
    (resource, action, arguments): (&str, &str, serde_json::Value),
) -> String {
    let service = service!(served, &swept.workspace);
    let arguments = arguments.as_object().expect("an object").clone();
    let answered =
        turns::invoke_named(&service, caller, &swept.chat(), resource, action, arguments).await;
    format!("{answered:?}")
}

/// [`tried`], for a step a test stands on: it has to be done.
async fn did(
    served: &Served,
    swept: &Swept,
    caller: &Caller,
    call: (&str, &str, serde_json::Value),
) {
    let (resource, action) = (call.0.to_owned(), call.1.to_owned());
    let answered = tried(served, swept, caller, call).await;
    assert!(
        answered.starts_with("Ok("),
        "{action} on {resource:?} is done: {answered}"
    );
}

/// The message whose words hold `word`, once the server holds it.
async fn held_message(served: &Served, workspace: &str, word: &str) -> Id {
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let part = conversation_part(&served.workspaces, workspace).await;
        if let Some((id, _)) = part
            .messages
            .iter()
            .find(|(_, held)| held.text.contains(word))
        {
            return id.clone();
        }
        assert!(Instant::now() < deadline, "the server never holds {word:?}");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// As [`Served::said_at`], in the thread under `parent`.
async fn replied_at(
    served: &Served,
    workspace: &str,
    root: &SigningKey,
    (conversation, parent): (&Id, &Id),
    (text, when): (&str, i64),
) -> Id {
    let governor = served.workspaces.device_cert().clone();
    let held = served
        .workspaces
        .resource_document(workspace, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    let chat = chat::Chat::read(&automerge::AutoCommit::load(&held).unwrap()).unwrap();
    let person = person_of(root);
    served
        .workspaces
        .author_application(
            workspace,
            &governor,
            &crate::common::catalogue(),
            CHAT,
            chat::parts::part(0).as_str(),
            |part| {
                chat::demo::apply_part_doc_as(
                    part,
                    &chat,
                    |replica| replica.as_invoker(&person),
                    Some(when * 1000),
                    chat::demo::post(conversation.as_str(), text, Some(parent.as_str())),
                )
                .map_err(|error| error.to_string())
            },
        )
        .await
        .unwrap();
    held_message(served, workspace, text).await
}

/// Chat's own document as the server holds it now.
async fn chat_held(served: &Served, workspace: &str) -> chat::Chat {
    let held = served
        .workspaces
        .resource_document(workspace, CHAT, "")
        .await
        .unwrap()
        .unwrap();
    chat::Chat::read(&automerge::AutoCommit::load(&held).unwrap()).unwrap()
}

/// `address` with `id` in the place of the item it names.
fn beside(address: &str, id: &str) -> String {
    let (within, _) = address.rsplit_once('/').expect("an item's address");
    format!("{within}/{id}")
}

/// `address` with an id no item has in the place of the item it names.
fn nobodys(address: &str) -> String {
    let (_, id) = address.rsplit_once('/').expect("an item's address");
    beside(address, &"0".repeat(id.len()))
}

/// The addresses the three sentences are read at, each from a reader whose person takes part.
fn addresses(reads: &[String]) -> [String; 3] {
    [(4, WORDS[0]), (2, WORDS[1]), (0, WORDS[2])]
        .map(|(reader, word)| address_in(&reads[reader], word).expect("its own agent reads it"))
}

/// A thread: what is answered under a message of a direct conversation is given with that
/// conversation and no further. The list of messages, the thread asked for by its first
/// message, and an answer in it asked for by its own id.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_thread_gives_an_agent_nothing_of_a_direct_conversation_its_person_takes_no_part_in() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let now = now_s();
    let under = [
        (&served.ada, "It is kept behind the egret.", "egret"),
        (&swept.cy.0, "The spare is under the osprey.", "osprey"),
        (&served.ada, "We meet by the gazebo.", "gazebo"),
    ];
    let mut answers = Vec::new();
    for (at, (root, text, _)) in under.iter().enumerate() {
        let thread = (&swept.places[at], &swept.said[at]);
        answers.push(replied_at(&served, &swept.workspace, root, thread, (text, now - 40)).await);
    }
    let reads = swept.all_read(&served).await;
    let first = addresses(&reads);
    let words: Vec<(usize, &str)> = under.iter().map(|(_, _, word)| *word).enumerate().collect();
    let service = service!(served, &swept.workspace);
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let at = (reader, *who);
        judged.answer(at, "the list of messages", &reads[reader], (&words, true));
        for (conversation, parent) in first.iter().enumerate() {
            let one = [words[conversation]];
            let path = serde_json::json!({"path": parent});
            let read = tool(&served, &swept, caller, ("tilo_read", path.clone())).await;
            let listed = tool(&served, &swept, caller, ("tilo_ls", path)).await;
            let thread = format!("{read}\n{listed}");
            judged.answer(
                at,
                "the thread by its first message",
                &thread,
                (&one, false),
            );
            let answer = beside(parent, answers[conversation].as_str());
            let (_, by_id) = read_by_id(&service, &swept.chat(), caller, &answer).await;
            judged.answer(
                at,
                "an answer in the thread by its id",
                &by_id,
                (&one, true),
            );
            if !takes_part(reader, conversation) {
                let (_, none) =
                    read_by_id(&service, &swept.chat(), caller, &nobodys(&answer)).await;
                judged.alike(who, "an answer in the thread by its id", &by_id, &none);
            }
        }
    }
    judged.settle();
    swept.page.peer.close();
}

/// A view: what a person's window shows of Chat, of its messages and of one message, as the
/// tool that reads a window answers it, and the outline the window is read from.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_view_shows_an_agent_nothing_of_a_direct_conversation_its_person_takes_no_part_in() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let reads = swept.all_read(&served).await;
    let at = addresses(&reads);
    let words = [(0, WORDS[0]), (1, WORDS[1]), (2, WORDS[2])];
    // Each view, and whether it shows what is said in the open chat: the window does, to
    // every reader; one message has no view of its own.
    let views = [
        ("the view of Chat", "chat".to_owned(), true),
        ("the view of its messages", "chat/Messages".to_owned(), true),
        ("the view of Ada's message", at[0].clone(), false),
        ("the view of Cy's message", at[1].clone(), false),
        ("the view of the open chat's message", at[2].clone(), false),
    ];
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        for (probe, path, shows) in &views {
            let asked = serde_json::json!({"path": path});
            let view = tool(&served, &swept, caller, ("tilo_view", asked)).await;
            judged.answer((reader, who), probe, &view, (&words[..2], false));
            judged.answer((reader, who), probe, &view, (&words[2..], *shows));
        }
        for (probe, resource) in [
            ("Chat's outline", String::new()),
            ("its messages' outline", messages()),
        ] {
            let call = (resource.as_str(), "outline", serde_json::json!({}));
            let outline = tried(&served, &swept, caller, call).await;
            judged.answer((reader, who), probe, &outline, (&words, false));
        }
    }
    judged.settle();
    swept.page.peer.close();
}

/// What a message carries: its words as they were before a change of them, whom it mentions,
/// what it answers, what is attached to it, its question and its view. Asked as fields of the
/// list, and each by the address it has under the message.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn what_a_message_carries_is_given_with_the_message_and_no_further() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let ada = Served::person(&served.ada, &served.devices.1, &swept.workspace);
    let old = serde_json::json!({"text": "The old word was dunlin."});
    let old = in_place(swept.places[0].as_str(), old);
    did(&served, &swept, &ada, (&messages(), "post", old)).await;
    let changed = held_message(&served, &swept.workspace, "dunlin").await;
    let change =
        serde_json::json!({"message": changed.as_str(), "text": "The new word is godwit."});
    did(&served, &swept, &ada, (&messages(), "edit-message", change)).await;
    held_message(&served, &swept.workspace, "godwit").await;
    let reads = swept.all_read(&served).await;
    let message = address_in(&reads[4], "godwit").expect("her own agent reads the changed message");
    let now = [(0, WORDS[0]), (1, WORDS[1]), (2, WORDS[2]), (0, "godwit")];
    let before = [(0, "dunlin")];
    let fields = serde_json::json!({"fields": ["text", "history", "mentions", "parent", "quoted",
        "attachments", "question", "view", "reactions"], "pageSize": 200});
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let at = (reader, *who);
        judged.answer(at, "the list of messages", &reads[reader], (&now, true));
        let listed = tried(
            &served,
            &swept,
            caller,
            (&messages(), "read", fields.clone()),
        )
        .await;
        judged.answer(at, "the list, asked for fields", &listed, (&now, true));
        judged.answer(at, "the list, asked for fields", &listed, (&before, true));
        for under in [
            "",
            "/text",
            "/history",
            "/mentions",
            "/attachments",
            "/question",
            "/view",
            "/work",
            "/reactions",
            "/quoted",
            "/body",
        ] {
            let probe = format!("the message by its id{under}");
            let nothing = serde_json::json!({});
            let call = (format!("{message}{under}"), nothing.clone());
            let kept = tried(&served, &swept, caller, (&call.0, "read", call.1)).await;
            // The message itself is read by its id; what it carries has no address of its own.
            let words = [(0, "godwit"), (0, "dunlin")];
            judged.answer(at, &probe, &kept, (&words, under.is_empty()));
            if !takes_part(reader, 0) {
                let none = format!("{}{under}", nobodys(&message));
                let none = tried(&served, &swept, caller, (&none, "read", nothing)).await;
                let hidden = |answer: &str, address: &str| {
                    let (_, id) = address.rsplit_once('/').expect("an item's address");
                    answer.replace(id, "ID")
                };
                judged.alike(
                    who,
                    &probe,
                    &hidden(&kept, &message),
                    &hidden(&none, &nobodys(&message)),
                );
            }
        }
    }
    judged.settle();
    swept.page.peer.close();
}

/// What Chat's own resource is read as by each reader: as text, as a listing, and by a read of
/// it.
async fn chat_as_read(
    served: &Served,
    swept: &Swept,
    caller: &Caller,
) -> Vec<(&'static str, String)> {
    let path = serde_json::json!({"path": "chat"});
    vec![
        (
            "Chat, read as text",
            tool(served, swept, caller, ("tilo_read", path.clone())).await,
        ),
        (
            "Chat, listed",
            tool(served, swept, caller, ("tilo_ls", path)).await,
        ),
        (
            "Chat's own resource, read",
            tried(served, swept, caller, ("", "read", serde_json::json!({}))).await,
        ),
    ]
}

/// Read marks: how far a person read a direct conversation says that it is there and how much
/// was said in it. Ada marks hers and Cy his; nobody else's agent is told of either mark.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_read_mark_tells_an_agent_nothing_of_a_direct_conversation_its_person_takes_no_part_in() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let ada = Served::person(&served.ada, &served.devices.1, &swept.workspace);
    let cy = Served::person(&swept.cy.0, &swept.cy.1, &swept.workspace);
    for (person, place) in [(&ada, &swept.places[0]), (&cy, &swept.places[1])] {
        let mark = in_place(place.as_str(), serde_json::json!({"part": 0, "seen": 1}));
        did(&served, &swept, person, ("", "mark-read", mark)).await;
    }
    let deadline = tilo_test_env::hang_deadline();
    loop {
        let chat = chat_held(&served, &swept.workspace).await;
        let marked = |place: &Id| {
            chat.read
                .values()
                .any(|marks| marks.contains_key(place.as_str()))
        };
        if marked(&swept.places[0]) && marked(&swept.places[1]) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the server never holds both marks: {:?}",
            chat.read
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    swept.all_read(&served).await;
    let marks = [(0, swept.places[0].as_str()), (1, swept.places[1].as_str())];
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        for (probe, answer) in chat_as_read(&served, &swept, caller).await {
            judged.answer((reader, who), probe, &answer, (&marks, false));
        }
    }
    judged.settle();
    swept.page.peer.close();
}

/// A kept summary: what an agent keeps of a conversation it answers in, in its own words, is
/// that conversation's. Ada's agent keeps one of hers; nobody else's agent reads it.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_kept_summary_is_given_with_its_conversation_and_no_further() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let summary = serde_json::json!({
        "through": swept.said[0].as_str(),
        "through_ts": now_s() - 200,
        "text": "Friday's word is a wading bird, the bittern.",
        "conversation": swept.places[0].as_str(),
    });
    did(
        &served,
        &swept,
        &swept.readers[4].1,
        ("", "keep-summary", summary),
    )
    .await;
    let deadline = tilo_test_env::hang_deadline();
    while !chat_held(&served, &swept.workspace)
        .await
        .summaries
        .contains_key(swept.places[0].as_str())
    {
        assert!(
            Instant::now() < deadline,
            "the server never holds the summary"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    swept.all_read(&served).await;
    let kept = [(0, "bittern"), (0, swept.places[0].as_str())];
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        for (probe, answer) in chat_as_read(&served, &swept, caller).await {
            judged.answer((reader, who), probe, &answer, (&kept, false));
        }
    }
    judged.settle();
    swept.page.peer.close();
}

/// A mention: naming a person in a direct conversation they take no part in gives their agent
/// nothing of it: not in the list, not by a search for its words, not as the list's mentions.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_mention_gives_the_named_persons_agent_nothing_of_a_direct_conversation() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let ada = Served::person(&served.ada, &served.devices.1, &swept.workspace);
    let bo = serde_json::json!({"user": {"person": person_of(&swept.bo.0)}, "start": 0, "len": 3, "is_agent": false});
    let naming = serde_json::json!({"text": "@Bo must not hear that the avocet lands at nine.", "mentions": [bo]});
    let naming = in_place(swept.places[0].as_str(), naming);
    did(&served, &swept, &ada, (&messages(), "post", naming)).await;
    held_message(&served, &swept.workspace, "avocet").await;
    let reads = swept.all_read(&served).await;
    let said = [(0, "avocet")];
    let fields = serde_json::json!({"fields": ["text", "mentions"], "pageSize": 200});
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let at = (reader, *who);
        judged.answer(at, "the list of messages", &reads[reader], (&said, true));
        let listed = tried(
            &served,
            &swept,
            caller,
            (&messages(), "read", fields.clone()),
        )
        .await;
        judged.answer(
            at,
            "the list, asked for its mentions",
            &listed,
            (&said, false),
        );
        let found = served.finds(&swept.workspace, caller, "avocet").await;
        judged.answer(at, "a search for its words", &found, (&said, false));
        let named = served.finds(&swept.workspace, caller, "Bo").await;
        judged.answer(at, "a search for the name", &named, (&said, false));
    }
    judged.settle();
    swept.page.peer.close();
}

/// The list of conversations and who is in them, and the rows a listing of the messages names:
/// a direct conversation is listed to nobody's agent whose person takes no part in it, and a
/// listing's rows are the rows of the read.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_list_of_conversations_names_an_agent_no_direct_conversation_its_person_takes_no_part_in()
 {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    swept.all_read(&served).await;
    let places = [(0, swept.places[0].as_str()), (1, swept.places[1].as_str())];
    let rows = [
        (0, WORDS[0]),
        (1, WORDS[1]),
        (2, WORDS[2]),
        (0, swept.said[0].as_str()),
        (1, swept.said[1].as_str()),
        (2, swept.said[2].as_str()),
    ];
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let at = (reader, *who);
        for (probe, answer) in chat_as_read(&served, &swept, caller).await {
            judged.answer(at, probe, &answer, (&places, false));
        }
        let everything = serde_json::json!({"path": "chat", "kind": "resources", "limit": 200});
        let listed = tool(&served, &swept, caller, ("tilo_ls", everything)).await;
        judged.answer(at, "Chat's resources, listed", &listed, (&places, false));
        judged.answer(at, "Chat's resources, listed", &listed, (&rows, false));
        let messages = serde_json::json!({"path": "chat/Messages", "limit": 200});
        let listed = tool(&served, &swept, caller, ("tilo_ls", messages)).await;
        judged.answer(at, "the messages, listed", &listed, (&rows[..3], true));
        judged.answer(at, "the messages, listed", &listed, (&rows[3..], false));
        judged.answer(at, "the messages, listed", &listed, (&places, false));
    }
    judged.settle();
    swept.page.peer.close();
}

/// What the service's feed of changes answers `caller` after `after`.
async fn changes(
    served: &Served,
    workspace: &str,
    caller: &Caller,
    after: Option<String>,
) -> Result<tilo_actions::remote::ChangePage, String> {
    use tilo_actions::remote::RemoteActionMessage;
    let (actions, caller) = (served.actions.clone(), caller.clone());
    let workspace = WorkspaceId::parse(workspace).unwrap();
    let answered = tokio::task::spawn_blocking(move || {
        let asked = RemoteActionMessage::ChangesRequest {
            correlation: 1,
            after,
        };
        let answer = actions.serve(workspace, &caller, asked)?;
        answer
            .wait_timeout(tilo_test_env::hang_bound())
            .ok()
            .flatten()
    })
    .await
    .unwrap();
    match answered {
        Some(RemoteActionMessage::ChangesResponse { result, .. }) => {
            result.map_err(|error| error.to_string())
        }
        other => Err(format!("not the feed's answer: {other:?}")),
    }
}

/// The feed of changes: every change of the workspace's documents, told to whoever follows it.
/// What is said in a direct conversation after a reader began to follow is told to that reader
/// only where its person takes part: not its words, not its message, not its conversation. The
/// tool that watches is asked the same.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn the_feed_of_changes_tells_an_agent_nothing_of_a_direct_conversation_its_person_takes_no_part_in()
 {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let mut from = Vec::new();
    for (who, caller) in &swept.readers {
        let page = changes(&served, &swept.workspace, caller, None).await;
        from.push(
            page.unwrap_or_else(|error| panic!("{who} follows the feed: {error}"))
                .cursor,
        );
    }
    let now = now_s();
    let later = [
        (&served.ada, "The feed's word is siskin.", "siskin"),
        (&swept.cy.0, "The feed's word is redpoll.", "redpoll"),
        (&served.ada, "The feed's word is linnet.", "linnet"),
    ];
    let mut ids = Vec::new();
    for (at, (root, text, _)) in later.iter().enumerate() {
        ids.push(
            served
                .said_at(&swept.workspace, root, &swept.places[at], (text, now - 30))
                .await,
        );
    }
    swept.all_read(&served).await;
    let mut told: Vec<(usize, &str)> = later.iter().map(|(_, _, word)| *word).enumerate().collect();
    told.extend(ids.iter().map(Id::as_str).enumerate());
    told.extend([(0, swept.places[0].as_str()), (1, swept.places[1].as_str())]);
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let at = (reader, *who);
        let page = changes(
            &served,
            &swept.workspace,
            caller,
            Some(from[reader].clone()),
        )
        .await;
        let feed = match &page {
            Ok(page) => format!("{} changes: {:?}", page.changes.len(), page.changes),
            Err(error) => format!("FAILED: {error}"),
        };
        judged.answer(at, "the feed of changes", &feed, (&told[..3], true));
        judged.answer(at, "the feed of changes", &feed, (&told[3..], false));
        if page.as_ref().map_or(true, |page| page.changes.is_empty()) {
            judged.faults.push(format!(
                "NOT GIVEN: {who}, the feed of changes: tells of no change at all"
            ));
        }
        let watched = tool(
            &served,
            &swept,
            caller,
            ("tilo_watch", serde_json::json!({"wait_ms": 200})),
        )
        .await;
        judged.answer(at, "the tool that watches", &watched, (&told, false));
    }
    judged.settle();
    swept.page.peer.close();
}

/// What the service answers an action on nothing.
const NOTHING: &str = "Err(\"no such action is offered\")";

impl Judged {
    /// What an action answered `who` for an id kept from it and for an id nothing has. Whoever
    /// is not offered the action is answered the same for both. Whoever is offered it is
    /// answered for the kept id as for an action on nothing: never by the action itself, which
    /// would say whose the message is or do what was asked.
    fn as_nothing(&mut self, who: &str, probe: &str, kept: &str, none: &str) {
        if kept != none && kept != NOTHING {
            self.alike(who, probe, kept, none);
        }
    }
}

/// What waits: a question Ada's agent put to her in their conversation is read by nobody
/// else's agent, and nobody else's agent answers it: an answer to it by its id is taken as an
/// action on nothing, and the question still waits.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_question_that_waits_is_neither_read_nor_answered_by_an_agent_of_someone_else() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    let asking = serde_json::json!({
        "text": "May I book the shrike table?",
        "answers": [{"id": "yes", "label": "Yes"}, {"id": "no", "label": "No"}],
    });
    let asking = in_place(swept.places[0].as_str(), asking);
    did(
        &served,
        &swept,
        &swept.readers[4].1,
        (&messages(), "ask", asking),
    )
    .await;
    let question = held_message(&served, &swept.workspace, "shrike").await;
    let reads = swept.all_read(&served).await;
    let asked = [(0, "shrike")];
    let fields = serde_json::json!({"fields": ["text", "question"], "pageSize": 200});
    let nobody = "0".repeat(question.as_str().len());
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let at = (reader, *who);
        judged.answer(at, "the list of messages", &reads[reader], (&asked, true));
        let listed = tried(
            &served,
            &swept,
            caller,
            (&messages(), "read", fields.clone()),
        )
        .await;
        judged.answer(
            at,
            "the list, asked for its questions",
            &listed,
            (&asked, true),
        );
        if takes_part(reader, 0) {
            continue;
        }
        let mut answers = Vec::new();
        for message in [question.as_str(), nobody.as_str()] {
            let answer = serde_json::json!({"message": message, "answer": "yes"});
            let answered = tried(
                &served,
                &swept,
                caller,
                (&messages(), "answer-question", answer),
            )
            .await;
            answers.push(answered.replace(message, "ID"));
        }
        judged.answer(
            at,
            "an answer to the question",
            &answers[0],
            (&asked, false),
        );
        judged.as_nothing(who, "an answer to the question", &answers[0], &answers[1]);
    }
    let part = conversation_part(&served.workspaces, &swept.workspace).await;
    let held = part.messages.iter().find(|(id, _)| **id == question);
    let waits = format!("{:?}", held.expect("the question is kept").1.question);
    if !waits.contains("answered: None") {
        judged.faults.push(format!(
            "ACTED ON: her question was answered from outside: {waits}"
        ));
    }
    judged.settle();
    swept.page.peer.close();
}

/// Chat's actions that take a message by its id as an argument, each with what else it takes.
fn by_message(
    message: &str,
    open: &str,
    author: &str,
) -> Vec<(String, &'static str, serde_json::Value)> {
    use serde_json::json;
    let on_messages = [
        ("pin", json!({"message": message, "pinned": true})),
        (
            "edit-message",
            json!({"message": message, "text": "Changed from outside."}),
        ),
        ("delete-message", json!({"message": message})),
        ("remove-message", json!({"message": message})),
        ("retry", json!({"message": message, "reply": "again"})),
        ("stop-agent", json!({"message": message})),
        (
            "answer-question",
            json!({"message": message, "answer": "allow"}),
        ),
        ("withdraw-question", json!({"message": message})),
        (
            "settle-question",
            json!({"message": message, "outcome": "done"}),
        ),
        ("stop-sharing", json!({"message": message})),
        ("share-open", json!({"message": message, "open": true})),
        (
            "post",
            in_place(open, json!({"text": "About that.", "parent": message})),
        ),
    ];
    let mut calls: Vec<_> = on_messages
        .into_iter()
        .map(|(action, arguments)| (messages(), action, arguments))
        .collect();
    let report =
        json!({"message": message, "author": {"person": author}, "reason": "It looks odd."});
    calls.push(("tilo.moderation".to_owned(), "report-message", report));
    calls
}

/// Chat's actions that take a conversation by its id as an argument.
fn by_conversation(
    conversation: &str,
    message: &str,
) -> Vec<(String, &'static str, serde_json::Value)> {
    use serde_json::json;
    let cause = json!({"kind": "reminder", "label": "From outside", "key": "sweep-1"});
    let summary = json!({"through": message, "through_ts": 1, "text": "From outside.", "conversation": conversation});
    vec![
        (
            messages(),
            "post",
            in_place(conversation, json!({"text": "Written from outside."})),
        ),
        (
            messages(),
            "ask",
            in_place(conversation, json!({"text": "Asked from outside?"})),
        ),
        (
            messages(),
            "ask-agent",
            in_place(conversation, json!({"text": "Do this.", "cause": cause})),
        ),
        (
            String::new(),
            "mark-read",
            in_place(conversation, json!({"part": 0, "seen": 1})),
        ),
        (
            String::new(),
            "export-content",
            in_place(conversation, json!({})),
        ),
        (String::new(), "keep-summary", summary),
    ]
}

/// An action that takes a message's or a conversation's id as an argument: asked with the id
/// of Ada's sentence to her agent, or of their conversation, by an agent whose person takes no
/// part in it, each is answered as an action on nothing or as it is for an id nothing has,
/// says nothing of what is kept, and leaves the conversation as it was. Asked with a direct
/// conversation of others that nothing was said in yet, each is answered the same. Her own
/// agent's action on her message is done.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_action_that_takes_an_id_does_nothing_with_a_direct_conversation_of_others() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    swept.all_read(&served).await;
    let (hers, place) = (swept.said[0].as_str(), swept.places[0].as_str());
    let no_message = "0".repeat(hers.len());
    let unsaid = conversation_with(&served.ada, "an-agent-nobody-keeps");
    let (open, ada) = (swept.places[2].as_str(), person_of(&served.ada));
    let words = [(0, WORDS[0])];
    let hidden = |answer: &str| {
        let mut answer = answer.to_owned();
        for id in [place, unsaid.as_str()] {
            answer = answer.replace(&escaped(id), "ID").replace(id, "ID");
        }
        answer.replace(hers, "ID").replace(&no_message, "ID")
    };
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate().take(4) {
        let kept = by_message(hers, open, &ada)
            .into_iter()
            .chain(by_conversation(place, hers));
        let none = by_message(&no_message, open, &ada)
            .into_iter()
            .chain(by_conversation(unsaid.as_str(), &no_message));
        for ((resource, action, arguments), (_, _, nothing)) in kept.zip(none) {
            let probe = format!("{action} with a kept id");
            let kept = tried(&served, &swept, caller, (&resource, action, arguments)).await;
            let none = tried(&served, &swept, caller, (&resource, action, nothing)).await;
            judged.answer((reader, who), &probe, &kept, (&words, false));
            judged.as_nothing(who, &probe, &hidden(&kept), &hidden(&none));
        }
    }
    let part = conversation_part(&served.workspaces, &swept.workspace).await;
    let there: Vec<String> = part
        .messages
        .iter()
        .map(|(id, held)| format!("{id}: {held:?}"))
        .filter(|held| names(held, place))
        .collect();
    let untouched =
        there.len() == 1 && there[0].contains(HERS) && !there[0].contains("pinned: true");
    if !untouched {
        judged.faults.push(format!(
            "ACTED ON: her conversation is not as she left it: {there:?}"
        ));
    }
    if chat_held(&served, &swept.workspace)
        .await
        .summaries
        .contains_key(place)
    {
        judged
            .faults
            .push("ACTED ON: a summary of her conversation was kept from outside".into());
    }
    let pin = serde_json::json!({"message": hers, "pinned": true});
    let own = tried(
        &served,
        &swept,
        &swept.readers[4].1,
        (&messages(), "pin", pin),
    )
    .await;
    if !own.starts_with("Ok(") {
        judged.faults.push(format!(
            "NOT GIVEN: the owner's agent pins her own message: {own}"
        ));
    }
    judged.settle();
    swept.page.peer.close();
}

/// A question about a kept message: an action that asks its person first, asked on Ada's
/// sentence by an agent whose person takes no part in her conversation, is not answered by
/// the action itself, and nothing the workspace then holds puts her words, or her message's
/// id, before anybody else: no question about it waits in any conversation.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn no_question_is_put_to_a_person_about_a_message_of_a_direct_conversation_of_others() {
    let mut served = Served::start(1).await;
    let swept = Box::pin(swept(&mut served)).await;
    swept.all_read(&served).await;
    let hers = swept.said[0].as_str();
    let nobody = "0".repeat(hers.len());
    let words = [(0, WORDS[0])];
    let probe = "an action that asks first, on the message";
    let mut judged = Judged::default();
    for (reader, (who, caller)) in swept.readers.iter().enumerate() {
        let mut answers = Vec::new();
        for message in [hers, nobody.as_str()] {
            let call = serde_json::json!({"path": "chat/Messages#delete-message", "args": {"message": message}, "wait_ms": 500});
            let answered = tool(&served, &swept, caller, ("tilo_do", call)).await;
            answers.push(answered.replace(message, "ID"));
        }
        judged.answer((reader, who), probe, &answers[0], (&words, false));
        // The action's own failure says whose the message is; an action on nothing says nothing.
        if !takes_part(reader, 0)
            && answers[0] != answers[1]
            && answers[0].contains("action_failed")
        {
            judged.alike(who, probe, &answers[0], &answers[1]);
        }
    }
    let part = conversation_part(&served.workspaces, &swept.workspace).await;
    let all: Vec<_> = part.messages.iter().collect();
    for (id, held) in all {
        let shown = format!("{held:?}");
        if id.as_str() == hers && held.text != HERS {
            judged.faults.push(format!(
                "ACTED ON: her message is not as she left it: {shown}"
            ));
        }
        if id.as_str() != hers && (shown.contains(WORDS[0]) || shown.contains(hers)) {
            judged.faults.push(format!(
                "GIVEN: the message {id} holds something of hers: {}",
                cut(&shown)
            ));
        }
    }
    judged.settle();
    swept.page.peer.close();
}
