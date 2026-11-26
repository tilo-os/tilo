//! A person's connections against a service that is a process of its
//! own: connected by its address, read, kept and removed; what it says of
//! itself kept as data; and what happens when it lies, never stops listing,
//! sends the server elsewhere, wants a sign-in, goes away, or sits where the
//! server does not call.

use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use accounts::vault_items::{ItemError, ItemId, ItemKind, Place};

use crate::VaultAt;
use serde_json::{Value, json};
use test_http::{Reply, Responder};
use tilo_workspace_service::person_connections::{
    Call, Command, Connection, Ended, Leave, SignIn, State, Use,
};

use super::stand_in::{SESSION, StandIn};
use super::{
    ASKS_FIRST, Calling, Reach, add, begin_sign_in, call, finish_sign_in, list, look, mcp, refresh,
    remove, uses,
};

const NOT_KEPT: &str = "That connection is not kept here";
const NOT_CALLED: &str = "That address is not called from here";
const NO_ANSWER: &str = "The service did not answer";
const RAN_OUT: &str = "The service no longer takes this sign-in";
const BEGIN_AGAIN: &str = "That sign-in was not begun here or took too long. Begin it again";
const NOT_SIGNED_IN: &str = "The service did not sign you in";
/// Where the person's own app catches their browser coming back.
const APP: &str = "https://app.example/signed-in";

/// A server whose operator lets it call its own network, where a test's
/// service listens.
fn let_in() -> Reach {
    Reach::of(true)
}

/// A server as it serves people: it calls addresses on the internet only.
fn outside_only() -> Reach {
    Reach::of(false)
}

/// A call of `command` of the connection `id` names, with `arguments`.
fn a_call(id: &str, command: &str, arguments: Value) -> Call {
    let Value::Object(arguments) = arguments else {
        panic!("arguments are an object");
    };
    Call {
        connection: id.to_owned(),
        command: command.to_owned(),
        arguments,
    }
}

/// The commands the service was called with, each with its arguments.
fn calls(seen: &[Value]) -> Vec<(String, Value)> {
    seen.iter()
        .filter(|asked| asked["method"] == "tools/call")
        .map(|asked| {
            (
                asked["command"].as_str().unwrap_or_default().to_owned(),
                asked["arguments"].clone(),
            )
        })
        .collect()
}

/// What a use says, without when it was.
fn told_of(used: &Use) -> (&str, &str, Option<Leave>, Ended, Option<&str>) {
    (
        used.command.as_str(),
        used.by.as_str(),
        used.leave,
        used.ended,
        used.why.as_deref(),
    )
}

fn methods(seen: &[Value]) -> Vec<&str> {
    seen.iter()
        .filter_map(|asked| asked["method"].as_str())
        .collect()
}

fn names(commands: &[Command]) -> Vec<&str> {
    commands
        .iter()
        .map(|command| command.name.as_str())
        .collect()
}

fn asking(commands: &[Command]) -> Vec<bool> {
    commands.iter().map(|command| command.asks).collect()
}

/// A person gives an address. The server greets the service there, reads the
/// name it gives itself and the commands it lists, over several pages and
/// from an event stream, and keeps that under the person's own root. A look
/// keeps nothing, and nothing of it is another person's.
#[tokio::test]
async fn a_service_is_connected_by_its_address_and_its_commands_are_read() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("plain");
    let reach = let_in();

    let looked = look(root.path(), &reach, &service.address()).await.unwrap();
    assert_eq!(looked.address, service.address());
    assert_eq!(looked.name, "Orchard Tools");
    assert!(!looked.needs_sign_in);
    let commands = looked.commands.unwrap();
    assert_eq!(
        names(&commands),
        ["find_trees", "count_apples", "plant_tree"]
    );
    assert_eq!(asking(&commands), [false, false, true]);
    assert_eq!(commands[1].title.as_deref(), Some("Count apples"));
    assert_eq!(
        commands[0].schema["required"],
        serde_json::json!(["orchard"])
    );
    assert!(
        list(root.path()).unwrap().connections.is_empty(),
        "a look keeps nothing"
    );

    // What the service saw of the server: a greeting, then every question
    // under the session and the version the greeting settled.
    let seen = service.seen();
    assert_eq!(
        methods(&seen),
        [
            "initialize",
            "notifications/initialized",
            "tools/list",
            "tools/list"
        ]
    );
    assert_eq!(seen[0]["session"], Value::Null);
    assert_eq!(seen[0]["protocol"], Value::Null);
    assert!(seen[0]["accept"].as_str().is_some_and(
        |accept| accept.contains("application/json") && accept.contains("text/event-stream")
    ));
    for later in &seen[1..] {
        assert_eq!(later["session"], SESSION);
        assert_eq!(later["protocol"], "2025-06-18");
    }
    assert!(seen.iter().all(|asked| asked["signedIn"] == false
        && asked["http"] == "POST"
        && asked["target"] == "/mcp"));

    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    assert_eq!(added.address, service.address());
    assert_eq!(added.name, "Orchard Tools");
    assert_eq!(added.state, State::Ready);
    assert_eq!(added.state_why, None);
    assert_eq!(added.sign_in, SignIn::None);
    assert_eq!(added.last_used_at_ms, None);
    assert_eq!(added.unusable, 0);
    assert!(added.added_at_ms > 0 && added.commands_read_at_ms >= added.added_at_ms);
    assert_eq!(added.commands, commands);
    assert!(added.commands.iter().all(|command| !command.changed));
    assert_eq!(list(root.path()).unwrap().connections, vec![added.clone()]);

    // The same address again is the connection already kept.
    let again = add(root.path(), &reach, &service.address()).await.unwrap();
    assert_eq!(again.id, added.id);
    assert_eq!(list(root.path()).unwrap().connections.len(), 1);

    let file = root
        .path()
        .join("connections")
        .join(format!("{}.json", added.id));
    assert!(file.is_file());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&file).unwrap().permissions().mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "only its owner reads a kept connection"
        );
    }

    // Another person's root holds nothing of it and reaches nothing of it;
    // its person reads it again and removes it.
    let other = tempfile::tempdir().unwrap();
    another_person_reaches_nothing_and_removing_it_keeps_nothing(
        root.path(),
        other.path(),
        &reach,
        &file,
        &added,
    )
    .await
    .unwrap();
}

