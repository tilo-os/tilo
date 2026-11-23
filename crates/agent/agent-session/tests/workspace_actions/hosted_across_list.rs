//! What a person's own assistant is told of their other workspaces, on a turn the server
//! carries out in the workspace that is theirs alone: the tools such a turn has list them, each
//! with whether the assistant may read there, and read nothing in any of them. Every call goes
//! through a whole turn of the assistant, with a model that calls the tools.
use super::*;
use tilo_server::environment::{EnvironmentRegistry, EnvironmentScope};

/// The tool path that lists a person's other workspaces.
const LIST: &str = "other-workspaces/workspaces#list";
/// The name a listing of everything gives the provider of that list.
const NAMED: &str = "other-workspaces";

/// A model that calls each of `calls` at once, a tool with its arguments, and then answers.
/// Every request it was sent is kept in `asked`.
fn calling(calls: &[(&str, serde_json::Value)], asked: &Arc<Mutex<Vec<ModelRequest>>>) -> Model {
    let calls: Vec<Content> = calls
        .iter()
        .enumerate()
        .map(|(at, (name, input))| {
            Content::ToolCall(ToolCall {
                id: format!("call-{at}"),
                name: (*name).into(),
                input: input.clone(),
                extra_content: None,
            })
        })
        .collect();
    let asked = Arc::clone(asked);
    Arc::new(move || {
        Some(Box::new(Scripted {
            answers: vec![
                answer(calls.clone(), StopReason::ToolCalls, 1),
                answer(
                    vec![Content::Text {
                        text: "I looked.".into(),
                    }],
                    StopReason::EndTurn,
                    1,
                ),
            ],
            asked: Arc::clone(&asked),
        }) as Box<dyn tilo_agent_session::model::PlanTurns>)
    })
}

/// The person of `root` writes `words` to the assistant in `workspace` on `device`, the
/// assistant acting for `person` (Ada where none is named), and the server answers that one turn with a model that calls `calls`, its agents told the devices
/// `placed` holds are the server's own. What each call was answered, in order.
async fn asked_by(
    served: &Served,
    (workspace, person): (&str, Option<(&SigningKey, &crate::support::Device)>),
    (root, device): (&SigningKey, &crate::support::Device),
    (words, calls): (&str, &[(&str, serde_json::Value)]),
    placed: Option<&EnvironmentRegistry>,
) -> Vec<String> {
    let asked = Arc::new(Mutex::new(Vec::new()));
    let message = write_to_agent(
        &served.actions,
        &served.workspaces,
        workspace,
        (root, device),
        words,
    )
    .await;
    let model = calling(calls, &asked);
    answered_knowing(
        served,
        (workspace, person),
        &message,
        model,
        placed.cloned(),
    )
    .await;
    tools_answered(&asked)
}

/// The two calls a turn makes here: a listing of everything, then the list of its person's
/// other workspaces.
fn everything_then_the_list() -> [(&'static str, serde_json::Value); 2] {
    [
        ("tilo_ls", serde_json::json!({})),
        ("tilo_do", serde_json::json!({"path": LIST, "args": {}})),
    ]
}

/// The workspaces the list in `answered` names, each by its id with what the list says of
/// reading there. Empty where `answered` holds no list.
fn listed_in(answered: &str) -> BTreeMap<String, String> {
    let whole = answered
        .find('{')
        .zip(answered.rfind('}'))
        .and_then(|(from, to)| answered.get(from..=to))
        .and_then(|held| serde_json::from_str::<serde_json::Value>(held).ok())
        .unwrap_or_default();
    whole["workspaces"]
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|one| {
            Some((
                one["workspace"].as_str()?.to_owned(),
                one["reading"].as_str()?.to_owned(),
            ))
        })
        .collect()
}

/// The server a test's fixture keeps, for what only the server itself holds: the list of the
/// devices it placed.
fn server_of(served: &Served) -> &tilo_server::daemon::Server {
    &served.alive
}

