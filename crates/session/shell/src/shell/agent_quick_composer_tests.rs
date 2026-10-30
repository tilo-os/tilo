use super::*;

fn owner(agent: &str) -> ComposerOwner {
    ComposerOwner {
        room: "room".into(),
        agent: agent.into(),
        person: "certified-person".into(),
        identity_revision: 7,
    }
}

fn open(composer: &mut AgentQuickComposer, owner: ComposerOwner) {
    composer.open(
        owner,
        "Tide",
        AgentQuickComposerOrigin {
            marker: Rect::new(100.0, 100.0, 132.0, 132.0),
            protected: Rect::new(100.0, 100.0, 220.0, 164.0),
            context: "Tide was renaming the release notes".into(),
        },
        Rect::new(0.0, 0.0, 800.0, 600.0),
    );
}

fn action(
    widget: &mut dyn Widget,
    id: &str,
    action: &ui::a11y::Action,
    doc: &mut automerge::AutoCommit,
) -> bool {
    if widget.test_id() == Some(id) {
        return widget.perform_action(
            action,
            &[],
            &mut ui::widget::Edits::lend(doc),
            &automerge::ROOT,
        );
    }
    widget
        .children_mut()
        .into_iter()
        .any(|child| self::action(child, id, action, doc))
}

fn edit(composer: &mut AgentQuickComposer, doc: &mut automerge::AutoCommit, text: &str) {
    Widget::sync(composer, doc, &automerge::ROOT);
    assert!(action(
        composer,
        "agent-quick-editor",
        &ui::a11y::Action::SetValue(text.into()),
        doc
    ));
    Widget::sync(composer, doc, &automerge::ROOT);
}

fn press(composer: &mut AgentQuickComposer, doc: &mut automerge::AutoCommit, id: &str) {
    Widget::sync(composer, doc, &automerge::ROOT);
    assert!(
        action(composer, id, &ui::a11y::Action::Activate, doc),
        "{id} is shown and enabled"
    );
    // A press queues the control's intent; the compositor runs the queue
    // when the input is handled (see `FrameBuilder::dispatch_to_desktop`),
    // and here the press is the whole input.
    ui::widget::controls::start_pending_control_invocations(
        composer,
        &mut ui::widget::Edits::lend(doc),
        &automerge::ROOT,
    );
}

fn submit(composer: &mut AgentQuickComposer, doc: &mut automerge::AutoCommit) -> ComposerRequest {
    press(composer, doc, "agent-quick-send");
    composer
        .take_request()
        .expect("the enabled ordinary button emits one intention")
}

fn shown(composer: &mut AgentQuickComposer, id: &str) -> bool {
    fn find(widget: &dyn Widget, id: &str) -> bool {
        widget.test_id() == Some(id) || widget.children().into_iter().any(|c| find(c, id))
    }
    let doc = automerge::AutoCommit::new();
    Widget::sync(composer, &doc, &automerge::ROOT);
    find(composer, id)
}

fn working(task: &str, phase: RunPhase) -> ComposerRun {
    ComposerRun {
        task: ComposerTask {
            task_id: task.into(),
            run_epoch: 3,
            revision: 8,
            label: "Fix the link".into(),
        },
        phase,
        progress: RunProgress::default(),
    }
}

#[test]
fn one_field_sends_a_request_the_agent_answers_and_a_follow_up_carries_it() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let heads = doc.get_heads();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    assert!(!composer.view.enabled.get(), "nothing to send yet");
    assert!(!composer.view.has_status.get(), "no checking notice");
    assert!(!shown(&mut composer, "agent-quick-stop"));
    edit(&mut composer, &mut doc, "What does idempotent mean?");
    let request = submit(&mut composer, &mut doc);
    assert_eq!(request.intent, ComposerIntent::Request);
    assert_eq!(request.task, None);
    assert_eq!(request.context, "Tide was renaming the release notes");
    assert!(request.conversation.is_empty());
    assert!(
        composer.take_request().is_none(),
        "one request at a time per agent"
    );
    assert_eq!(composer.view.status.get(), t("agent-quick-thinking"));
    composer.observe_receipt(ComposerReceipt {
        owner: tide,
        submission: request.submission,
        outcome: ComposerOutcome::Answered("Safe to repeat.".into()),
    });
    assert!(composer.draft.get().is_empty());
    assert_eq!(composer.view.answer.get(), "Safe to repeat.");
    assert!(composer.view.asked.get().contains("idempotent"));
    assert!(!composer.view.has_status.get());
    assert!(
        !composer.view.can_use_answer.get(),
        "nothing selected, nothing to insert into"
    );

    edit(&mut composer, &mut doc, "Give an example");
    let follow_up = submit(&mut composer, &mut doc);
    assert_eq!(follow_up.intent, ComposerIntent::Request);
    assert_eq!(
        follow_up.conversation,
        vec![
            ComposerTurn {
                agent: false,
                text: "What does idempotent mean?".into()
            },
            ComposerTurn {
                agent: true,
                text: "Safe to repeat.".into()
            },
        ]
    );
    assert_eq!(
        doc.get_heads(),
        heads,
        "private editor input never writes collaborative content"
    );
}

