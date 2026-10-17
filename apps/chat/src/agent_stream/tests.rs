use super::*;
use crate::messages::chat_part;
use crate::parts::ChatPart;
use crate::testing::Room;
use ui::tilo_resource::Id;

fn fixture() -> Room {
    crate::testing::demo_doc()
}

fn event(sequence: u64, output: AgentOutput) -> ui::tilo_resource::Invoke<ChatPart> {
    ChatPart::STREAM_OUTPUT.with(chat_part::StreamOutput {
        channel: Id::from_raw("c_crit"),
        reply: "stream_reply".into(),
        name: None,
        event: serde_json::to_value(AgentStreamEvent {
            run: "run".into(),
            sequence,
            event: output,
        })
        .unwrap(),
    })
}

fn apply(doc: &mut Room, sequence: u64, output: AgentOutput) {
    crate::testing::apply_part_as(doc, crate::model::ME, event(sequence, output)).unwrap();
}

fn message(doc: &Room) -> crate::model::Message {
    doc.message_row("stream_reply").unwrap()
}

fn stream(doc: &Room) -> AgentStream {
    message(doc).work.unwrap().stream.unwrap()
}

#[test]
fn live_events_append_unicode_exactly_once_and_preserve_partial_answer_on_failure() {
    let mut doc = fixture();
    apply(
        &mut doc,
        1,
        AgentOutput::TextDelta {
            text: "Hello 🌍".into(),
        },
    );
    let duplicate = event(
        1,
        AgentOutput::TextDelta {
            text: "Hello 🌍".into(),
        },
    );
    assert!(crate::testing::apply_part_as(&mut doc, crate::model::ME, duplicate).is_err());
    assert!(
        crate::testing::apply_part_as(
            &mut doc,
            crate::model::ME,
            event(3, AgentOutput::TextDelta { text: "gap".into() })
        )
        .is_err()
    );
    apply(
        &mut doc,
        2,
        AgentOutput::ReasoningDelta {
            text: "Provider summary 🦀".into(),
        },
    );
    apply(
        &mut doc,
        3,
        AgentOutput::TextDelta {
            text: " — retained".into(),
        },
    );
    apply(
        &mut doc,
        4,
        AgentOutput::Finished {
            outcome: AgentFinish::Failed,
            detail: "Connection lost".into(),
        },
    );
    // What a part keeps is what loads back.
    let loaded = crate::messages::Messages::single(crate::messages::read_part(
        &automerge::AutoCommit::load(&doc.part_bytes(0).unwrap()).unwrap(),
    ));
    let reply = crate::projection::message_row(
        &Id::from_raw("stream_reply"),
        loaded.get(&Id::from_raw("stream_reply")).unwrap(),
    );
    let kept = reply.work.as_ref().unwrap().stream.clone().unwrap();
    assert_eq!(reply.text, "Hello 🌍 — retained");
    assert_eq!(kept.reasoning, "Provider summary 🦀");
    assert_eq!(kept.finish, Some(AgentFinish::Failed));
    assert_eq!(kept.detail, "Connection lost");
}

#[test]
fn terminal_outcomes_remain_distinct_and_a_retry_cannot_reuse_a_reply() {
    for finish in [
        AgentFinish::Complete,
        AgentFinish::Truncated,
        AgentFinish::Refused,
        AgentFinish::Cancelled,
        AgentFinish::Failed,
    ] {
        let mut doc = fixture();
        apply(
            &mut doc,
            1,
            AgentOutput::TextDelta {
                text: "partial".into(),
            },
        );
        apply(
            &mut doc,
            2,
            AgentOutput::Finished {
                outcome: finish,
                detail: String::new(),
            },
        );
        assert!(
            crate::testing::apply_part_as(
                &mut doc,
                crate::model::ME,
                event(
                    3,
                    AgentOutput::TextDelta {
                        text: "must not append".into()
                    }
                )
            )
            .is_err()
        );
        let mut retry = AgentStreamEvent {
            run: "retry".into(),
            sequence: 1,
            event: AgentOutput::TextDelta {
                text: "must not replace".into(),
            },
        };
        assert!(!stream(&doc).accepts(&retry));
        retry.run = "run".into();
        assert!(!stream(&doc).accepts(&retry));
        assert_eq!(stream(&doc).finish, Some(finish));
        assert_eq!(message(&doc).text, "partial");
    }
}

#[test]
fn only_the_reply_author_can_append_and_rejected_output_creates_no_message() {
    let mut doc = fixture();
    assert!(
        crate::testing::apply_part_as(
            &mut doc,
            crate::model::ME,
            event(
                2,
                AgentOutput::Progress {
                    label: "gap".into()
                }
            )
        )
        .is_err()
    );
    assert!(doc.message_row("stream_reply").is_none());
    apply(
        &mut doc,
        1,
        AgentOutput::TextDelta {
            text: "owned".into(),
        },
    );
    assert!(
        crate::testing::apply_part_as(
            &mut doc,
            "u_aria",
            event(
                2,
                AgentOutput::TextDelta {
                    text: "forged".into()
                }
            )
        )
        .is_err()
    );
    assert_eq!(message(&doc).text, "owned");
}