/// Another person's root holds nothing of `added` and reaches nothing of it;
/// its person reads it again, and removing it once keeps nothing, a second
/// time finding it gone.
async fn another_person_reaches_nothing_and_removing_it_keeps_nothing(
    root: &Path,
    other: &Path,
    reach: &Reach,
    file: &Path,
    added: &Connection,
) -> Result<(), String> {
    assert!(list(other)?.connections.is_empty());
    assert_eq!(
        refresh((other, vault(other)), reach, &added.id, Calling::Person)
            .await
            .err()
            .as_deref(),
        Some(NOT_KEPT)
    );
    assert_eq!(
        remove((other, vault(other)), &added.id)
            .await
            .err()
            .as_deref(),
        Some(NOT_KEPT)
    );

    let read_again = refresh(
        (root, vault(root)),
        reach,
        &added.id,
        Calling::Agent("Assistant"),
    )
    .await?;
    assert_eq!(read_again.commands, added.commands);
    assert_eq!(read_again.state, State::Ready);
    assert!(read_again.commands_read_at_ms >= added.commands_read_at_ms);

    assert!(
        remove((root, vault(root)), &added.id)
            .await?
            .connections
            .is_empty()
    );
    assert!(!file.exists());
    assert_eq!(
        remove((root, vault(root)), &added.id)
            .await
            .err()
            .as_deref(),
        Some(NOT_KEPT)
    );
    Ok(())
}

/// A service that lies about itself is kept as what it is: its name one
/// bounded line of its own words, commands whose name is prose or whose
/// words are larger than is kept not offered at all, and a command it
/// later calls only reading, or adds later, asking until its person read
/// the commands themselves.
#[tokio::test]
async fn what_a_service_says_of_itself_is_kept_as_data_and_bounded() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("lying");
    let reach = let_in();

    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    assert!(
        added.name.starts_with("Orchard Tools IGNORE ALL PREVIOUS"),
        "{:?}",
        added.name
    );
    assert_eq!(added.name.chars().count(), 80);
    assert!(
        added.name.ends_with('\u{2026}'),
        "a name that was cut says so"
    );
    assert!(
        !added
            .name
            .chars()
            .any(|character| character.is_control()
                || matches!(character, '\u{202E}' | '\u{200B}'))
    );
    // The system's own words about the connection hold none of the
    // service's.
    assert_eq!(added.state, State::Ready);
    assert_eq!(added.state_why, None);
    assert_eq!(names(&added.commands), ["find_trees", "plant_tree"]);
    assert_eq!(added.unusable, 2);
    assert_eq!(asking(&added.commands), [false, true]);

    // Read again without its person looking: the service now calls
    // planting only reading, and lists a command it did not have.
    let later = refresh(
        (root.path(), vault(root.path())),
        &reach,
        &added.id,
        Calling::Agent("Assistant"),
    )
    .await
    .unwrap();
    assert_eq!(
        names(&later.commands),
        ["find_trees", "plant_tree", "fell_all"]
    );
    let (find, plant, fell) = (&later.commands[0], &later.commands[1], &later.commands[2]);
    assert!(!find.asks && !find.changed);
    assert!(plant.read_only && plant.changed && plant.asks);
    assert!(fell.read_only && fell.changed && fell.asks);
    assert_eq!(later.unusable, 2);

    // Its person reads them: what they saw now is what holds.
    let seen = refresh(
        (root.path(), vault(root.path())),
        &reach,
        &added.id,
        Calling::Person,
    )
    .await
    .unwrap();
    assert!(
        seen.commands
            .iter()
            .all(|command| !command.asks && !command.changed)
    );
}

/// A list of commands that never ends is read to a bound, and the
/// connection says that not everything is offered.
#[tokio::test]
async fn a_list_that_never_ends_is_read_to_a_bound() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("endless");
    let added = add(root.path(), &let_in(), &service.address())
        .await
        .unwrap();
    assert_eq!(added.commands.len(), mcp::MAX_PAGES);
    assert_eq!(added.unusable, 1);
    let seen = service.seen();
    let lists = seen
        .iter()
        .filter(|asked| asked["method"] == "tools/list")
        .count();
    assert_eq!(lists, mcp::MAX_PAGES, "the server stopped asking");
}