#[test]
fn the_person_can_rule_that_an_instruction_is_work_or_an_answer() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    edit(
        &mut composer,
        &mut doc,
        "Rename every file to its invoice number",
    );
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    // Nothing is picked until the person picks it: the agent decides.
    let decided = submit(&mut composer, &mut doc);
    assert_eq!(decided.as_task, None);
    assert!(
        decided.conversation.is_empty(),
        "the agent's own decision says nothing extra"
    );
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: decided.submission,
        outcome: ComposerOutcome::Answered("Which folder?".into()),
    });

    edit(&mut composer, &mut doc, "Sum up this section");
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    assert!(
        action(
            &mut composer,
            MODE_PICKER,
            &ui::a11y::Action::SetValue(t("agent-quick-mode-task")),
            &mut doc
        ),
        "the mode picker offers running it as a task"
    );
    let ruled = submit(&mut composer, &mut doc);
    assert_eq!(ruled.as_task, Some(true));
    assert_eq!(
        ruled.conversation.last(),
        Some(&ComposerTurn {
            agent: false,
            text: t("agent-quick-mode-task-line"),
        }),
        "a follow-up tells the agent what the person ruled"
    );
    // The composer waits for the agent's receipt of a request before it
    // sends another.
    composer.observe_receipt(ComposerReceipt {
        owner: tide,
        submission: ruled.submission,
        outcome: ComposerOutcome::Answered("Done.".into()),
    });

    // The ruling was for that request: the next one asks the agent again.
    edit(&mut composer, &mut doc, "And how long is the section?");
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    let next = submit(&mut composer, &mut doc);
    assert_eq!(next.as_task, None);
}

/// "Open as chat" moves what the person typed into a chat: it is asked
/// as a task for a chat, and the composer gives way once it is asked.
/// With a run followed it opens that run's issue as a chat, and with
/// nothing typed and no run it waits disabled; once a run ended, the
/// chat of the run whose work it still shows opens.
#[test]
fn open_as_chat_asks_what_was_typed_as_a_chat_or_opens_the_followed_run() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    assert!(shown(&mut composer, "agent-quick-open-chat"));
    assert!(
        !composer.view.can_chat.get(),
        "nothing typed, no run: nothing to open"
    );

    edit(
        &mut composer,
        &mut doc,
        "Review the applications in my inbox",
    );
    press(&mut composer, &mut doc, "agent-quick-open-chat");
    assert_eq!(composer.take_open_chat(), None, "no run to open yet");
    let asked = composer
        .take_request()
        .expect("the typed request is asked as a chat");
    assert!(asked.chat);
    assert_eq!(
        asked.as_task,
        Some(true),
        "a chat is work the agent carries on"
    );
    assert_eq!(asked.intent, ComposerIntent::Request);
    assert_eq!(asked.text, "Review the applications in my inbox");
    assert!(composer.is_open(), "it stays until the agent was asked");
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: asked.submission,
        outcome: ComposerOutcome::Started {
            task_id: "task:chat".into(),
            title: "Review the applications".into(),
        },
    });
    assert!(!composer.is_open(), "the conversation goes on in the chat");
    assert!(composer.draft.get().is_empty());

    // Opened again on the run it follows, the same control opens that
    // run's issue as a chat and asks nothing.
    open(&mut composer, tide.clone());
    composer.project(&tide, Some(working("task:chat", RunPhase::Working)));
    press(&mut composer, &mut doc, "agent-quick-open-chat");
    assert_eq!(composer.take_open_chat(), Some("task:chat".into()));
    assert_eq!(composer.take_open_chat(), None, "heard once");
    assert!(composer.take_request().is_none());
    assert!(composer.is_open());

    // Ended, the run is no longer followed but its work stays shown, and
    // the same control still opens its chat.
    composer.project(&tide, Some(working("task:chat", RunPhase::Finished(None))));
    composer.project(&tide, None);
    assert!(composer.followed().is_none());
    assert!(composer.view.can_chat.get(), "the ended run's chat opens");
    press(&mut composer, &mut doc, "agent-quick-open-chat");
    assert_eq!(composer.take_open_chat(), Some("task:chat".into()));
    assert!(composer.take_request().is_none());
}

