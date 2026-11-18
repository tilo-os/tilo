use super::*;
use crate::messages::chat_messages;
use crate::parts::ChatMessages;
use crate::testing::MountedChat;
use crate::testing::Workspace;
use ui::tilo_resource::Id;
use ui::tilo_resource::MemberId;

fn fixture() -> Workspace {
    crate::testing::demo_doc()
}

fn event(sequence: u64, output: AgentOutput) -> ui::tilo_resource::Invoke<ChatMessages> {
    ChatMessages::STREAM_OUTPUT.with(chat_messages::StreamOutput {
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

fn apply(doc: &mut Workspace, sequence: u64, output: AgentOutput) {
    crate::testing::apply_part_as(doc, crate::model::ME, event(sequence, output)).unwrap();
}

fn message(doc: &Workspace) -> crate::model::Message {
    doc.message_row("stream_reply").unwrap()
}

fn stream(doc: &Workspace) -> AgentStream {
    message(doc).work.unwrap().stream.unwrap()
}

#[test]
fn reasoning_history_grows_linearly_with_received_content() {
    let mut doc = fixture();
    let part = |doc: &Workspace| doc.part_bytes(0).unwrap();
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

/// The bytes the newest change of the part saved as `saved` carries.
fn newest_change_bytes(saved: &[u8]) -> usize {
    let mut loaded = automerge::AutoCommit::load(saved).unwrap();
    loaded
        .get_changes(&[])
        .last()
        .map_or(0, |change| change.raw_bytes().len())
}

/// A reply that grows a piece at a time is stored as the pieces it received: the sixtieth
/// piece's change carries what the first's did, not the fifty-nine before it once more.
#[test]
fn a_growing_replys_history_grows_linearly_with_received_content() {
    let mut doc = fixture();
    let part = |doc: &Workspace| doc.part_bytes(0).unwrap();
    let baseline = part(&doc).len();
    let mut expected = String::new();
    let mut random = 11_u64;
    let mut halfway = 0;
    let mut first = 0;
    // Sixty pieces of a thousand bytes: an answer just under what one message holds.
    for sequence in 1..=60 {
        // Incompressible across pieces: repeated prose can conceal a text written whole again.
        let piece: String = (0..1000)
            .map(|_| {
                random = random
                    .wrapping_mul(6_364_136_223_846_793_005)
                    .wrapping_add(1);
                char::from(b' ' + ((random >> 32) % 95) as u8)
            })
            .collect();
        expected.push_str(&piece);
        apply(&mut doc, sequence, AgentOutput::TextDelta { text: piece });
        if sequence == 1 {
            first = newest_change_bytes(&part(&doc));
        }
        if sequence == 30 {
            halfway = part(&doc).len() - baseline;
        }
    }
    let saved = part(&doc);
    let growth = saved.len() - baseline;
    let mut loaded = automerge::AutoCommit::load(&saved).unwrap();
    let changes = loaded.get_changes(&[]);
    let last = changes.last().unwrap();
    eprintln!(
        "sixty pieces of 1,000 bytes grew the part by {growth} bytes ({halfway} after thirty); \
         the last piece's change is {} bytes, of which its message {} bytes, in {} operations",
        last.raw_bytes().len(),
        last.message().map_or(0, str::len),
        last.len()
    );
    assert!(
        last.raw_bytes().len() <= first * 2,
        "a piece's change carries the piece, not the reply so far: the first piece's change is \
         {first} bytes, the sixtieth's {}",
        last.raw_bytes().len()
    );
    assert!(
        growth < expected.len() * 4,
        "{growth} bytes for {} content bytes",
        expected.len()
    );
    assert!(
        growth < halfway * 3,
        "doubling content grew history from {halfway} to {growth}"
    );
    assert_eq!(message(&doc).text, expected);
}

tilo_test_env::scenario! {
    /// What a streamed reply resources of its events.
    /// Each case was a test of its own, on a fixture of its own; a failure
    /// names the cases that failed.
    fn a_reply_stream_keeps_its_text_work_and_outcome;

    phase live_events_append_unicode_exactly_once_and_preserve_partial_answer_on_failure;
    {
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

    phase terminal_outcomes_remain_distinct_and_a_retry_cannot_reuse_a_reply;
    {
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

    phase work_updates_do_not_erase_provider_stream_or_trace;
    {
        let mut doc = fixture();
        let summons = Id::from_raw("m_concept");
        let reply = crate::chat::reply_id(&summons, &MemberId::person(crate::model::ME));
        let output = |sequence, event| {
            ChatMessages::STREAM_OUTPUT.with(chat_messages::StreamOutput {
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
            ChatMessages::RECORD_WORK.with(chat_messages::RecordWork {
                message: summons,
                work: crate::chat::WorkUpdate {
                    state: "doing".into(),
                    app: None,
                    did: Some("Read source".into()),
                    about: Some("Evidence".into()),
                    trace: vec![crate::chat::StepUpdate {
                        id: "step".into(),
                        sequence: 0,
                        resource: "notes".into(),
                        label: "Read".into(),
                        outcome: "applied".into(),
                        why: None,
                    }],
                    detail: None,
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

    phase only_the_reply_author_can_append_and_rejected_output_creates_no_message;
    {
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
    app.attach_workspace(&doc);
    app.reload(&doc);
    let message = message(&doc);
    let collapsed = app.agent_display_blocks(&message);
    assert!(
        !collapsed
            .iter()
            .any(|block| block.text.contains("Provider summary"))
    );
    app.expanded_agent.insert(message.id.clone());
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
        let held = app.with_chat(|surface| surface.held_messages()).unwrap();
        let message = state.message_row(&held, "shot_agent_stream").unwrap();
        assert!(message.work.as_ref().unwrap().stream.is_some());
        app.with_chat(|surface| {
            assert_eq!(
                surface.expanded_agent.contains(&message.id),
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