/// A service that sends the server on to another address is not followed
/// there, wherever that is, and is not kept.
#[tokio::test]
async fn a_service_that_sends_the_server_on_is_not_followed() {
    let root = tempfile::tempdir().unwrap();
    let elsewhere = Responder::handle(|_| Some(Reply::status(200).body("{}")));
    let service = StandIn::start(&format!("sent-on:{}/mcp", elsewhere.url()));
    let refused = add(root.path(), &let_in(), &service.address()).await;
    assert_eq!(
        refused.unwrap_err(),
        "The service sent the request on to another address, and a server follows none"
    );
    assert_eq!(methods(&service.seen()), ["initialize"]);
    assert!(elsewhere.drain_requests().is_empty(), "the server followed");
    assert!(list(root.path()).unwrap().connections.is_empty());
}

/// A service that wants a sign-in before it says anything is kept as
/// needing one, with no command offered.
#[tokio::test]
async fn a_service_that_wants_a_sign_in_is_kept_as_needing_one() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("wants-sign-in");
    let reach = let_in();
    let looked = look(root.path(), &reach, &service.address()).await.unwrap();
    assert!(looked.needs_sign_in);
    assert_eq!(looked.commands, None);
    assert_eq!(looked.name, "");

    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    assert_eq!(added.state, State::NeedsSignIn);
    assert_eq!(
        added.state_why.as_deref(),
        Some("The service wants you to sign in")
    );
    assert_eq!(added.sign_in, SignIn::Needed);
    assert!(added.commands.is_empty());
    assert!(
        service
            .seen()
            .iter()
            .all(|asked| asked["signedIn"] == false)
    );
}

/// A service that goes away is said to be unreachable, since when, and its
/// connection keeps what was last read of it.
#[tokio::test]
async fn a_service_that_goes_away_is_unreachable_and_keeps_what_was_read() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("plain");
    let reach = let_in();
    let address = service.address();
    let added = add(root.path(), &reach, &address).await.unwrap();
    service.stop();

    let stood = refresh(
        (root.path(), vault(root.path())),
        &reach,
        &added.id,
        Calling::Person,
    )
    .await
    .unwrap();
    assert_eq!(stood.state, State::Unreachable);
    assert_eq!(stood.state_why.as_deref(), Some(NO_ANSWER));
    assert!(stood.state_since_ms >= added.state_since_ms);
    assert_eq!(stood.commands, added.commands);
    assert_eq!(stood.commands_read_at_ms, added.commands_read_at_ms);
    assert_eq!(list(root.path()).unwrap().connections, vec![stood]);
    // Nobody is there to look at, and nothing new is kept for it.
    assert_eq!(
        look(root.path(), &reach, &address).await.unwrap_err(),
        NO_ANSWER
    );
}

/// Nothing is asked at an address the server does not call: a service inside
/// its network is neither looked at nor added, and one that was added
/// while the operator let the server call there is not asked once that leave
/// is gone. The connection says so and keeps what it had; with the leave
/// back it is ready again.
#[tokio::test]
async fn nothing_is_asked_where_the_server_does_not_call() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("plain");
    let outside = outside_only();
    let with_tls = service.address().replacen("http://", "https://", 1);
    for (address, why) in [
        (
            service.address(),
            "A service's address begins with https://",
        ),
        (with_tls, NOT_CALLED),
    ] {
        let looked = look(root.path(), &outside, &address).await.map(|_| ());
        assert_eq!(looked.unwrap_err(), why);
        let added = add(root.path(), &outside, &address).await.map(|_| ());
        assert_eq!(added.unwrap_err(), why);
    }
    assert!(service.seen().is_empty(), "the service was asked");
    assert!(list(root.path()).unwrap().connections.is_empty());

    let added = add(root.path(), &let_in(), &service.address())
        .await
        .unwrap();
    assert!(!service.seen().is_empty());
    let stood = refresh(
        (root.path(), vault(root.path())),
        &outside,
        &added.id,
        Calling::Person,
    )
    .await
    .unwrap();
    assert_eq!(stood.state, State::Refused);
    assert_eq!(stood.state_why.as_deref(), Some(NOT_CALLED));
    assert_eq!(stood.commands, added.commands);
    assert!(
        service.seen().is_empty(),
        "the service was asked after the leave was gone"
    );

    let back = refresh(
        (root.path(), vault(root.path())),
        &let_in(),
        &added.id,
        Calling::Person,
    )
    .await
    .unwrap();
    assert_eq!(back.state, State::Ready);
    assert_eq!(back.state_why, None);
}

/// One person's server asks services a bounded number of times a minute,
/// whoever suggested the addresses, and another person's asking is their
/// own.
#[tokio::test]
async fn a_persons_server_asks_services_at_a_bounded_pace() {
    let (root, other) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let nobody = {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        format!(
            "http://127.0.0.1:{}/mcp",
            listener.local_addr().unwrap().port()
        )
    };
    let reach = let_in();
    let mut answers = Vec::new();
    for _ in 0..40 {
        answers.push(look(root.path(), &reach, &nobody).await.unwrap_err());
    }
    let asked = answers.iter().filter(|why| *why == NO_ANSWER).count();
    assert_eq!(asked, 30, "{answers:?}");
    assert!(
        answers[30..]
            .iter()
            .all(|why| why.starts_with("Too many services were asked")),
        "{answers:?}"
    );
    assert_eq!(
        look(other.path(), &reach, &nobody).await.unwrap_err(),
        NO_ANSWER
    );
}