#[test]
fn work_runs_as_a_task_the_same_field_steers_and_stop_appears_while_it_runs() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    edit(
        &mut composer,
        &mut doc,
        "Replace the link with the real one",
    );
    let request = submit(&mut composer, &mut doc);
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: request.submission,
        outcome: ComposerOutcome::Started {
            task_id: "task:new".into(),
            title: "Fix the link".into(),
        },
    });
    assert!(composer.draft.get().is_empty());
    assert!(composer.view.status.get().contains("Fix the link"));
    assert!(
        !shown(&mut composer, "agent-quick-stop"),
        "nothing to stop before the run starts"
    );

    let run = working("task:new", RunPhase::Working);
    composer.project(&tide, Some(run.clone()));
    assert!(shown(&mut composer, "agent-quick-stop"));
    edit(&mut composer, &mut doc, "Use the https address");
    let steer = submit(&mut composer, &mut doc);
    assert_eq!(steer.intent, ComposerIntent::Steer);
    assert_eq!(steer.task, Some(run.task.clone()));
    assert!(!steer.stop);
    let saved = ComposerTask {
        revision: 9,
        ..run.task.clone()
    };
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: steer.submission,
        outcome: ComposerOutcome::DirectionSaved(ComposerTask {
            run_epoch: 4,
            ..saved.clone()
        }),
    });
    assert_eq!(
        composer.draft.get(),
        "Use the https address",
        "another run's receipt never clears the draft"
    );
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: steer.submission,
        outcome: ComposerOutcome::DirectionSaved(saved.clone()),
    });
    assert!(composer.draft.get().is_empty());
    let saved_notice = composer.view.status.get();
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: steer.submission,
        outcome: ComposerOutcome::DirectionApplied(saved.clone()),
    });
    let applied_notice = composer.view.status.get();
    assert_ne!(applied_notice, saved_notice);
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: steer.submission,
        outcome: ComposerOutcome::DirectionSaved(saved),
    });
    assert_eq!(
        composer.view.status.get(),
        applied_notice,
        "applied never regresses to saved"
    );

    press(&mut composer, &mut doc, "agent-quick-stop");
    let stop = composer.take_request().expect("Stop is a request");
    assert!(stop.stop);
    assert_eq!(stop.task, Some(run.task.clone()));
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: stop.submission,
        outcome: ComposerOutcome::Stopped(run.task),
    });
    assert!(composer.followed().is_none());
    assert!(!shown(&mut composer, "agent-quick-stop"));
    edit(&mut composer, &mut doc, "Something else");
    assert_eq!(
        submit(&mut composer, &mut doc).intent,
        ComposerIntent::Request,
        "with nothing running, the field asks anew"
    );
}

#[test]
fn a_finished_run_is_told_once_as_the_agents_turn() {
    let mut composer = AgentQuickComposer::default();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    composer.set_about_selection(true);
    composer.project(&tide, Some(working("task:one", RunPhase::Working)));
    assert!(composer.followed().is_some());
    let done = working("task:one", RunPhase::Finished(Some("Linked it.".into())));
    composer.project(&tide, Some(done.clone()));
    composer.project(&tide, Some(done));
    assert!(composer.followed().is_none());
    assert_eq!(composer.view.answer.get(), "Linked it.");
    assert!(
        !composer.view.can_use_answer.get(),
        "a run's closing words are not text to insert"
    );
    assert_eq!(
        composer.conversations[&tide].len(),
        1,
        "a repeated projection is told once"
    );
    assert!(composer.view.status.get().contains("Tide"));
}

