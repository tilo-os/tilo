//! A removal Undo cannot take back, carried inside another action.
//!
//! With the server, the person and the assistant of the tests this is a part of. The library
//! publishes `apply` and `group`, which carry its other actions. What its person's grants
//! leave to them asks by its own name, whatever carries it: the question says what would be
//! removed and does not say that Undo brings it back. An always is for the action that asked
//! and never for what carried it: a carrier's question offers none, whatever its person
//! answers there lets nothing stand, and an always a server before this kept for a carrier
//! opens nothing. The removal's own always, given where it asked by its own name, lets the
//! removal go ahead inside a carrier too, until its person takes it back.

use super::*;
use tilo_actions::remote::person_asking::Thing;
use tilo_server::workspaces::person_asking::Allowing;

/// What the library calls taking a book off its shelf.
const REMOVAL: &str = "Remove from library";
/// What the assistant closes a turn with.
const CLOSING: &str = "That is all I did.";

/// Taking the book `book` off the shelf, as a carrier holds it.
fn removal_of(book: &str) -> serde_json::Value {
    serde_json::json!({"action": "remove-from-library", "args": {"book": format!("book:{book}")}})
}

/// The arguments with which the library's `carrier` (`apply` or `group`) carries `members`,
/// each an action of the library with its own arguments. An `apply` carries the first.
fn carrying(carrier: &str, members: &[serde_json::Value]) -> serde_json::Value {
    if carrier == "apply" {
        serde_json::json!({"action": {"op": members[0]}})
    } else {
        serde_json::json!({"actions": members})
    }
}

/// One turn in which the assistant makes `calls` and closes.
async fn turn_of(hosted: &Hosted, words: &str, mut calls: Vec<Reply>) {
    calls.push(Reply::text(CLOSING));
    hosted.turn(words, calls).await;
}

/// The assistant shelves the book `book`, the one book on the shelf then, and takes it off
/// again with the removal carried inside the library's `carrier`.
async fn shelves_then_carries_the_removal(hosted: &Hosted, carrier: &str, book: &str) {
    let shelved = super::super::api_questions::shelved(book, book, "");
    turn_of(
        hosted,
        &format!("Put {book} on the shelf, then take it off again."),
        vec![
            does(&format!("{book}-open"), "ebook#open", serde_json::json!({})),
            does(&format!("{book}-shelve"), "ebook#shelve", shelved),
            does(
                &format!("{book}-carried"),
                &format!("ebook/1 book#{carrier}"),
                carrying(carrier, &[removal_of(book)]),
            ),
        ],
    )
    .await;
}

/// Whether the question `held` asks offers its person the answer `answer`.
fn offers(held: &Held, answer: &str) -> bool {
    held.question
        .as_ref()
        .is_some_and(|question| question.answers.iter().any(|choice| choice.id == answer))
}

/// What the question `held` asks shows its person, as JSON.
fn shown(held: &Held) -> serde_json::Value {
    held.question.as_ref().map(detail).unwrap_or_default()
}

/// How the server says the answer to `question` ended.
fn ended(question: &chat::question::Question) -> Option<&str> {
    question
        .outcome
        .as_ref()
        .map(|outcome| outcome.outcome.0.as_str())
}

/// Whether the book titled `title` stands on the shelf of the workspace's library now, as
/// the server keeps the library.
async fn on_the_shelf(hosted: &Hosted, title: &str) -> bool {
    let saved = hosted
        .workspaces
        .resource_document(&hosted.workspace, EBOOK, "")
        .await
        .unwrap()
        .expect("the server keeps the workspace's library");
    ebook::resource::Ebook::read(&automerge::AutoCommit::load(&saved).unwrap())
        .unwrap()
        .entries
        .values()
        .any(|book| book.title == title)
}

/// Its person answers the question `id` with `answer`, the server carries that out, and its
/// asker is told how that ended: the question as it stands then.
async fn answered(hosted: &Hosted, id: &Id, answer: &str) -> chat::question::Question {
    assert!(
        hosted.answers(Who::Herself, id, answer, None).await,
        "its person answers {answer}"
    );
    let settled = hosted.settled(id).await;
    hosted.told_how_it_ended(id).await;
    settled
}

