//! Chat's share of what the system does for every app: a person's card, opened where a person
//! shows (a message's author, the workspace's people), and a problem report's recording.
//! The system's own words show as their ids here (`tilolang-person-moderator`): no words are given.

use crate::fixtures::{
    chat_workspace, open_at as open, person, rich, rows, target, texts_of as texts,
};
use tilolang_runtime::blob::{BlobHost, FileRequest, FileValue, SaveRequest, Saved};
use tilolang_runtime::{InvokeOutcome, UiNode, Value};

const APP: &str = "so.tilo.chat";

/// The device's content store: a file is kept under its own name, and what it holds is the
/// recording a report carries.
struct Device;

impl BlobHost for Device {
    fn read(&self, _hash: &str) -> Option<Vec<u8>> {
        Some(b"a recording".to_vec())
    }

    fn choose(&self, _request: &FileRequest) -> Result<Vec<FileValue>, String> {
        Ok(Vec::new())
    }

    fn save(&self, _request: &SaveRequest, _content: &[u8]) -> Result<Saved, String> {
        Err("this test writes no files out".to_owned())
    }

    fn keep(&self, name: &str, media: &str, bytes: &[u8]) -> Result<FileValue, String> {
        Ok(FileValue::new(
            name,
            media,
            i64::try_from(bytes.len()).unwrap_or(i64::MAX),
            "cd".repeat(32),
        ))
    }
}

/// The card a person's popover holds in `node`, by the popover's label.
fn card<'a>(node: &'a UiNode, label: &str) -> Option<&'a UiNode> {
    node.of_kind("ui.PopoverButton")
        .into_iter()
        .find(|b| b.props.get("label") == Some(&Value::text(label)))
}

/// The key of the button in `node` labelled `label`.
fn button(node: &UiNode, label: &str) -> Option<String> {
    node.of_kind("ui.Button")
        .into_iter()
        .find(|b| b.props.get("label") == Some(&Value::text(label)))
        .map(|b| b.key.clone())
}

/// Choosing a message's author opens their card: their face and name, what they are in the
/// workspace, and what Chat does with someone: writing to them, reporting them and, for a
/// moderator, banning them. Message on the card opens the conversation with them; the card of the
/// person looking offers none of these.
#[test]
fn a_messages_author_opens_their_card_with_their_role_and_what_to_do_with_them() {
    let ada = chat_workspace("ada");
    let root = ada.root(APP);
    let all = Value::resource(&root);
    let design = ada.call_as("ada", APP, "new_channel", vec![all, Value::text("design")]);
    ada.commit();
    ada.call_as(
        "ben",
        APP,
        "send",
        vec![design.clone(), rich("Shipping at four.")],
    );
    ada.call_as("ada", APP, "send", vec![design.clone(), rich("Thanks.")]);
    ada.commit();

    let mut channel = open(&ada, &target(&design));
    let shown = rows(&mut channel);
    assert_eq!(shown.len(), 2, "two messages");
    let ben_card = card(&shown[0], "About ben").expect("Ben's message opens his card");
    let said = texts(ben_card);
    for part in [
        "ben",
        "tilolang-person-moderator",
        "Message",
        "Report person",
        "Ban",
    ] {
        assert!(
            said.contains(&part.to_owned()),
            "{part} on Ben's card: {said:?}"
        );
    }
    let own = card(&shown[1], "About ada").expect("Ada's message opens her card");
    let said = texts(own);
    assert!(
        said.contains(&"tilolang-person-owner".to_owned()),
        "{said:?}"
    );
    assert!(
        button(own, "Message").is_none() && !said.contains(&"Report person".to_owned()),
        "nothing to write to or report about oneself: {said:?}"
    );

    // Message on Ben's card opens the one conversation with him.
    let message = button(ben_card, "Message").unwrap();
    let opened = channel.press(&message).unwrap();
    assert!(matches!(opened, InvokeOutcome::Done(_)), "{opened:?}");
    ada.commit();
    let with_ben = ada.call_as(
        "ada",
        APP,
        "conversation_with",
        vec![Value::resource(&root), Value::Actor(person("ben"))],
    );
    assert!(
        matches!(with_ben, Value::Opt(Some(_))),
        "Message on the card opened the conversation with Ben: {with_ben:?}"
    );
}

/// The workspace's people page opens each person's card, which says what each is here.
#[test]
fn the_people_page_opens_each_persons_card() {
    let ada = chat_workspace("ada");
    let root = ada.root(APP);
    let mut people = ada
        .host
        .open_resource(&root, Some(&ada.view_id(APP, "people")))
        .unwrap();
    people.poll_patches().unwrap();
    for (who, standing) in [
        ("ada", "tilolang-person-owner"),
        ("ben", "tilolang-person-moderator"),
        ("cy", "tilolang-person-member"),
    ] {
        let shown =
            card(people.tree(), &format!("About {who}")).unwrap_or_else(|| panic!("{who}'s card"));
        let said = texts(shown);
        assert!(
            said.contains(&standing.to_owned()),
            "{who} is {standing}: {said:?}"
        );
    }
}

/// A problem report carries a recording of what happened, and its row in #feedback offers to
/// replay it; what is no recording is refused, so a report never claims one it lacks.
#[test]
fn a_problem_report_carries_its_recording_and_its_row_replays_it() {
    let ada = chat_workspace("ada");
    let root = ada.root(APP);
    // The host says what it keeps of the workspace's content, as every host does: here, all of it.
    ada.host
        .give_content(&tilolang_tilo::content::WorkspaceContent {
            file_bytes: 50 * 1_048_576,
            member_bytes: 250 * 1_048_576,
            hourly_bytes: 250 * 1_048_576,
            refusals: Vec::new(),
            host_person: None,
        });
    ada.host.runtime().set_blobs(Device);
    let all = Value::resource(&root);
    let video = |name: &str, media: &str| {
        Value::file(FileValue::new(name, media, 4 * 1_048_576, "cd".repeat(32)))
    };
    let meta = ada.host.package(APP).unwrap().meta;
    let refused = ada.host.runtime().invoke_action(
        &person("ben"),
        (
            crate::fixtures::action_id(&meta, "report_problem").as_str(),
            vec![
                all.clone(),
                rich("The sidebar froze."),
                Value::some(video("notes.pdf", "application/pdf")),
            ],
        ),
    );
    assert!(
        format!("{refused:?}").contains("A recording is a video"),
        "{refused:?}"
    );
    ada.call_as(
        "ben",
        APP,
        "report_problem",
        vec![
            all,
            rich("The sidebar froze."),
            Value::some(video("Replay.mp4", "video/mp4")),
        ],
    );
    ada.commit();

    let feedback = match ada.read(&ada.root(APP), "channels") {
        Value::List(items) => items.iter().map(target).next().expect("#feedback"),
        other => panic!("the app's channels: {other:?}"),
    };
    let mut channel = open(&ada, &feedback);
    let shown = rows(&mut channel);
    assert_eq!(shown.len(), 1, "the one report");
    let said = texts(&shown[0]);
    assert!(said.contains(&"The sidebar froze.".to_owned()), "{said:?}");
    let replay = button(&shown[0], "Replay Replay.mp4")
        .unwrap_or_else(|| panic!("the report's row offers its replay: {said:?}"));
    let played = channel.press(&replay).unwrap();
    assert!(
        matches!(played, InvokeOutcome::Done(_)),
        "the recording opens: {played:?}"
    );
}