#[test]
fn a_refused_request_keeps_the_draft_and_an_unknown_one_is_never_resent() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    edit(&mut composer, &mut doc, "Tidy this");
    let request = submit(&mut composer, &mut doc);
    composer.observe_receipt(ComposerReceipt {
        owner: owner("reed"),
        submission: request.submission,
        outcome: ComposerOutcome::Refused,
    });
    assert!(composer.busy(), "another agent's receipt is ignored");
    composer.observe_receipt(ComposerReceipt {
        owner: tide.clone(),
        submission: request.submission,
        outcome: ComposerOutcome::Unknown,
    });
    assert_eq!(composer.draft.get(), "Tidy this");
    assert!(composer.busy(), "uncertain work is not replayed");
    assert!(composer.take_request().is_none());
    composer.observe_receipt(ComposerReceipt {
        owner: tide,
        submission: request.submission,
        outcome: ComposerOutcome::Refused,
    });
    assert_eq!(composer.draft.get(), "Tidy this");
    assert!(composer.view.enabled.get());
}

#[test]
fn drafts_and_conversations_stay_with_their_agent() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    edit(&mut composer, &mut doc, "Tide's private draft");
    open(&mut composer, owner("reed"));
    assert!(composer.draft.get().is_empty());
    edit(&mut composer, &mut doc, "Reed's private draft");
    open(&mut composer, tide);
    assert_eq!(composer.draft.get(), "Tide's private draft");
}

#[test]
fn a_text_answer_about_a_selection_can_be_inserted_or_kept_as_a_note() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    composer.set_about_selection(true);
    assert!(!shown(&mut composer, "agent-quick-insert"), "no answer yet");
    edit(&mut composer, &mut doc, "Translate this");
    let request = submit(&mut composer, &mut doc);
    composer.observe_receipt(ComposerReceipt {
        owner: tide,
        submission: request.submission,
        outcome: ComposerOutcome::Answered("Hallo".into()),
    });
    assert!(shown(&mut composer, "agent-quick-insert"));
    press(&mut composer, &mut doc, "agent-quick-insert");
    assert_eq!(
        composer.take_answer_use(),
        Some(AnswerUse::Insert("Hallo".into()))
    );
    press(&mut composer, &mut doc, "agent-quick-save-note");
    assert_eq!(
        composer.take_answer_use(),
        Some(AnswerUse::SaveAsNote("Hallo".into()))
    );
    assert_eq!(composer.take_answer_use(), None);
}

#[test]
fn the_composer_is_anchored_beside_what_it_is_about() {
    let mut composer = AgentQuickComposer::default();
    let selection = Rect::new(560.0, 180.0, 700.0, 240.0);
    composer.open(
        owner("tide"),
        "Tide",
        AgentQuickComposerOrigin {
            marker: Rect::new(560.0, 180.0, 561.0, 181.0),
            protected: selection,
            context: String::new(),
        },
        Rect::new(0.0, 0.0, 1280.0, 800.0),
    );
    let theme = Theme::dark();
    let mut text = ui::text_render::TextRenderer::new();
    let mut taffy = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    Widget::build_layout(&mut composer, &mut taffy, &theme, &mut nodes);
    composer.popover.place(&mut text, &theme);
    let rect = composer.popover.rect();
    assert!(rect.area() > 0.0);
    assert_eq!(
        rect.intersect(selection).area(),
        0.0,
        "{rect:?} covers {selection:?}"
    );
    assert!(rect.x0 >= selection.x1, "beside the selection: {rect:?}");

    // No room to its right: below it, still clear of it.
    let right = Rect::new(1100.0, 180.0, 1270.0, 240.0);
    composer.open(
        owner("tide"),
        "Tide",
        AgentQuickComposerOrigin {
            marker: Rect::new(1100.0, 180.0, 1101.0, 181.0),
            protected: right,
            context: String::new(),
        },
        Rect::new(0.0, 0.0, 1280.0, 800.0),
    );
    Widget::build_layout(&mut composer, &mut taffy, &theme, &mut nodes);
    composer.popover.place(&mut text, &theme);
    let rect = composer.popover.rect();
    assert_eq!(
        rect.intersect(right).area(),
        0.0,
        "{rect:?} covers {right:?}"
    );
}

