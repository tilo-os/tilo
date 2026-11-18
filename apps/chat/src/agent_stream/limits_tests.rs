use super::*;
use crate::messages::chat_messages;
use crate::parts::ChatMessages;
use ui::tilo_resource::Id;

#[test]
fn nonempty_replies_cannot_be_reused_and_oversized_output_creates_no_reply() {
    let mut doc = crate::testing::demo_doc();
    let reply = crate::testing::say(
        &mut doc,
        crate::model::ME,
        "c_crit",
        "Keep the existing answer",
    );
    for (id, text) in [
        (reply.as_str(), "replacement".to_owned()),
        (
            "oversized_reply",
            "x".repeat(crate::chat::MAX_MESSAGE_TEXT_BYTES + 1),
        ),
    ] {
        let invoke = ChatMessages::STREAM_OUTPUT.with(chat_messages::StreamOutput {
            channel: Id::from_raw("c_crit"),
            reply: id.into(),
            name: None,
            event: serde_json::to_value(AgentStreamEvent {
                run: "new_run".into(),
                sequence: 1,
                event: AgentOutput::TextDelta { text },
            })
            .unwrap(),
        });
        assert!(crate::testing::apply_part_as(&mut doc, crate::model::ME, invoke).is_err());
    }
    assert_eq!(
        doc.message_row(&reply).unwrap().text,
        "Keep the existing answer"
    );
    assert!(doc.message_row("oversized_reply").is_none());
}