/// A command is called with its arguments exactly as given and answers what
/// the service answered. The person calls any command of their connection.
/// An agent calls one the service marks as only reading; one that changes
/// something it does not call by itself, and nothing is sent for it. Every
/// call is written down with who called, by what leave and how it ended,
/// the refused one too, and nobody else's root reaches any of it.
#[tokio::test]
async fn a_command_is_called_as_an_action_and_written_down() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("plain");
    let reach = let_in();
    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let id = added.id.as_str();
    service.seen();

    let planting = json!({"kind": "apple", "row": 4, "notes": {"deep": [1, {"er": null}]}});
    let planted = call(
        (root.path(), vault(root.path())),
        &reach,
        Calling::Person,
        a_call(id, "plant_tree", planting.clone()),
    )
    .await
    .unwrap();
    assert_eq!(
        planted.content,
        vec![json!({"type": "text", "text": "done"})]
    );
    assert!(!planted.is_error);
    assert_eq!(planted.structured, None);

    let assistant = Calling::Agent("Assistant");
    let finding = json!({"orchard": "north"});
    let found = call(
        (root.path(), vault(root.path())),
        &reach,
        assistant,
        a_call(id, "find_trees", finding.clone()),
    )
    .await
    .unwrap();
    assert_eq!(found.structured, Some(json!({"trees": 3})));
    assert_eq!(found.content[0]["text"], "3 trees in \"north\"");

    let refused = call(
        (root.path(), vault(root.path())),
        &reach,
        assistant,
        a_call(id, "plant_tree", planting.clone()),
    )
    .await;
    assert_eq!(refused.unwrap_err(), ASKS_FIRST);
    let unknown = call(
        (root.path(), vault(root.path())),
        &reach,
        Calling::Person,
        a_call(id, "fell_all", json!({})),
    )
    .await;
    assert_eq!(
        unknown.unwrap_err(),
        "That connection offers no such command"
    );

    // The service was called twice, each time with what was given.
    assert_eq!(
        calls(&service.seen()),
        [
            ("plant_tree".to_owned(), planting),
            ("find_trees".to_owned(), finding)
        ]
    );

    let used = uses(root.path(), id).unwrap().uses;
    assert_eq!(
        used.iter().map(told_of).collect::<Vec<_>>(),
        [
            ("plant_tree", "", Some(Leave::Person), Ended::Ok, None),
            (
                "find_trees",
                "Assistant",
                Some(Leave::ReadOnly),
                Ended::Ok,
                None
            ),
            (
                "plant_tree",
                "Assistant",
                None,
                Ended::Refused,
                Some(ASKS_FIRST)
            ),
        ]
    );
    assert!(used.iter().all(|used| used.at_ms >= added.added_at_ms));
    let kept = &list(root.path()).unwrap().connections[0];
    assert!(
        kept.last_used_at_ms
            .is_some_and(|used| used >= added.added_at_ms)
    );
    the_calls_leave(root.path(), &reach, &service, id).await;
}

/// A command the service came to call only reading after its person saw it
/// as a change is not called by an agent, whatever the service says now,
/// and neither is one the service added since: both ask until the person
/// read the commands themselves.
#[tokio::test]
async fn a_service_cannot_mark_its_way_past_the_person() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("lying");
    let reach = let_in();
    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let id = added.id.as_str();
    let assistant = Calling::Agent("Assistant");
    let later = refresh(
        (root.path(), vault(root.path())),
        &reach,
        id,
        Calling::Agent("Assistant"),
    )
    .await
    .unwrap();
    assert!(later.commands.iter().all(|command| command.read_only));
    service.seen();

    for command in ["plant_tree", "fell_all"] {
        let refused = call(
            (root.path(), vault(root.path())),
            &reach,
            assistant,
            a_call(id, command, json!({})),
        )
        .await;
        assert_eq!(refused.unwrap_err(), ASKS_FIRST, "{command}");
    }
    assert!(
        calls(&service.seen()).is_empty(),
        "a command was called unasked"
    );

    // The person reads the commands and so takes them as they are now.
    refresh(
        (root.path(), vault(root.path())),
        &reach,
        id,
        Calling::Person,
    )
    .await
    .unwrap();
    service.seen();
    call(
        (root.path(), vault(root.path())),
        &reach,
        assistant,
        a_call(id, "plant_tree", json!({})),
    )
    .await
    .unwrap();
    assert_eq!(calls(&service.seen()).len(), 1);
}