/// A person whose newest device in another workspace is one the server placed for a window:
/// the assistant never reads there as that device. Where the server still answers the placed
/// device for the person, they are on no device of their own there and nothing may be read;
/// where it answers a device of their own, that one is the device. The server's own device,
/// a member of every workspace it governs, is never the device either. And agents that were
/// not told which devices the server placed read in no other workspace at all.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_device_the_server_placed_for_a_window_is_never_the_one_the_assistant_reads_as() {
    let mut served = Served::start(2).await;
    let (own, own_page, _) = served.workspace(|_| Vec::new()).await;
    let (windowed, windowed_page) = served.opened.pop().expect("a second workspace");
    // A workspace no device of Ada's own ever joined: the server's device is all she has there.
    let bare = served.workspaces.create("Depot").await.unwrap().workspace;
    let ada = served.ada.verifying_key().to_bytes();
    let hers = (&served.ada, &served.devices.1);
    let server = server_of(&served);
    let placed = server.environments();

    // The fixture certifies every device at the same early moment, the server's own among
    // them. Her tablet is certified a second ago and joins the second workspace: a device of
    // her own, newer than those.
    let mut tablet = enrol(&served.ada, "tablet", DeviceCapabilities::member());
    tablet.cert.issued_at_ns = tilo_clock::unix_now_ns().saturating_sub(1_000_000_000);
    tilo_user_identity::sign_device_cert(&mut tablet.cert, &served.ada)
        .expect("her root certifies the tablet");
    let tablet_page = Browser::open_at(server, &tablet, &served.devices.0, &windowed).await;
    // Her laptop is certified now and joins too: it is the newest device she has there. The
    // server's list of the devices it placed holds it: what a device the server certified for
    // that workspace's window is.
    let mut laptop = enrol(&served.ada, "laptop", DeviceCapabilities::member());
    laptop.cert.issued_at_ns = tilo_clock::unix_now_ns();
    tilo_user_identity::sign_device_cert(&mut laptop.cert, &served.ada)
        .expect("her root certifies the laptop");
    let laptop_page = Browser::open_at(server, &laptop, &served.devices.0, &windowed).await;
    let windowed_id = WorkspaceId::parse(&windowed).unwrap();
    let window = laptop.cert.device_public_key;
    placed
        .place(
            window,
            EnvironmentScope::for_resource(windowed_id, "so.tilo.browser", "window"),
            tilo_clock::unix_now_ns().saturating_add(3_600_000_000_000),
        )
        .expect("the server keeps what it placed");
    assert!(
        laptop.cert.issued_at_ns > tablet.cert.issued_at_ns,
        "the placed device is the newest she has"
    );
    let answered_for_her = served.workspaces.person_device(windowed_id, ada).await;

    let calls = everything_then_the_list();
    let shown = asked_by(
        &served,
        (&own, None),
        hers,
        ("Which other workspaces am I in?", &calls),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    let listed = listed_in(&shown[1]);
    let reading = |workspace: &str| listed.get(workspace).map(String::as_str);
    if answered_for_her == Some(window) {
        // The server answers the placed device for her: she is on none of her own there.
        assert_eq!(
            reading(&windowed),
            Some("no_device"),
            "nothing is read as the device the server placed: {}",
            shown[1]
        );
    } else {
        assert_eq!(
            answered_for_her.map(hex::encode),
            Some(hex::encode(tablet.cert.device_public_key)),
            "the device a read acts as is her own, the newest the server did not place"
        );
        assert_eq!(reading(&windowed), Some("not_asked"), "{}", shown[1]);
    }
    assert_eq!(
        reading(&bare),
        Some("no_device"),
        "the server's own device is never the one a read acts as: {}",
        shown[1]
    );

    // Agents that were told nothing of what the server placed cannot tell a placed device from
    // her own: they say she is on no device in any of them.
    let untold = asked_by(
        &served,
        (&own, None),
        hers,
        ("Which other workspaces am I in, once more?", &calls),
        None,
    )
    .await;
    assert_eq!(untold.len(), 2, "two tool calls were answered: {untold:?}");
    let listed = listed_in(&untold[1]);
    assert_eq!(
        (
            listed.get(&windowed).map(String::as_str),
            listed.get(&bare).map(String::as_str)
        ),
        (Some("no_device"), Some("no_device")),
        "{}",
        untold[1]
    );
    for page in [own_page, windowed_page, tablet_page, laptop_page] {
        page.peer.close();
    }
}

