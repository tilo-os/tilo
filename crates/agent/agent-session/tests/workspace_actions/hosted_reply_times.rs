//! What a person's device receives of an answer while its model writes it: a measurement that
//! stays in the tree.
//!
//! A person's provider writes four hundred words, one every 25 ms, as a model streams an
//! answer. The person's device follows the conversation over its session with the server, as
//! a window showing it does, and the test keeps when each change arrived and what the device
//! held then. It asserts what holds on every machine: the device reads along while the model
//! writes, its reply only ever grows, and every word arrives once and in order. What a
//! machine's speed decides is printed, on a pass too (`.config/nextest.toml`): how many
//! changes the words came in, when the first and the last arrived after the model's first and
//! last word, and how many bytes a change adds to what the device keeps of the conversation.
use super::writing::{PACED_EVERY, WORDS, paced, paced_whole};
use super::*;

/// One change of the conversation's part as the device received it.
struct Change {
    /// When it arrived.
    at: Instant,
    /// How many bytes the device kept of the part then.
    kept: usize,
    /// By how many bytes the part grew with it.
    grew: usize,
    /// The reply's words as the device held them then.
    words: String,
}

/// The reply `id` as `kept` holds it: the conversation's part, saved whole.
fn reply_in(kept: &[u8], id: &Id) -> Option<chat::chat::Message> {
    let doc = automerge::AutoCommit::load(kept).ok()?;
    chat::messages::read_part(&doc).messages.get(id).cloned()
}

/// Whether the newest of `kept`, the part as the device kept it after each change, holds the
/// reply `id` reading `answer` with its writing ended.
fn holds_whole(kept: Option<&Vec<([u8; 32], Vec<u8>)>>, id: &Id, answer: &str) -> bool {
    kept.and_then(|kept| kept.last())
        .and_then(|(_, bytes)| reply_in(bytes, id))
        .is_some_and(|reply| {
            reply.text == answer
                && reply
                    .work
                    .as_ref()
                    .and_then(|work| work.stream.as_ref())
                    .is_some_and(|stream| stream.finish.is_some())
        })
}

/// Each change of the part with when it `arrived` and what the device `kept` after it.
fn changes_of(arrived: &[Instant], kept: &[([u8; 32], Vec<u8>)], id: &Id) -> Vec<Change> {
    let mut before = None::<usize>;
    arrived
        .iter()
        .zip(kept)
        .map(|(at, (_, bytes))| {
            let grew = before.map_or(0, |before| bytes.len().saturating_sub(before));
            before = Some(bytes.len());
            Change {
                at: *at,
                kept: bytes.len(),
                grew,
                words: reply_in(bytes, id).map_or_else(String::new, |reply| reply.text),
            }
        })
        .collect()
}

/// The changes that brought words, each with how many bytes of words it brought. The reply at
/// the device only ever grows, by the answer's own words in their order.
fn grown<'a>(changes: &'a [Change], answer: &str) -> Vec<(&'a Change, usize)> {
    let mut held = 0;
    let mut grown = Vec::new();
    for change in changes {
        assert!(
            answer.starts_with(change.words.as_str()),
            "the device holds the answer's words in their order and nothing else: {:?}",
            change.words
        );
        assert!(
            change.words.len() >= held,
            "the reply at the device never loses a word: {} bytes after {held}",
            change.words.len()
        );
        if change.words.len() > held {
            grown.push((change, change.words.len().saturating_sub(held)));
            held = change.words.len();
        }
    }
    grown
}

/// The middle, nine of ten and the most of `values`.
fn spread<T: Ord + Copy + Default + std::fmt::Display>(values: &[T]) -> String {
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let at = |tenths: usize| {
        let index = sorted.len().saturating_sub(1).saturating_mul(tenths) / 10;
        sorted.get(index).copied().unwrap_or_default()
    };
    format!("{} / {} / {}", at(5), at(9), at(10))
}