/// A command the service says failed is answered as failed and written down
/// so; a call the service does not take, and a service that is gone, are
/// said in the system's words, and a service that is gone leaves its
/// connection unreachable.
#[tokio::test]
async fn a_call_that_fails_says_how_and_is_written_down() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("plain");
    let reach = let_in();
    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let id = added.id.as_str();

    let dead = call(
        (root.path(), vault(root.path())),
        &reach,
        Calling::Person,
        a_call(id, "plant_tree", json!({"kind": "dead"})),
    )
    .await
    .unwrap();
    assert!(dead.is_error);
    assert_eq!(dead.content[0]["text"], "nothing grows from that");

    let large = json!({"notes": "n".repeat(300 * 1024)});
    let too_large = call(
        (root.path(), vault(root.path())),
        &reach,
        Calling::Person,
        a_call(id, "plant_tree", large),
    )
    .await;
    assert_eq!(
        too_large.unwrap_err(),
        "Those arguments are larger than one call carries"
    );

    service.stop();
    let gone = call(
        (root.path(), vault(root.path())),
        &reach,
        Calling::Person,
        a_call(id, "plant_tree", json!({})),
    )
    .await;
    assert_eq!(gone.unwrap_err(), NO_ANSWER);
    let stood = &list(root.path()).unwrap().connections[0];
    assert_eq!(stood.state, State::Unreachable);

    let used = uses(root.path(), id).unwrap().uses;
    assert_eq!(
        used.iter().map(told_of).collect::<Vec<_>>(),
        [
            (
                "plant_tree",
                "",
                Some(Leave::Person),
                Ended::Failed,
                Some("The service said the command failed")
            ),
            (
                "plant_tree",
                "",
                Some(Leave::Person),
                Ended::Refused,
                Some("Those arguments are larger than one call carries")
            ),
            (
                "plant_tree",
                "",
                Some(Leave::Person),
                Ended::Failed,
                Some(NO_ANSWER)
            ),
        ]
    );
}

/// Sign the person at `root` in to the connection `id` of `service`, as
/// their app and their browser do it between them.
async fn sign_in(root: &Path, reach: &Reach, service: &StandIn, id: &str) -> Connection {
    let step = begin_sign_in(root, reach, id, APP).await.unwrap();
    let returned = service.browse(&step.open);
    finish_sign_in((root, vault(root)), reach, id, &returned)
        .await
        .unwrap()
}

/// What `name` is in the address a browser came back to.
fn came_back_with(returned: &str, name: &str) -> String {
    returned
        .split(&format!("{name}="))
        .nth(1)
        .and_then(|rest| rest.split('&').next())
        .unwrap_or_default()
        .to_owned()
}

/// The vault of the person at `root`, as the workspace's vault stands in for them here.
fn held(root: &Path) -> &'static crate::vault_at::TestStores {
    static HELD: std::sync::Mutex<
        std::collections::BTreeMap<PathBuf, &'static crate::vault_at::TestStores>,
    > = std::sync::Mutex::new(std::collections::BTreeMap::new());
    HELD.lock()
        .unwrap()
        .entry(root.to_path_buf())
        .or_insert_with(|| Box::leak(Box::default()))
}

/// The workspace's vault for the person at `root`.
fn vault(root: &Path) -> VaultAt<'static> {
    VaultAt::Held(held(root))
}

/// The items of the vault under `root`: each one's id, its kind and where
/// it belongs.
fn vault_items(root: &Path) -> Vec<(String, ItemKind, Vec<String>)> {
    held(root)
        .lock()
        .unwrap()
        .facts()
        .into_iter()
        .map(|facts| {
            (
                facts.id.as_str().to_owned(),
                facts.kind,
                facts.places.into_iter().map(String::from).collect(),
            )
        })
        .collect()
}

/// Every file under `dir` whose bytes hold `needle`.
fn files_holding(dir: &Path, needle: &str) -> Vec<PathBuf> {
    let needle = needle.as_bytes();
    let mut holding = Vec::new();
    let mut waiting = vec![dir.to_path_buf()];
    while let Some(next) = waiting.pop() {
        for entry in std::fs::read_dir(&next).into_iter().flatten().flatten() {
            let path = entry.path();
            match std::fs::metadata(&path) {
                Ok(metadata) if metadata.is_dir() => waiting.push(path),
                Ok(metadata) if metadata.is_file() => {
                    let bytes = std::fs::read(&path).unwrap();
                    if bytes.windows(needle.len()).any(|window| window == needle) {
                        holding.push(path);
                    }
                }
                _ => {}
            }
        }
    }
    holding
}

/// The lines a subscriber wrote.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(bytes);
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self {
        self.clone()
    }
}

/// The kinds of exchange the service's issuer saw.
fn grants(seen: &[Value]) -> Vec<&str> {
    seen.iter()
        .filter_map(|asked| asked["grant"].as_str())
        .filter(|grant| !grant.is_empty())
        .collect()
}