#[test]
fn reasoning_history_grows_linearly_with_received_content() {
    let mut doc = fixture();
    let part = |doc: &Room| doc.part_bytes(0).unwrap();
    let baseline = part(&doc).len();
    let mut expected = String::new();
    let mut random = 7_u64;
    let mut halfway = 0;
    for sequence in 1..=192 {
        // Incompressible across chunks: repeated prose can conceal snapshot rewrites.
        let chunk: String = (0..1024)
            .map(|_| {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                char::from(b' ' + ((random >> 32) % 95) as u8)
            })
            .collect();
        expected.push_str(&chunk);
        apply(
            &mut doc,
            sequence,
            AgentOutput::ReasoningDelta { text: chunk },
        );
        if sequence == 96 {
            halfway = part(&doc).len() - baseline;
        }
    }
    let saved = part(&doc);
    let growth = saved.len() - baseline;
    assert!(
        growth < expected.len() * 4,
        "{growth} bytes for {} content bytes",
        expected.len()
    );
    assert!(
        growth < halfway * 3,
        "doubling content grew history from {halfway} to {growth}"
    );
    let loaded = crate::messages::Messages::single(crate::messages::read_part(
        &automerge::AutoCommit::load(&saved).unwrap(),
    ));
    let reply = loaded.get(&Id::from_raw("stream_reply")).unwrap();
    let kept = reply.work.as_ref().unwrap().stream.as_ref().unwrap();
    assert_eq!(kept.reasoning, expected);
    assert_eq!(kept.sequence, 192);
}

#[test]
fn work_updates_do_not_erase_provider_stream_or_trace() {
    let mut doc = fixture();
    let summons = Id::from_raw("m_concept");
    let reply = crate::record::reply_id(&summons, crate::model::ME);
    let output = |sequence, event| {
        ChatPart::STREAM_OUTPUT.with(chat_part::StreamOutput {
            channel: Id::from_raw("c_crit"),
            reply: reply.to_string(),
            name: None,
            event: serde_json::to_value(AgentStreamEvent {
                run: "interleaved".into(),
                sequence,
                event,
            })
            .unwrap(),
        })
    };
    crate::testing::apply_part_as(
        &mut doc,
        crate::model::ME,
        output(
            1,
            AgentOutput::ReasoningDelta {
                text: "Preserved reasoning".into(),
            },
        ),
    )
    .unwrap();
    crate::testing::apply_part_as(
        &mut doc,
        crate::model::ME,
        ChatPart::RECORD_WORK.with(chat_part::RecordWork {
            message: summons,
            work: crate::record::WorkUpdate {
                state: "doing".into(),
                app: None,
                did: Some("Read source".into()),
                about: Some("Evidence".into()),
                route: None,
                why: None,
                trace: vec![crate::record::StepUpdate {
                    id: "step".into(),
                    sequence: 0,
                    resource: "notes".into(),
                    label: "Read".into(),
                    outcome: "applied".into(),
                    why: None,
                }],
            },
            say: None,
        }),
    )
    .unwrap();
    crate::testing::apply_part_as(
        &mut doc,
        crate::model::ME,
        output(
            2,
            AgentOutput::TextDelta {
                text: "Answer".into(),
            },
        ),
    )
    .unwrap();
    let message = doc.message_row(reply.as_str()).unwrap();
    let work = message.work.unwrap();
    assert_eq!(work.stream.unwrap().reasoning, "Preserved reasoning");
    assert_eq!(work.trace.len(), 1);
    assert_eq!(work.trace[0].label, "Read");
    assert_eq!(message.text, "Answer");
}

#[test]
fn rendering_discloses_only_provider_reasoning_and_preserves_public_work_card() {
    let mut doc = fixture();
    apply(
        &mut doc,
        1,
        AgentOutput::ReasoningDelta {
            text: "Provider summary".into(),
        },
    );
    apply(
        &mut doc,
        2,
        AgentOutput::TextDelta {
            text: "Answer".into(),
        },
    );
    let mut app = crate::ChatApp::new_ephemeral();
    app.attach_room(&doc);
    app.reload(&doc);
    let message = message(&doc);
    let collapsed = app.agent_display_blocks(&message);
    assert!(
        !collapsed
            .iter()
            .any(|block| block.text.contains("Provider summary"))
    );
    app.expanded_reasoning.insert(message.id.clone());
    let expanded = app.agent_display_blocks(&message);
    assert!(
        expanded
            .iter()
            .any(|block| block.text.contains("Provider summary"))
    );
    assert_eq!(
        message.work.as_ref().unwrap().state,
        crate::model::AgentWorkState::Taken
    );
}

#[test]
fn stream_fixtures_are_durable_and_long_thread_uses_the_shared_scroll_service() {
    use ui::framework::testing::TestApp;
    for scenario in [
        "agent-streaming",
        "agent-failed",
        "agent-reasoning",
        "agent-long-thread",
    ] {
        let mut app = TestApp::<crate::ChatShell>::capture(Some(scenario)).unwrap();
        let state = app.state();
        let held = app
            .with_surface::<crate::ChatApp, _>(|surface| surface.held_messages())
            .unwrap();
        let message = state.message_row(&held, "shot_agent_stream").unwrap();
        assert!(message.work.as_ref().unwrap().stream.is_some());
        app.with_surface::<crate::ChatApp, _>(|surface| {
            assert_eq!(
                surface.expanded_reasoning.contains(&message.id),
                matches!(scenario, "agent-reasoning" | "agent-long-thread")
            );
            if scenario == "agent-long-thread" {
                assert!(surface.thread_scroll.max_offset_y() > 0.0);
                let max = surface.thread_scroll.max_offset_y();
                surface.thread_scroll.set_offset_y(max);
                assert_eq!(surface.thread_scroll.offset_y(), max);
            }
        })
        .unwrap();
    }
}