/// The app said where the selected paragraph is drawn, in a window as
/// wide as the screen and reaching its bottom: no room beside it or
/// below, so the composer opens above the paragraph and covers none of
/// it.
#[test]
fn the_composer_opens_above_a_selected_paragraph_with_no_room_beside_or_below() {
    let mut composer = AgentQuickComposer::default();
    let paragraph = Rect::new(8.0, 470.0, 1272.0, 790.0);
    composer.open(
        owner("tide"),
        "Tide",
        AgentQuickComposerOrigin {
            marker: paragraph,
            protected: paragraph,
            context: String::new(),
        },
        Rect::new(0.0, 0.0, 1280.0, 800.0),
    );
    let theme = Theme::dark();
    let mut text = ui::text_render::TextRenderer::new();
    let mut taffy = taffy::TaffyTree::new();
    let mut nodes = HashMap::new();
    Widget::build_layout(&mut composer, &mut taffy, &theme, &mut nodes);
    composer.popover.place(&mut text, &theme);
    let rect = composer.popover.rect();
    assert!(rect.area() > 0.0);
    assert_eq!(
        rect.intersect(paragraph).area(),
        0.0,
        "{rect:?} covers {paragraph:?}"
    );
    assert!(rect.y1 <= paragraph.y0, "above the paragraph: {rect:?}");
}

#[test]
fn a_composer_opened_on_a_selection_carries_it_with_the_request() {
    use super::super::selection_agent::SelectionSeed;
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    let seed = SelectionSeed::new(
        "so.tilo.word-processor",
        "Writer",
        Some(
            ui::shared_selection::SharedSelection::of_text("which were late")
                .at(tilo_actions::SelectionPlace::range("block-4", 12, 27)),
        ),
    );
    composer.open_about(
        tide.clone(),
        "Tide",
        &AgentQuickComposerOrigin {
            marker: Rect::new(400.0, 200.0, 401.0, 201.0),
            protected: Rect::new(400.0, 200.0, 401.0, 201.0),
            context: String::new(),
        },
        Rect::new(0.0, 0.0, 800.0, 600.0),
        seed.clone(),
    );
    assert!(composer.about_selection());
    assert!(composer.selection, "a text answer may go at the selection");
    assert!(
        composer.view.context.get().contains("which were late"),
        "the composer shows what it is about: {}",
        composer.view.context.get()
    );
    edit(&mut composer, &mut doc, "Tighten this");
    let request = submit(&mut composer, &mut doc);
    assert_eq!(request.intent, ComposerIntent::Request);
    assert_eq!(request.text, "Tighten this");
    assert!(request.context.contains("which were late"));
    assert_eq!(request.about, Some(seed));

    open(&mut composer, tide);
    assert!(
        !composer.about_selection() && !composer.selection,
        "a composer opened at a marker is about nothing selected"
    );
}

#[test]
fn the_person_picks_which_agent_to_tell_and_the_host_hears_it() {
    use super::super::selection_agent::SelectionSeed;
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let origin = AgentQuickComposerOrigin {
        marker: Rect::new(400.0, 200.0, 401.0, 201.0),
        protected: Rect::new(400.0, 200.0, 401.0, 201.0),
        context: String::new(),
    };
    let seed = SelectionSeed::new("so.tilo.notes", "Notes", None);
    composer.open_about(
        owner("tide"),
        "tide",
        &origin,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        seed.clone(),
    );
    composer.offer_agents(vec!["tide".into()]);
    assert!(
        !shown(&mut composer, AGENT_PICKER),
        "one agent: nothing to pick"
    );
    composer.offer_agents(vec!["tide".into(), "reed".into()]);
    assert!(shown(&mut composer, AGENT_PICKER));
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    assert!(action(
        &mut composer,
        AGENT_PICKER,
        &ui::a11y::Action::SetValue("reed".into()),
        &mut doc
    ));
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    assert_eq!(composer.take_agent_switch(), Some("reed".into()));
    assert_eq!(composer.take_agent_switch(), None, "heard once");
    composer.open_about(
        owner("reed"),
        "reed",
        &origin,
        Rect::new(0.0, 0.0, 800.0, 600.0),
        seed.clone(),
    );
    composer.offer_agents(vec!["tide".into(), "reed".into()]);
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    assert_eq!(composer.take_agent_switch(), None);
    assert_eq!(composer.about(), Some(&seed));
}