/// A service that wants a sign-in says where its person signs in. The
/// person begins it, signs in in their browser, and finishes it with what
/// the browser came back with. What the service's issuer hands out goes
/// into their vault and nowhere else: the token the service takes belongs
/// to the service's address and is handed out there alone, the one that
/// renews it to the issuer's. Every request after that shows the sign-in,
/// an agent's call too, and no file, no kept connection and no log line
/// holds a token.
#[tokio::test]
async fn a_person_signs_in_by_the_services_own_flow_and_the_vault_keeps_what_it_hands_out() {
    let lines = Lines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_max_level(tracing::Level::TRACE)
        .with_ansi(false)
        .with_writer(lines.clone())
        .finish();
    let _logged = tracing::subscriber::set_default(subscriber);
    tracing::info!("the log is being read");

    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("signing-in");
    let reach = let_in();
    let looked = look(root.path(), &reach, &service.address()).await.unwrap();
    assert!(looked.needs_sign_in);
    assert_eq!(looked.name, "Orchard Tools");
    assert_eq!(looked.sign_in_at, Some(service.issuer_site()));
    assert_eq!(looked.commands, None);

    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let id = added.id.as_str();
    assert_eq!(
        (added.state, added.sign_in),
        (State::NeedsSignIn, SignIn::Needed)
    );
    assert_eq!(added.sign_in_at, Some(service.issuer_site()));
    assert_eq!(added.name, "Orchard Tools");
    assert!(added.commands.is_empty());
    assert!(vault_items(root.path()).is_empty());

    let step = begin_sign_in(root.path(), &reach, id, APP).await.unwrap();
    let issuer = format!("http://{}/issuer/authorize?", service.issuer_site());
    assert!(
        step.open.starts_with(&issuer),
        "the browser goes to the issuer"
    );
    assert!(step.expires_at_ms > added.added_at_ms);
    let returned = service.browse(&step.open);
    assert!(returned.starts_with(APP));
    let signed = finish_sign_in((root.path(), vault(root.path())), &reach, id, &returned)
        .await
        .unwrap();
    assert_eq!(
        (signed.state, signed.sign_in),
        (State::Ready, SignIn::SignedIn)
    );
    assert_eq!(signed.state_why, None);
    assert_eq!(
        names(&signed.commands),
        ["find_trees", "count_apples", "plant_tree"]
    );
    assert!(signed.commands.iter().all(|command| !command.changed));

    // Until the sign-in the service was shown none; from it on every
    // request shows one the service takes, and the browser's code was
    // exchanged once.
    let seen = service.seen();
    let asked: Vec<&Value> = seen
        .iter()
        .filter(|asked| asked["target"] == "/mcp")
        .collect();
    let from = asked
        .iter()
        .position(|asked| asked["signedIn"] == true)
        .expect("the service was shown a sign-in");
    assert!(asked[..from].iter().all(|asked| asked["signedIn"] == false));
    assert!(asked[from..].iter().all(|asked| asked["taken"] == true));
    assert_eq!(grants(&seen), ["authorization_code"]);

    let assistant = Calling::Agent("Assistant");
    let finding = a_call(id, "find_trees", json!({"orchard": "north"}));
    let found = call(
        (root.path(), vault(root.path())),
        &reach,
        assistant,
        finding,
    )
    .await
    .unwrap();
    assert_eq!(found.structured, Some(json!({"trees": 3})));
    assert!(service.seen().iter().all(|asked| asked["taken"] == true));

    a_sign_in_left(root.path(), &service, (id, returned.as_str()), &lines);

    // Its sign-in leaves the vault with the connection.
    remove((root.path(), vault(root.path())), id).await.unwrap();
    assert!(vault_items(root.path()).is_empty());
}

/// A sign-in the service takes no more is renewed behind the call that met
/// it, and what the vault kept is replaced. One that cannot be renewed
/// ends: the call says so, the connection says so and keeps what was last
/// read, the vault keeps nothing of it, and the person signs in again.
#[tokio::test]
async fn a_sign_in_that_ran_out_is_renewed_and_one_that_cannot_be_says_so() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("signing-in");
    let reach = let_in();
    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let id = added.id.as_str();
    let signed = sign_in(root.path(), &reach, &service, id).await;
    let before = vault_items(root.path());
    let assistant = Calling::Agent("Assistant");
    let finding = || a_call(id, "find_trees", json!({"orchard": "north"}));
    service.seen();

    service.lapse();
    let found = call(
        (root.path(), vault(root.path())),
        &reach,
        assistant,
        finding(),
    )
    .await
    .unwrap();
    assert_eq!(found.structured, Some(json!({"trees": 3})));
    let seen = service.seen();
    assert_eq!(grants(&seen), ["refresh_token"]);
    assert_eq!(calls(&seen).len(), 1, "the command was called once");
    let after = vault_items(root.path());
    assert_eq!(after.len(), 2);
    assert!(
        after.iter().all(|item| !before.contains(item)),
        "what was renewed replaced what the vault kept"
    );
    let kept = &list(root.path()).unwrap().connections[0];
    assert_eq!((kept.state, kept.sign_in), (State::Ready, SignIn::SignedIn));

    service.withdraw();
    let refused = call(
        (root.path(), vault(root.path())),
        &reach,
        assistant,
        finding(),
    )
    .await;
    assert_eq!(refused.unwrap_err(), RAN_OUT);
    let stood = &list(root.path()).unwrap().connections[0];
    assert_eq!(
        (stood.state, stood.sign_in),
        (State::SignInRanOut, SignIn::Needed)
    );
    assert_eq!(stood.state_why.as_deref(), Some(RAN_OUT));
    assert_eq!(stood.commands, signed.commands);
    assert!(
        vault_items(root.path()).is_empty(),
        "a sign-in that is over leaves the vault"
    );
    // Reading it again says the same until the person signed in again.
    let read = refresh((root.path(), vault(root.path())), &reach, id, assistant)
        .await
        .unwrap();
    assert_eq!(read.state, State::SignInRanOut);
    let written = uses(root.path(), id).unwrap().uses;
    assert_eq!(
        written.iter().map(told_of).collect::<Vec<_>>(),
        [
            (
                "find_trees",
                "Assistant",
                Some(Leave::ReadOnly),
                Ended::Ok,
                None
            ),
            (
                "find_trees",
                "Assistant",
                Some(Leave::ReadOnly),
                Ended::Failed,
                Some(RAN_OUT)
            ),
        ]
    );

    let again = sign_in(root.path(), &reach, &service, id).await;
    assert_eq!(
        (again.state, again.sign_in),
        (State::Ready, SignIn::SignedIn)
    );
    assert_eq!(vault_items(root.path()).len(), 2);
}

