//! The pictures a planning conversation shows: a look's picture after the
//! tool results, none to a model that does not see, `look_content` only at
//! an address the conversation named, and a re-plan's newest look in its
//! first turn.

use super::tests::{Looks, called, look_context, pictures, results, scripted, text_of, tool_names};
use crate::READ_VIEW;
use crate::planner::Planner as _;
use crate::vision::LOOK_CONTENT;
use tilo_model_gateway::host_backend::protocol::{Content, Message, ModelRequest, Role};

/// The first user message `request` sends: the first turn, after the
/// instructions and the apps' reference.
pub(crate) fn first_user(request: &ModelRequest) -> Message {
    request
        .messages
        .iter()
        .find(|message| message.role == Role::User)
        .cloned()
        .unwrap()
}

fn look_read() -> serde_json::Value {
    serde_json::json!({"operation": "canvas.artwork#look", "arguments": {}})
}

fn draw_plan() -> serde_json::Value {
    serde_json::json!({"steps": [{"operation": "canvas.artwork#draw",
        "arguments": {"marks": []}}], "then": ""})
}

/// A tool result takes text alone: a look's picture follows the tool
/// results in one user message, which names it first.
#[test]
fn a_look_read_shows_its_picture_after_the_tool_results() {
    let (mut planner, sent) = scripted(
        vec![
            called("r1", READ_VIEW, &look_read()),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        true,
    );
    let mut reads = Looks::new('a');
    planner
        .plan_reading("look, then draw", &look_context(), &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    assert!(tool_names(&requests[0]).contains(&LOOK_CONTENT.to_owned()));
    let messages = &requests[1].messages;
    let tool = messages
        .iter()
        .position(|message| message.role == Role::Tool)
        .unwrap();
    assert!(results(&requests[1])[0].contains("picture shown below, 8×6 PNG"));
    let after = &messages[tool + 1];
    assert_eq!(after.role, Role::User);
    assert!(text_of(after).starts_with("The pictures the calls above showed"));
    assert_eq!(pictures(after), 1);
}

/// A model that is not shown pictures is not offered `look_content`, reads
/// that a look's picture was not shown, and nothing is fetched.
#[test]
fn a_model_that_does_not_see_gets_no_look_content_and_is_told_so() {
    let (mut planner, sent) = scripted(
        vec![
            called("r1", READ_VIEW, &look_read()),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        false,
    );
    let mut reads = Looks::new('a');
    planner
        .plan_reading("look, then draw", &look_context(), &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    assert!(!tool_names(&requests[0]).contains(&LOOK_CONTENT.to_owned()));
    let answered = &results(&requests[1])[0];
    assert!(
        answered.contains(&reads.rendered) && answered.contains("picture not shown"),
        "{answered}"
    );
    assert_eq!(
        requests[1].messages.len(),
        5,
        "the instructions, the reference, the turn, its call and the answer; no pictures message"
    );
    assert!(reads.looked.is_empty(), "nothing fetched for a blind model");
    assert!(
        requests[1]
            .messages
            .iter()
            .all(|message| pictures(message) == 0)
    );
}

/// `look_content` looks only at an address the conversation named, and at
/// a room's own picture only where the agent may show them; a PDF is
/// pointed to pdf-viewer. Allowed, a room's picture is written out again in
/// the confined worker and shown.
#[test]
fn look_content_refuses_unshown_addresses_and_room_pictures_unless_allowed() {
    let mut reads = Looks::new('a');
    let scan = reads.keep('b', crate::vision::tests::png(10, 10));
    let pdf = reads.keep('c', b"%PDF-1.4\n".to_vec());
    let unshown = crate::vision::tests::address('d');
    let look = |address: &str| serde_json::json!({"content": address});
    let (mut planner, sent) = scripted(
        vec![
            called("l1", LOOK_CONTENT, &look(&unshown)),
            called("l2", LOOK_CONTENT, &look(&scan)),
            called("l3", LOOK_CONTENT, &look(&pdf)),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        true,
    );
    let task = format!("trace the scan {scan} onto the canvas; the brief is {pdf}");
    planner
        .plan_reading(&task, &look_context(), &mut reads)
        .unwrap();
    let requests = sent.lock().unwrap().clone();
    let unnamed = &results(&requests[1])[0];
    assert!(
        unnamed.starts_with("Not shown:") && unnamed.contains("was not named"),
        "{unnamed}"
    );
    let room = &results(&requests[2])[0];
    assert!(room.contains("not shown the room's pictures"), "{room}");
    let document = &results(&requests[3])[0];
    assert!(
        document.contains("is a PDF") && document.contains("look-page"),
        "{document}"
    );
    assert!(
        !reads
            .looked
            .contains(&crate::vision::content_hash(&unshown).unwrap()),
        "an unnamed address is never fetched"
    );

    let (mut planner, sent) = scripted(
        vec![
            called("l1", LOOK_CONTENT, &look(&scan)),
            called("p1", tilo_agent::tools::PLAN, &draw_plan()),
        ],
        true,
    );
    let mut context = look_context();
    context.room_pictures = true;
    planner.plan_reading(&task, &context, &mut reads).unwrap();
    let requests = sent.lock().unwrap().clone();
    let shown = &results(&requests[1])[0];
    assert!(shown.contains("10×10 JPEG picture"), "{shown}");
    assert_eq!(pictures(requests[1].messages.last().unwrap()), 1);
}

/// A re-plan's first turn shows the newest picture the task's steps looked
/// at, after the text, and the text says it is shown.
#[test]
fn a_replan_shows_the_newest_looked_at_picture_in_its_first_turn() {
    let (mut planner, sent) = scripted(
        vec![called("p1", tilo_agent::tools::PLAN, &draw_plan())],
        true,
    );
    let mut reads = Looks::new('a');
    let older = reads.keep('e', crate::vision::tests::png(4, 4));
    let mut context = look_context();
    context.request = Some("draw a band, then check it looks right".into());
    context.pictures = vec![older, reads.rendered.clone()];
    planner
        .plan_reading("check it looks right", &context, &mut reads)
        .unwrap();
    let first = first_user(&sent.lock().unwrap()[0]);
    assert_eq!(pictures(&first), 1, "only the newest");
    assert!(
        matches!(first.content.last(), Some(Content::Image { .. })),
        "after the text"
    );
    let text = text_of(&first);
    assert!(
        text.contains(&format!("{} picture shown below, 8×6 PNG", reads.rendered)),
        "{text}"
    );
}