/// A person's own assistant, in the workspace that is theirs alone, finds the list of their
/// other workspaces among its tools and reads it: every other workspace they are a member of
/// on the server, and not the one the turn is in. A turn that begins once somebody else is a
/// member of that workspace is named no such list, and so is a turn another member starts on
/// the assistant: each is answered what a call of something that does not exist is answered.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_persons_own_assistant_lists_their_other_workspaces_while_its_workspace_is_theirs_alone()
{
    let mut served = Served::start(2).await;
    let (own, own_page, _) = served.workspace(|_| Vec::new()).await;
    let (shared, shared_page) = served.opened.pop().expect("a second workspace");
    let hers = (&served.ada, &served.devices.1);
    let server = server_of(&served);
    let placed = server.environments();
    // The fixture certifies every device at the same early moment, the server's own among
    // them, so which is her newest in a workspace is not told. Her laptop is certified now
    // and joins her other workspace: the newest device she has there is one of her own.
    let mut laptop = enrol(&served.ada, "laptop", DeviceCapabilities::member());
    laptop.cert.issued_at_ns = tilo_clock::unix_now_ns();
    tilo_user_identity::sign_device_cert(&mut laptop.cert, &served.ada)
        .expect("her root certifies the laptop");
    let laptop_page = Browser::open_at(server, &laptop, &served.devices.0, &shared).await;
    let calls = everything_then_the_list();

    let shown = asked_by(
        &served,
        (&own, None),
        hers,
        ("Which other workspaces am I in?", &calls),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    assert!(
        shown[0].contains(NAMED),
        "the turn's listing of everything names the list: {}",
        shown[0]
    );
    let listed = listed_in(&shown[1]);
    assert_eq!(
        listed,
        BTreeMap::from([(shared.clone(), "not_asked".to_owned())]),
        "her other workspace and nothing else, and nothing read there unasked: {}",
        shown[1]
    );

    // Cy joins the workspace her assistant lives in: whoever is in it holds the conversation.
    let (cy, _) = tilo_user_identity::UserIdentity::generate("Cy", 1);
    let cy_device = enrol(&cy, "browser", DeviceCapabilities::member());
    admit_as(&served, &own, &cy_device, Role::Editor).await;
    // A turn of hers that begins from then on is named no such list, and its call is answered
    // what a call of something that does not exist is answered, word for word but for the name.
    let nothing = "no-such-thing/workspaces#list";
    let after = [
        ("tilo_ls", serde_json::json!({})),
        ("tilo_do", serde_json::json!({"path": LIST, "args": {}})),
        ("tilo_do", serde_json::json!({"path": nothing, "args": {}})),
    ];
    let shown = asked_by(
        &served,
        (&own, None),
        hers,
        ("And now, which other workspaces am I in?", &after),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 3, "three tool calls were answered: {shown:?}");
    assert!(
        !shown[0].contains(NAMED),
        "the turn's tools name no list of other workspaces any more: {}",
        shown[0]
    );
    assert_eq!(
        shown[1].replace(NAMED, "no-such-thing"),
        shown[2],
        "nothing tells the list apart from something that does not exist"
    );
    assert!(
        listed_in(&shown[1]).is_empty() && !shown[1].contains(&shared),
        "and it names no workspace: {}",
        shown[1]
    );

    // A turn Cy starts on her assistant acts for him and is answered the same.
    let his = [
        ("tilo_do", serde_json::json!({"path": LIST, "args": {}})),
        ("tilo_do", serde_json::json!({"path": nothing, "args": {}})),
    ];
    let shown = asked_by(
        &served,
        (&own, None),
        (&cy, &cy_device),
        ("Which other workspaces is Ada in?", &his),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    assert_eq!(
        shown[0].replace(NAMED, "no-such-thing"),
        shown[1],
        "nothing tells the list apart from something that does not exist"
    );
    assert!(
        !shown[0].contains(&shared) && !shown[0].contains("another member"),
        "{}",
        shown[0]
    );
    for page in [own_page, shared_page, laptop_page] {
        page.peer.close();
    }
}

/// On a person's own server the server's own device is a device of that person too, and it is a
/// member of every workspace the server governs. Where it is the newest device of theirs a
/// workspace admits, a turn of theirs there acts as it, and their assistant still never reads
/// in another workspace as it: the device a read acts as is the newest of their own that the
/// workspace admits, here the one their page is on. A workspace no device of their own ever
/// joined has none.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn where_the_servers_own_device_is_a_persons_newest_the_assistant_reads_as_their_next_one() {
    // The server's own device is certified now, later than every device the fixture gives Ada.
    let mut served = Served::start_certified(2, tilo_clock::unix_now_ns()).await;
    let (own, own_page, _) = served.workspace(|_| Vec::new()).await;
    let (other, other_page) = served.opened.pop().expect("a second workspace");
    // A workspace no device of Ada's own ever joined: the server's device is all she has there.
    let bare = served.workspaces.create("Depot").await.unwrap().workspace;
    let ada = served.ada.verifying_key().to_bytes();
    let hers = (&served.ada, &served.devices.1);
    let placed = server_of(&served).environments();
    assert!(
        served.devices.0.cert.issued_at_ns > served.devices.1.cert.issued_at_ns,
        "the server's own device is the newest she has"
    );
    let other_id = WorkspaceId::parse(&other).unwrap();
    assert_eq!(
        served
            .workspaces
            .person_device(other_id, ada)
            .await
            .map(hex::encode),
        Some(hex::encode(served.devices.0.cert.device_public_key)),
        "the device a turn of hers acts as in her other workspace is the server's own"
    );

    let calls = everything_then_the_list();
    let shown = asked_by(
        &served,
        (&own, None),
        hers,
        ("Which other workspaces am I in?", &calls),
        Some(&placed),
    )
    .await;
    assert_eq!(shown.len(), 2, "two tool calls were answered: {shown:?}");
    let listed = listed_in(&shown[1]);
    assert_eq!(
        (
            listed.get(&other).map(String::as_str),
            listed.get(&bare).map(String::as_str)
        ),
        (Some("not_asked"), Some("no_device")),
        "where her page is, its device is the one a read acts as; the server's own never is: {}",
        shown[1]
    );
    for page in [own_page, other_page] {
        page.peer.close();
    }
}

/// The same list on a server that keeps people's own workspaces.
#[path = "hosted_across_kept.rs"]
mod kept;