/// A sign-in is finished once, by what its own browser came back with, at
/// the place its app catches: another state, another place, a second try
/// and a code from another sign-in each end it, and nothing is kept.
#[tokio::test]
async fn a_sign_in_is_finished_once_and_only_by_what_its_own_browser_came_back_with() {
    let root = tempfile::tempdir().unwrap();
    let service = StandIn::start("signing-in");
    let reach = let_in();
    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let id = added.id.as_str();

    let unbegun = format!("{APP}?code=orchard-code-1&state=guessed");
    let refused = finish_sign_in((root.path(), vault(root.path())), &reach, id, &unbegun).await;
    assert_eq!(refused.unwrap_err(), BEGIN_AGAIN);
    for back in [
        "javascript:alert(1)",
        "http://elsewhere.example/signed-in",
        "https://app.example/signed-in#fragment",
        "tilo://signed-in",
    ] {
        let begun = begin_sign_in(root.path(), &reach, id, back).await;
        assert!(begun.is_err(), "a browser would come back to {back}");
    }

    // Another state than the one it was begun with, and then the true one
    // too late: a sign-in is finished once.
    let step = begin_sign_in(root.path(), &reach, id, APP).await.unwrap();
    let returned = service.browse(&step.open);
    let forged = returned.replacen("state=", "state=x", 1);
    let refused = finish_sign_in((root.path(), vault(root.path())), &reach, id, &forged).await;
    assert_eq!(refused.unwrap_err(), BEGIN_AGAIN);
    let late = finish_sign_in((root.path(), vault(root.path())), &reach, id, &returned).await;
    assert_eq!(late.unwrap_err(), BEGIN_AGAIN);

    // Come back to another place than its app's.
    let step = begin_sign_in(root.path(), &reach, id, APP).await.unwrap();
    let returned = service.browse(&step.open);
    let elsewhere = returned.replacen(APP, "https://app.example/other", 1);
    let refused = finish_sign_in((root.path(), vault(root.path())), &reach, id, &elsewhere).await;
    assert_eq!(refused.unwrap_err(), BEGIN_AGAIN);

    // The code of another sign-in under this one's state: the issuer does
    // not take it, because the key that proves it is not this sign-in's.
    let first = begin_sign_in(root.path(), &reach, id, APP).await.unwrap();
    let first_back = service.browse(&first.open);
    let second = begin_sign_in(root.path(), &reach, id, APP).await.unwrap();
    let second_back = service.browse(&second.open);
    let mixed = second_back.replacen(
        &came_back_with(&second_back, "code"),
        &came_back_with(&first_back, "code"),
        1,
    );
    assert_ne!(mixed, second_back);
    let refused = finish_sign_in((root.path(), vault(root.path())), &reach, id, &mixed).await;
    assert_eq!(refused.unwrap_err(), NOT_SIGNED_IN);

    assert!(vault_items(root.path()).is_empty());
    let kept = &list(root.path()).unwrap().connections[0];
    assert_eq!(
        (kept.state, kept.sign_in),
        (State::NeedsSignIn, SignIn::Needed)
    );
    assert_eq!(
        grants(&service.seen()),
        ["authorization_code"],
        "one exchange was tried, and the issuer refused it"
    );
}