#[test]
#[serial_test::serial]
fn the_model_picker_lists_what_the_persons_providers_offer() {
    use agent_provider_config::{OfferedModel, OfferedModels};
    let _home = tilo_test_env::TestHome::new().unwrap();
    let held =
        crate::shell::settings_records::Holder::of(&crate::shell::state::local_user_id()).unwrap();
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    open(&mut composer, owner("tide"));
    assert!(!shown(&mut composer, MODEL_PICKER), "only the agent's own");

    held.run(|records| {
        let record = records.shared::<OfferedModels>().unwrap();
        record
            .invoke(&OfferedModels::OFFER.with(vec![OfferedModel {
                profile: "work".into(),
                provider: "Work".into(),
                model: "m-large".into(),
                label: "Large".into(),
            }]))
            .unwrap();
    })
    .unwrap();
    open(&mut composer, owner("tide"));
    assert!(shown(&mut composer, MODEL_PICKER));
    let large = composer.models[1].0.clone();
    Widget::sync(&mut composer, &doc, &automerge::ROOT);
    assert!(action(
        &mut composer,
        MODEL_PICKER,
        &ui::a11y::Action::SetValue(large),
        &mut doc
    ));
    edit(&mut composer, &mut doc, "Tidy the notes");
    assert_eq!(
        submit(&mut composer, &mut doc).model,
        Some(runtime_collab::TaskRunModel {
            profile: "work".into(),
            model: "m-large".into(),
        })
    );
}

fn board_card() -> ResourceAt {
    ResourceAt {
        app: "so.tilo.lang.board".into(),
        record: None,
        reference: tilo_reference::ResourceRef::whole(
            "tl.so.tilo.lang.board.Card",
            "tl:so.tilo.lang.board:root/cards:c1",
            "Write the release notes",
        ),
    }
}

fn release_progress(answer: bool) -> RunProgress {
    RunProgress {
        streamed: "Three cards are **open**".into(),
        steps: Vec::new(),
        programs: vec![ProgramStep {
            program: "board.move(@board.Card:root/cards:c1, column: \"Review\")".into(),
            outcome: tilo_reference::StepOutcome::Asked,
            answer: vec![
                "@board.Card:root/cards:c1 \"Write the release notes\" column: \"Review\"".into(),
            ],
            refs: vec![board_card()],
            changes: vec![StepChange {
                resource: board_card(),
                action: "Moved to Review".into(),
            }],
            previews: vec![StepPreview {
                invocation: ResourceAt {
                    app: "so.tilo.lang.chat".into(),
                    record: None,
                    reference: tilo_reference::ResourceRef::whole("tl.invocation", "inv-7", "Post"),
                },
                action: "Post in Release".into(),
                summary: String::new(),
            }],
            planned: Vec::new(),
        }],
        answer: answer.then(|| ResourceAt {
            app: "so.tilo.lang.views".into(),
            record: None,
            reference: tilo_reference::ResourceRef::whole(
                "tl.so.tilo.lang.views.WrittenView",
                "tl:so.tilo.lang.views:a1",
                "What's left for the release",
            ),
        }),
    }
}

