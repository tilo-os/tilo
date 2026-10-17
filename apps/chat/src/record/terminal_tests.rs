use super::tests::{Talk, id};
use crate::agent_stream::{AgentFinish, AgentOutput, AgentStreamEvent};
use crate::messages::chat_part;
use crate::model::AgentWorkState;
use crate::parts::ChatPart;

#[test]
fn provider_terminal_state_and_accessibility_report_output_not_applied_actions() {
    for (outcome, description) in [
        (AgentFinish::Complete, "Complete"),
        (AgentFinish::Truncated, "Answer interrupted"),
        (AgentFinish::Refused, "Request refused"),
        (AgentFinish::Cancelled, "Stopped"),
        (AgentFinish::Failed, "Could not finish"),
    ] {
        let mut replica = Talk::new();
        for (sequence, output) in [
            (
                1,
                AgentOutput::TextDelta {
                    text: "Retained partial answer 🦀".into(),
                },
            ),
            (
                2,
                AgentOutput::Finished {
                    outcome,
                    detail: "Provider detail".into(),
                },
            ),
        ] {
            replica
                .apply(
                    ChatPart::STREAM_OUTPUT.with(chat_part::StreamOutput {
                        channel: id("c_general"),
                        reply: "terminal_reply".into(),
                        name: None,
                        event: serde_json::to_value(AgentStreamEvent {
                            run: "terminal_run".into(),
                            sequence,
                            event: output,
                        })
                        .unwrap(),
                    }),
                )
                .unwrap();
        }
        let chat = replica.chat.state().unwrap();
        let held = replica.messages();
        let record = held.get(&id("terminal_reply")).unwrap().clone();
        let expected = AgentWorkState::ProviderFinished { outcome };
        assert_eq!(
            record
                .work
                .as_ref()
                .unwrap()
                .state
                .0
                .as_ref()
                .unwrap()
                .state,
            expected
        );
        let reply = chat.message_row(&held, "terminal_reply").unwrap();
        assert_eq!(reply.text, "Retained partial answer 🦀");
        let work = reply.work.unwrap();
        assert_eq!(work.state, expected);
        assert!(work.finished());
        assert!(work.trace.is_empty());
        assert_eq!(
            crate::render_chat::agent_work_accessible_description(&work),
            description
        );
        assert_eq!(
            crate::model::AgentWork::from_update(&crate::testing::work_update(&work))
                .unwrap()
                .state,
            expected
        );

        // A saved record from before terminal states were added still projects
        // its received outcome, without requiring a document migration.
        let mut legacy = record.clone();
        legacy
            .work
            .as_mut()
            .unwrap()
            .state
            .0
            .as_mut()
            .unwrap()
            .state = AgentWorkState::Taken;
        let projected = crate::projection::message_row(&id("terminal_reply"), &legacy);
        assert_eq!(projected.work.unwrap().state, expected);
    }
}