/// What the device received, in words a log keeps: `grown` are the changes that brought
/// words, and the model wrote its first word at `began` and its last at `ended`.
fn table(grown: &[(&Change, usize)], began: Instant, ended: Instant) -> String {
    let (Some((first, _)), Some((last, _))) = (grown.first(), grown.last()) else {
        return "no change brought a word".to_owned();
    };
    let while_writing = grown
        .iter()
        .filter(|(change, _)| change.at <= ended)
        .count();
    let between: Vec<u128> = grown
        .iter()
        .zip(grown.iter().skip(1))
        .map(|((before, _), (after, _))| after.at.saturating_duration_since(before.at).as_millis())
        .collect();
    let brought: Vec<usize> = grown.iter().map(|(_, bytes)| *bytes).collect();
    let grew: Vec<usize> = grown.iter().map(|(change, _)| change.grew).collect();
    let three = [0, grown.len() / 2, grown.len().saturating_sub(1)]
        .iter()
        .filter_map(|at| grown.get(*at))
        .map(|(change, bytes)| format!("{bytes} bytes of words grew it by {}", change.grew))
        .collect::<Vec<_>>()
        .join("; ");
    let build = if cfg!(debug_assertions) {
        "a build that is not optimised (debug assertions on)"
    } else {
        "an optimised build"
    };
    format!(
        "ANSWER TIMES AT A DEVICE, {build}: {WORDS} words, one every {} ms; the model wrote its \
         last word {} ms after its first.\n\
         The device received them in {} changes, {while_writing} of them while the model was \
         writing: the first {} ms after the model's first word, the last {} ms after its last.\n\
         Between two changes (middle / nine of ten / most): {} ms. Bytes of words a change \
         brought: {}.\n\
         The conversation's part as the device keeps it: {} bytes before the first word, {} \
         after the last; a change that brought words grew it by {} bytes.\n\
         The first, the middle and the last change: {three}.",
        PACED_EVERY.as_millis(),
        ended.saturating_duration_since(began).as_millis(),
        grown.len(),
        first.at.saturating_duration_since(began).as_millis(),
        last.at.saturating_duration_since(ended).as_millis(),
        spread(&between),
        spread(&brought),
        first.kept.saturating_sub(first.grew),
        last.kept,
        spread(&grew),
    )
}

/// Her assistant's model writes four hundred words, one every 25 ms. Her device reads along:
/// the first words are there while the model still writes, the reply only ever grows by the
/// answer's own words, and when the turn has ended it holds every word once and in order.
/// How many changes that took, when they arrived and what they cost is printed.
#[tokio::test(flavor = "multi_thread", worker_threads = 4)]
async fn four_hundred_words_reach_a_device_while_they_are_written() {
    team!(
        root, browser, actions, workspaces, workspace, page, person, served
    );
    let mut page = page;
    let (theirs, _lets_go) = paced(&workspaces, &workspace).await;
    let first = write_to_agent(
        &actions,
        &workspaces,
        &workspace,
        (&root, &browser),
        "say four hundred words",
    )
    .await;
    // The person's device follows the conversation's messages, as a window showing them does.
    let route = DocumentRoute {
        app_id: CHAT.into(),
        cluster_id: page.cluster_id.clone(),
        document: chat::parts::part(0).as_str().to_owned(),
    };
    page.follow_chain(CHAT, chat::parts::chain().chain_head().as_str());
    let agents = worked(&served, &workspace, never_asked(Arc::default())).await;
    let turns = agents.turns(&workspace).await.unwrap();
    assert_eq!(turns.len(), 1, "{turns:?}");
    let (answering, at, turn) = (agents.clone(), workspace.clone(), turns[0].clone());
    let going = tokio::spawn(async move { answering.answer(&at, &turn).await });

    let reply_id = chat::chat::reply_id(&first, &MemberId::agent(SCOUT));
    let answer = paced_whole();
    // When each change of the part arrived at the device, in the order it keeps them.
    let mut arrived = vec![Instant::now(); page.heard(&route)];
    let deadline = tilo_test_env::hang_deadline();
    loop {
        page.pump();
        let heard = page.heard(&route);
        if heard > arrived.len() {
            arrived.resize(heard, Instant::now());
        }
        if going.is_finished() && holds_whole(page.documents.get(&route), &reply_id, &answer) {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the device never held the whole answer with its writing ended"
        );
        tokio::time::sleep(Duration::from_millis(2)).await;
    }
    going.await.unwrap().unwrap();
    let reply = ended(&workspaces, &workspace, &first).await;
    assert_eq!(
        reply.text, answer,
        "every word stands in the reply on the server, once and in order"
    );

    let wrote = theirs.wrote();
    let (Some(began), Some(last_word)) = (wrote.first().copied(), wrote.last().copied()) else {
        panic!("the model wrote nothing");
    };
    let kept = page.documents.get(&route).cloned().unwrap_or_default();
    let changes = changes_of(&arrived, &kept, &reply_id);
    let steps = grown(&changes, &answer);
    eprintln!("{}", table(&steps, began, last_word));
    assert!(
        steps
            .first()
            .is_some_and(|(change, _)| change.at < last_word),
        "the first words are at the device while the model still writes"
    );
    assert!(
        steps.len() > 1,
        "the answer reaches the device in more than one change: {}",
        steps.len()
    );
    assert_eq!(
        changes.last().map(|change| change.words.as_str()),
        Some(answer.as_str()),
        "the device holds every word once and in order when the turn has ended"
    );
    let _ = person;
    page.peer.close();
}