/// A working run shows its steps, and what the person does with them reaches the host as
/// presses: opening a reference, undoing a change for the run's task, deciding a proposal.
/// The steps stay readable once the run ended, and its answer view opens exactly once.
#[test]
fn the_run_shows_its_steps_and_what_the_person_does_with_them_reaches_the_host() {
    let mut composer = AgentQuickComposer::default();
    let mut doc = automerge::AutoCommit::new();
    let tide = owner("tide");
    open(&mut composer, tide.clone());
    assert!(!composer.view.has_work.get(), "no run, no steps");
    let mut run = working("task:release", RunPhase::Working);
    run.progress = release_progress(false);
    composer.project(&tide, Some(run.clone()));
    assert!(composer.view.has_work.get());
    assert_eq!(composer.shown_answer(), "Three cards are **open**");
    assert!(
        shown(&mut composer, "agent-quick-answer"),
        "the reply is rendered"
    );
    assert!(
        composer.take_answer_to_open().is_none(),
        "no answer view yet"
    );

    press(
        &mut composer,
        &mut doc,
        "reference:tl:so.tilo.lang.board:root/cards:c1",
    );
    press(&mut composer, &mut doc, "agent-quick-step-0-change-0-undo");
    press(
        &mut composer,
        &mut doc,
        "agent-quick-step-0-preview-0-decline",
    );
    let presses = composer.take_work_presses();
    assert_eq!(
        presses,
        vec![
            WorkPress::Open(board_card()),
            WorkPress::Undo {
                task: "task:release".into(),
                change: run.progress.programs[0].changes[0].clone(),
            },
            WorkPress::Resolve {
                preview: run.progress.programs[0].previews[0].clone(),
                approve: false,
            },
        ]
    );
    assert!(
        composer.take_work_presses().is_empty(),
        "each is taken once"
    );

    // The run answers with a view and ends: the view opens once, the steps stay.
    run.progress = release_progress(true);
    composer.project(&tide, Some(run.clone()));
    let answer = run.progress.answer.clone().unwrap();
    assert_eq!(composer.take_answer_to_open(), Some(answer.clone()));
    run.phase = RunPhase::Finished(Some("Moved it.".into()));
    composer.project(&tide, Some(run.clone()));
    composer.project(&tide, Some(run));
    assert_eq!(composer.take_answer_to_open(), None, "opened once");
    assert!(composer.followed().is_none());
    assert!(composer.view.has_work.get(), "the ended run's steps stay");
    assert!(shown(&mut composer, "agent-quick-step-0-program"));
    // Opening it again is the person's to ask.
    press(&mut composer, &mut doc, "agent-quick-answer-open");
    assert_eq!(composer.take_answer_to_open(), Some(answer));

    // A new request makes way for new work.
    edit(&mut composer, &mut doc, "And what about the website?");
    let _ = submit(&mut composer, &mut doc);
    assert!(!composer.view.has_work.get());
}

#[test]
fn every_composer_string_is_in_every_catalogue() {
    const KEYS: &[&str] = &[
        "agent-quick-title",
        "agent-quick-agent",
        "agent-quick-about-selection",
        "agent-quick-about-window",
        "agent-quick-accessible-name",
        "agent-quick-close",
        "agent-quick-insert",
        "agent-quick-save-note",
        "agent-quick-placeholder-idle",
        "agent-quick-placeholder-running",
        "agent-quick-editor-name",
        "agent-quick-send",
        "agent-quick-stop",
        "agent-quick-open-run",
        "agent-quick-open-chat",
        "agent-quick-you-asked",
        "agent-quick-thinking",
        "agent-quick-steering",
        "agent-quick-stopping",
        "agent-quick-steer-saved",
        "agent-quick-steer-applied",
        "agent-quick-run-starting",
        "agent-quick-run-working",
        "agent-quick-run-done",
        "agent-quick-run-attention",
        "agent-quick-run-stopped",
        "agent-quick-outcome-unknown",
        "agent-quick-refused",
        "agent-quick-stop-refused",
        "agent-quick-answer-view",
        "agent-quick-answer-open",
        "agent-quick-undo-refused",
        "agent-quick-resolve-refused",
    ];
    for (locale, source) in [
        ("en-US", include_str!("../../locales/en-US.ftl")),
        ("de-DE", include_str!("../../locales/de-DE.ftl")),
    ] {
        for key in KEYS {
            assert!(
                source
                    .lines()
                    .any(|line| line.starts_with(&format!("{key} = "))),
                "{key} is missing from {locale}"
            );
        }
        for retired in [
            "agent-quick-checking",
            "agent-quick-queue",
            "agent-quick-steer",
        ] {
            assert!(
                !source
                    .lines()
                    .any(|line| line.starts_with(&format!("{retired} = "))),
                "{retired} is retired from {locale}"
            );
        }
    }
}
