use super::*;
use crate::{record::chat, resource::Chat};
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
            "x".repeat(crate::record::MAX_MESSAGE_TEXT_BYTES + 1),
        ),
    ] {
        let invoke = Chat::STREAM_OUTPUT.with(chat::StreamOutput {
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
        assert!(crate::testing::apply_as(&mut doc, crate::model::ME, invoke).is_err());
    }
    let state = crate::projection::read(&doc);
    assert_eq!(
        state.message_row(&reply).unwrap().text,
        "Keep the existing answer"
    );
    assert!(state.message_row("oversized_reply").is_none());
}