/// What the question about the removal asked by its own name shows an always would cover,
/// with the name `action` in the removal's place: the same agent, workspace, kind and thing.
fn scope_named(shown: &serde_json::Value, action: &str) -> Asked {
    let always = &shown["always"];
    let mut name: ActionName =
        serde_json::from_value(always["action"].clone()).expect("the action an always names");
    action.clone_into(&mut name.action);
    Asked {
        workspace: always["workspace"].as_str().unwrap_or_default().to_owned(),
        agent: always["agent"]["id"]
            .as_str()
            .unwrap_or_default()
            .to_owned(),
        action: name,
        on: serde_json::from_value::<Option<Thing>>(always["on"].clone())
            .expect("the thing an always names"),
    }
}

/// `scope` stands for the person from before, worded `words`: what their server keeps of an
/// always it read, written by the code that reads one.
fn stands_from_before(hosted: &Hosted, scope: Asked, words: &str) {
    let allowing = Allowing {
        scope,
        name: "Scout".to_owned(),
        words: Words {
            action: words.to_owned(),
            on: String::new(),
            place: "Team".to_owned(),
        },
        asked_by: None,
    };
    hosted
        .workspaces
        .let_always(&hosted.acts_for(), allowing)
        .map(drop)
        .expect("the server keeps what stood before");
}