/// A person the service did not sign in is told so, and no step of a
/// sign-in follows a redirect: an issuer whose exchange sends the server on
/// to another address signs nobody in, and that address is never asked.
#[tokio::test]
async fn a_refused_sign_in_says_so_and_no_step_of_one_follows_a_redirect() {
    let reach = let_in();
    {
        let root = tempfile::tempdir().unwrap();
        let service = StandIn::start("signing-in:refused");
        let added = add(root.path(), &reach, &service.address()).await.unwrap();
        let step = begin_sign_in(root.path(), &reach, &added.id, APP)
            .await
            .unwrap();
        let returned = service.browse(&step.open);
        assert!(returned.contains("error="));
        let refused = finish_sign_in(
            (root.path(), vault(root.path())),
            &reach,
            &added.id,
            &returned,
        )
        .await;
        assert_eq!(refused.unwrap_err(), NOT_SIGNED_IN);
        assert!(vault_items(root.path()).is_empty());
        assert!(grants(&service.seen()).is_empty(), "nothing was exchanged");
    }
    let root = tempfile::tempdir().unwrap();
    let elsewhere = Responder::handle(|_| {
        Some(
            Reply::status(200)
                .header("Content-Type", "application/json")
                .body(r#"{"access_token":"from-elsewhere","token_type":"Bearer"}"#),
        )
    });
    let service = StandIn::start(&format!(
        "signing-in:token-sent-on:{}/token",
        elsewhere.url()
    ));
    let added = add(root.path(), &reach, &service.address()).await.unwrap();
    let step = begin_sign_in(root.path(), &reach, &added.id, APP)
        .await
        .unwrap();
    let returned = service.browse(&step.open);
    let refused = finish_sign_in(
        (root.path(), vault(root.path())),
        &reach,
        &added.id,
        &returned,
    )
    .await;
    assert_eq!(refused.unwrap_err(), NOT_SIGNED_IN);
    assert!(elsewhere.drain_requests().is_empty(), "the server followed");
    assert!(vault_items(root.path()).is_empty());
    let kept = &list(root.path()).unwrap().connections[0];
    assert_eq!(
        (kept.state, kept.sign_in),
        (State::NeedsSignIn, SignIn::Needed)
    );
}

/// What the calls of the connection `id` leave: neither arguments nor
/// answers written down, nothing another person's root reaches, and nothing
/// at all once the connection is removed.
async fn the_calls_leave(root: &Path, reach: &Reach, service: &StandIn, id: &str) {
    // What was written down holds neither the arguments nor the answer.
    let written =
        std::fs::read_to_string(root.join("connections").join(format!("{id}.uses.json"))).unwrap();
    assert!(
        !written.contains("apple") && !written.contains("north") && !written.contains("trees in")
    );

    let other = tempfile::tempdir().unwrap();
    let theirs = call(
        (other.path(), vault(other.path())),
        reach,
        Calling::Person,
        a_call(id, "find_trees", json!({})),
    )
    .await;
    assert_eq!(theirs.unwrap_err(), NOT_KEPT);
    assert_eq!(uses(other.path(), id).unwrap_err(), NOT_KEPT);
    assert!(calls(&service.seen()).is_empty());

    // Its uses go with the connection.
    remove((root, vault(root)), id).await.unwrap();
    assert_eq!(uses(root, id).unwrap_err(), NOT_KEPT);
    let written = root.join("connections").join(format!("{id}.uses.json"));
    assert!(
        !written.exists(),
        "what a removed connection was used for is kept"
    );
}

/// What signing in to the connection `id` left, for the browser that came
/// back with `returned`: two keys in the vault, each handed out where it
/// belongs and nowhere else and written down as used for the agent that
/// called, and nothing of what was handed out in the clear, in a file of
/// the person's or in a line that was logged.
fn a_sign_in_left(root: &Path, service: &StandIn, (id, returned): (&str, &str), lines: &Lines) {
    // The vault keeps two keys: one for the service's address, one for
    // its issuer's, each handed out where it belongs and nowhere else.
    let items = vault_items(root);
    assert_eq!(items.len(), 2);
    assert!(items.iter().all(|(_, kind, _)| *kind == ItemKind::Key));
    let at_service = format!("http://{}", service.site());
    let at_issuer = format!("http://{}", service.issuer_site());
    let item_at = |place: &str| {
        let (id, _, _) = items
            .iter()
            .find(|(_, _, places)| *places == [place.to_owned()])
            .unwrap_or_else(|| panic!("no key belongs to {place}: {items:?}"));
        ItemId::parse(id).unwrap()
    };
    let (access, renewal) = (item_at(&at_service), item_at(&at_issuer));
    let mut vault = held(root).lock().unwrap();
    for (item, elsewhere) in [(&access, &at_issuer), (&renewal, &at_service)] {
        let elsewhere = Place::parse(elsewhere).unwrap();
        let handed = vault
            .use_as(item, ItemKind::Key, &[elsewhere], "")
            .map(|_| ());
        assert_eq!(handed, Err(ItemError::WrongPlace));
    }
    let used = vault.uses(&access).unwrap();
    assert!(used.iter().any(|used| used.by == "Assistant"), "{used:?}");
    let service_alone = [Place::parse(&at_service).unwrap()];
    assert!(
        used.iter().all(|used| used.places == service_alone),
        "{used:?}"
    );
    drop(vault);

    // Nothing of what was handed out is anywhere in the clear: in no file
    // of the person's, and in no line that was logged.
    let state = came_back_with(returned, "state");
    assert!(!state.is_empty());
    let logged = String::from_utf8(lines.0.lock().unwrap().clone()).unwrap();
    assert!(logged.contains("the log is being read"));
    for secret in [
        "orchard-access",
        "orchard-renewal",
        "orchard-code",
        state.as_str(),
    ] {
        assert!(
            files_holding(root, secret).is_empty(),
            "a file holds what a sign-in handed out"
        );
        assert!(
            !logged.contains(secret),
            "a log line holds what a sign-in handed out"
        );
    }
    let kept =
        std::fs::read_to_string(root.join("connections").join(format!("{id}.json"))).unwrap();
    assert!(kept.contains(access.as_str()) && kept.contains(renewal.as_str()));
}
