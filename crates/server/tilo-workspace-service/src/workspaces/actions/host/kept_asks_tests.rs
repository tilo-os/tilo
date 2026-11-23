//! What the server keeps of an agent's ask while its person is asked: kept
//! once, bounded, claimed by one reader of the answer, found started after
//! a restart, and gone once forgotten.

use serde_json::json;
use tilo_actions::remote::person_asking::Words;

use crate::workspaces::person_asking::kept_asks::{
    CARRIED_OUT, MAX_OPEN_ASKS, NOTHING_WAITS, NewAsk, TOO_MANY_WAIT,
};
use crate::workspaces::tests::{governor, test_host};

const ADA: [u8; 32] = [7; 32];
const BO: [u8; 32] = [9; 32];

/// The job `number` an agent asks for, in `workspace`.
fn asking(workspace: &str, number: usize) -> NewAsk {
    NewAsk {
        workspace: workspace.to_owned(),
        agent: "scout".into(),
        name: "Scout".into(),
        carrier: "action".into(),
        kept: json!({"job": number}),
        always: None,
        words: Words::default(),
        asked_in: json!({"conversation": "c_general"}),
    }
}

/// An ask is kept until it is forgotten, the same asked again adds none, and
/// a person has a bounded number waiting: one more is refused, another
/// person's are their own.
#[tokio::test]
async fn an_ask_is_kept_once_and_a_person_has_a_bounded_number_waiting() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();

    let first = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    assert_eq!(first.ask.len(), 32);
    assert_eq!(first.person, hex::encode(ADA));
    assert!(!first.started);
    assert_eq!(
        workspaces.keep_ask(&ADA, asking("w", 0)).unwrap(),
        None,
        "asked the same again while nobody answered, the one question stands"
    );
    assert_eq!(workspaces.open_asks("w"), vec![first.clone()]);
    assert_eq!(
        workspaces.open_asks("another"),
        Vec::new(),
        "an ask is its workspace's"
    );

    for number in 1..MAX_OPEN_ASKS {
        workspaces
            .keep_ask(&ADA, asking("w", number))
            .unwrap()
            .expect("kept");
    }
    assert_eq!(
        workspaces.keep_ask(&ADA, asking("w", MAX_OPEN_ASKS)),
        Err(TOO_MANY_WAIT.to_owned())
    );
    let his = workspaces
        .keep_ask(&BO, asking("w", 0))
        .unwrap()
        .expect("another person's asks are their own");
    assert_eq!(workspaces.open_asks("w").len(), MAX_OPEN_ASKS + 1);

    // Forgotten, it does nothing any more and leaves a place free.
    workspaces.forget_ask(&first);
    assert_eq!(
        workspaces.claim_ask(&first.ask),
        Err(NOTHING_WAITS.to_owned())
    );
    workspaces
        .keep_ask(&ADA, asking("w", MAX_OPEN_ASKS))
        .unwrap()
        .expect("there is a place again");
    workspaces.forget_ask(&his);
    assert_eq!(workspaces.open_asks("w").len(), MAX_OPEN_ASKS);
    server.shutdown().await;
}

/// One reader of an answer claims its ask and every other is refused, also
/// after the server restarted: an ask claimed before the restart is read back
/// as interrupted and is not claimed again, one that only waited still
/// waits, and one forgotten is gone.
#[tokio::test]
async fn an_answer_is_claimed_once_also_across_a_restart_of_the_server() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let waiting = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    let answered = workspaces
        .keep_ask(&ADA, asking("w", 1))
        .unwrap()
        .expect("kept");
    let gone = workspaces
        .keep_ask(&ADA, asking("w", 2))
        .unwrap()
        .expect("kept");

    let claimed = workspaces
        .claim_ask(&answered.ask)
        .expect("the first reader");
    assert!(claimed.started);
    assert_eq!(claimed.kept, answered.kept, "what is done is what was kept");
    assert_eq!(
        workspaces.claim_ask(&answered.ask),
        Err(CARRIED_OUT.to_owned()),
        "a second reader of the same answer does nothing"
    );
    assert_eq!(
        workspaces.claim_ask("0123456789abcdef0123456789abcdef"),
        Err(NOTHING_WAITS.to_owned())
    );
    assert_eq!(
        workspaces.claim_ask("../../people"),
        Err(NOTHING_WAITS.to_owned())
    );
    workspaces.forget_ask(&gone);
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = workspaces.open_asks("w");
    assert_eq!(kept.len(), 2, "{kept:?}");
    let still_waiting = kept.iter().find(|ask| ask.ask == waiting.ask).unwrap();
    assert!(!still_waiting.started && !still_waiting.interrupted);
    let was_running = kept.iter().find(|ask| ask.ask == answered.ask).unwrap();
    assert!(
        was_running.started && was_running.interrupted,
        "the server stopped while its answer was carried out: {was_running:?}"
    );
    assert_eq!(
        workspaces.claim_ask(&answered.ask),
        Err(CARRIED_OUT.to_owned()),
        "it is not carried out a second time"
    );
    assert!(workspaces.claim_ask(&waiting.ask).is_ok());
    // Stored under the workspace store, never under people/<person>/, and
    // readable only by the person asked and the asking agent.
    assert!(
        !dir.path().join("people").exists(),
        "pending asks must not be stored under people/"
    );
    assert_eq!(workspaces.open_asks_for("w", &ADA, Some("scout")).len(), 2);
    assert!(
        workspaces
            .open_asks_for("w", &[0x99; 32], Some("scout"))
            .is_empty(),
        "another person cannot read pending asks"
    );
    assert!(
        workspaces
            .open_asks_for("w", &ADA, Some("other"))
            .is_empty(),
        "another agent cannot read pending asks"
    );
    server.shutdown().await;
}

/// A claim given back before anything of the answer was carried out leaves the answer waiting:
/// the next reader claims it, also after the server restarted, and it is not read back as
/// interrupted, so the person's answer is carried out once all the same.
#[tokio::test]
async fn an_answer_whose_claim_is_given_back_is_claimed_again_once() {
    let dir = tempfile::tempdir().unwrap();
    let (key, owner) = governor();
    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let answered = workspaces
        .keep_ask(&ADA, asking("w", 0))
        .unwrap()
        .expect("kept");
    let claimed = workspaces
        .claim_ask(&answered.ask)
        .expect("the first reader");
    workspaces.release_ask(&claimed).expect("given back");
    let again = workspaces
        .claim_ask(&answered.ask)
        .expect("the next reader claims it");
    workspaces.release_ask(&again).expect("given back again");
    server.shutdown().await;

    let server = test_host(dir.path(), &key, &owner).await;
    let workspaces = server.workspaces();
    let kept = workspaces.open_asks("w");
    assert_eq!(kept.len(), 1, "{kept:?}");
    assert!(
        !kept[0].started && !kept[0].interrupted,
        "nothing of it was carried out: {:?}",
        kept[0]
    );
    assert!(workspaces.claim_ask(&answered.ask).is_ok());
    assert_eq!(
        workspaces.claim_ask(&answered.ask),
        Err(CARRIED_OUT.to_owned()),
        "claimed again, it is still one reader's"
    );
    server.shutdown().await;
}