/// The question a removal carried inside `carrier` puts says the removal and the book in its
/// person's words, does not say that Undo takes it back, and offers a yes and a no and no
/// always.
async fn the_question_names_the_removal_inside(carrier: &str) {
    let hosted = Hosted::start().await;
    shelves_then_carries_the_removal(&hosted, carrier, "dusk").await;
    let (_, held) = the_one_question(&hosted).await;
    let shown = shown(&held);
    let draft = &shown["draft"];
    assert!(
        held.text.contains(REMOVAL) && !held.text.contains("Apply"),
        "inside {carrier} the question says what would be done, not what carries it: {:?}",
        held.text
    );
    assert_eq!(
        draft["action_label"], REMOVAL,
        "inside {carrier} its person reviews the removal: {draft}"
    );
    assert!(
        draft["fields"].to_string().contains("book:dusk"),
        "inside {carrier} it names the book: {draft}"
    );
    assert_ne!(
        draft["undoable"], true,
        "inside {carrier} it does not say that Undo takes the removal back: {draft}"
    );
    assert!(
        shown.get("always").is_none() && !offers(&held, chat::question::ALWAYS),
        "inside {carrier} the question offers no always: {shown}"
    );
    assert!(
        offers(&held, chat::question::ALLOW) && offers(&held, chat::question::DENY),
        "inside {carrier} it offers a yes and a no"
    );
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_apply_the_question_names_the_removal_and_offers_no_always() {
    Box::pin(the_question_names_the_removal_inside("apply")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_a_group_the_question_names_the_removal_and_offers_no_always() {
    Box::pin(the_question_names_the_removal_inside("group")).await;
}

/// A removal carried inside `carrier` asks; its person answers the most the question lets
/// them say, always where it is offered. The removal is done once. Nothing stands for the
/// carrier, and the next removal it carries asks its person again, with nothing taken off
/// the shelf before they answer.
async fn the_next_removal_asks_again_inside(carrier: &str) {
    let mut hosted = Hosted::start().await;
    shelves_then_carries_the_removal(&hosted, carrier, "dusk").await;
    let (id, held) = the_one_question(&hosted).await;
    let most = if offers(&held, chat::question::ALWAYS) {
        chat::question::ALWAYS
    } else {
        chat::question::ALLOW
    };
    let settled = answered(&hosted, &id, most).await;
    assert_eq!(
        ended(&settled),
        Some(chat::question::DONE),
        "inside {carrier}, answered {most}: {:?}",
        settled.outcome
    );
    assert!(
        !on_the_shelf(&hosted, "dusk").await,
        "inside {carrier}, answered {most}: the book is taken off"
    );

    shelves_then_carries_the_removal(&hosted, carrier, "dawn").await;
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        2,
        "inside {carrier} the next removal asks its person again, whatever they answered the \
         first ({most}): {}",
        hosted.tool_results()
    );
    assert!(
        asked[1]
            .1
            .question
            .as_ref()
            .is_some_and(chat::question::Question::waiting),
        "inside {carrier} the second question waits for its person"
    );
    assert!(
        on_the_shelf(&hosted, "dawn").await,
        "inside {carrier} nothing is taken off the shelf before its person answered"
    );
    let stands = hosted.permissions().await;
    assert!(
        stands.is_empty(),
        "nothing stands for {carrier}: {stands:?}"
    );
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_apply_the_next_removal_asks_again_whatever_its_person_answered() {
    Box::pin(the_next_removal_asks_again_inside("apply")).await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn inside_a_group_the_next_removal_asks_again_whatever_its_person_answered() {
    Box::pin(the_next_removal_asks_again_inside("group")).await;
}

/// An always a server before this kept for `carrier`, whatever edit it was given for, opens
/// nothing: a removal carried inside it asks its person, the book stays on the shelf, and
/// what stood is used for nothing.
async fn an_always_from_before_opens_nothing_inside(carrier: &str, words: &str) {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let (id, held) = the_one_question(&hosted).await;
    stands_from_before(&hosted, scope_named(&shown(&held), carrier), words);
    assert_eq!(
        hosted.permissions().await.len(),
        1,
        "it stands for {carrier}, as a server before this kept it"
    );
    // Their no to the removal by its own name: the book stays, and nothing of the removal's
    // own stands.
    let settled = answered(&hosted, &id, chat::question::DENY).await;
    assert_eq!(ended(&settled), Some(chat::question::DROPPED));

    turn_of(
        &hosted,
        "Take Dusk off the shelf.",
        vec![
            does("dusk-open-again", "ebook#open", serde_json::json!({})),
            does(
                "dusk-carried",
                &format!("ebook/1 book#{carrier}"),
                carrying(carrier, &[removal_of("dusk")]),
            ),
        ],
    )
    .await;
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        2,
        "an always kept for {carrier} opens no removal inside it: {}",
        hosted.tool_results()
    );
    assert!(
        asked[1]
            .1
            .question
            .as_ref()
            .is_some_and(chat::question::Question::waiting),
        "the removal inside {carrier} waits for its person"
    );
    assert!(
        on_the_shelf(&hosted, "dusk").await,
        "nothing is taken off the shelf by an always kept for {carrier}"
    );
    let stands = hosted.permissions().await;
    assert_eq!(
        stands[0]["usedTimes"], 0,
        "what stood for {carrier} was used for nothing: {stands:?}"
    );
    hosted.page.peer.close();
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_always_kept_for_apply_from_before_opens_no_removal() {
    Box::pin(an_always_from_before_opens_nothing_inside(
        "apply",
        "Apply Books edit",
    ))
    .await;
}

#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_always_kept_for_a_group_from_before_opens_no_removal() {
    Box::pin(an_always_from_before_opens_nothing_inside(
        "group",
        "Apply several Books edits as one",
    ))
    .await;
}

/// The removal's own always, given where it asked by its own name, covers that action and
/// nothing else: the next removal by its own name goes ahead, and so does the removal carried
/// inside `apply` and inside `group`, each counted on the one permission that stands. Taken
/// back by its person, the removal asks again inside a carrier.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn a_removals_own_always_lets_it_go_ahead_inside_a_carrier_until_it_is_taken_back() {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let (id, _) = the_one_question(&hosted).await;
    answered(&hosted, &id, chat::question::ALWAYS).await;
    let stands = hosted.permissions().await;
    assert_eq!(stands.len(), 1, "{stands:?}");
    let one = stands[0].clone();
    assert_eq!(one["action"]["action"], "remove-from-library", "{one}");

    hosted
        .shelves_then_removes("Put Dawn on the shelf, then take it off again.", "dawn")
        .await;
    assert_eq!(
        hosted.questions().await.len(),
        1,
        "by its own name the next removal goes ahead: {}",
        hosted.tool_results()
    );
    assert!(!on_the_shelf(&hosted, "dawn").await);

    for (carrier, book) in [("apply", "noon"), ("group", "dew")] {
        shelves_then_carries_the_removal(&hosted, carrier, book).await;
        assert_eq!(
            hosted.questions().await.len(),
            1,
            "inside {carrier} the removal goes ahead by its own always: {}",
            hosted.tool_results()
        );
        assert!(
            !on_the_shelf(&hosted, book).await,
            "inside {carrier} the book is taken off"
        );
    }
    let stands = hosted.permissions().await;
    assert_eq!(
        stands.len(),
        1,
        "no carrier keeps one of its own: {stands:?}"
    );
    assert_eq!(stands[0]["usedTimes"], 3, "each use is counted: {stands:?}");

    let take_back = &one["takeBack"];
    let left = hosted
        .page
        .invoke_on(
            take_back["provider"].as_str().unwrap(),
            ActionAddress::new(
                take_back["resource"].as_str().unwrap(),
                take_back["action"].as_str().unwrap(),
            ),
            InvokeArgument::Json(take_back["arguments"].as_object().unwrap().clone()),
        )
        .await
        .expect("the person takes back what stood for them")
        .value
        .unwrap_or_default();
    assert_eq!(left["permissions"], serde_json::json!([]), "{left}");
    shelves_then_carries_the_removal(&hosted, "apply", "eve").await;
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        2,
        "taken back, the removal asks again inside a carrier: {}",
        hosted.tool_results()
    );
    assert!(on_the_shelf(&hosted, "eve").await);
    hosted.page.peer.close();
}

/// An always that stands for a harmless edit of the library opens no removal, whatever
/// carries the two: the removal inside `apply` asks, and a group that shelves a book and
/// removes another asks too, by the removal's name and showing both, with neither done before
/// its person answers.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn an_always_for_a_harmless_edit_opens_no_removal_inside_a_carrier() {
    let mut hosted = Hosted::start().await;
    hosted
        .shelves_then_removes("Put Dusk on the shelf, then take it off again.", "dusk")
        .await;
    let (id, held) = the_one_question(&hosted).await;
    stands_from_before(&hosted, scope_named(&shown(&held), "shelve"), "Shelve");
    answered(&hosted, &id, chat::question::DENY).await;

    turn_of(
        &hosted,
        "Take Dusk off the shelf.",
        vec![
            does("dusk-open-again", "ebook#open", serde_json::json!({})),
            does(
                "dusk-carried",
                "ebook/1 book#apply",
                carrying("apply", &[removal_of("dusk")]),
            ),
        ],
    )
    .await;
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        2,
        "the removal inside apply asks: {}",
        hosted.tool_results()
    );
    let second = asked[1].0.clone();
    answered(&hosted, &second, chat::question::DENY).await;

    let shelving = serde_json::json!({
        "action": "shelve",
        "args": super::super::api_questions::shelved("dawn", "dawn", ""),
    });
    turn_of(
        &hosted,
        "Put Dawn on the shelf and take Dusk off it, as one.",
        vec![
            does("both-open", "ebook#open", serde_json::json!({})),
            does(
                "both-carried",
                "ebook/1 book#group",
                carrying("group", &[shelving, removal_of("dusk")]),
            ),
        ],
    )
    .await;
    let asked = hosted.questions().await;
    assert_eq!(
        asked.len(),
        3,
        "a group that shelves and removes asks: {}",
        hosted.tool_results()
    );
    let third = &asked[2].1;
    let shown = shown(third);
    assert!(
        third.text.contains(REMOVAL) && !third.text.contains("Apply"),
        "the group's question says the removal: {:?}",
        third.text
    );
    let fields = shown["draft"]["fields"].to_string();
    assert!(
        fields.contains("book:dusk") && fields.contains("dawn"),
        "and shows its person everything their yes would do: {fields}"
    );
    assert!(
        on_the_shelf(&hosted, "dusk").await && !on_the_shelf(&hosted, "dawn").await,
        "nothing of the group is done before its person answered"
    );
    let stands = hosted.permissions().await;
    assert_eq!(
        stands[0]["usedTimes"], 0,
        "the harmless edit's always was used for nothing: {stands:?}"
    );
    hosted.page.peer.close();
}
