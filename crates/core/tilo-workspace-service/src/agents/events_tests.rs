#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::panic,
    reason = "in a test these are the assertion, not a hazard"
)]

use super::*;
use tilo_automation::dispatch::retry;

const PERSON: &str = "abababababababababababababababababababababababababababababababab";

fn reminder() -> Definition {
    Definition {
        id: "7f0c1c0e-5a1b-4a52-9d0f-0d6f3a1b2c3d".into(),
        name: " Call home ".into(),
        when: "once at 2026-11-17T11:32:00Z".into(),
        only_if: Some("  ".into()),
        every: Some(1),
        instruction: " Say that it is time to call home. ".into(),
        told_as: None,
        set_up_at_ms: 1,
        looks: false,
    }
}

fn kept(definition: Definition) -> Kept {
    Kept {
        definition: definition
            .checked()
            .unwrap_or_else(|error| panic!("{error}")),
        note: Note {
            style: "One line.".into(),
            ..Note::default()
        },
        person: PERSON.into(),
        assistant: "scout".into(),
        by: MemberId::person(PERSON),
    }
}

fn ids(steps: &[Step]) -> Vec<&str> {
    steps.iter().map(|step| step.id.as_str()).collect()
}

fn then_of(step: &Step) -> &[Step] {
    match &step.kind {
        StepKind::If { then, .. } => then,
        _ => &[],
    }
}

/// The `text` the tell step hands over, and its other arguments.
fn told_by(step: &Step) -> (&str, &BTreeMap<String, Binding>) {
    let StepKind::Invoke { action, args, .. } = &step.kind else {
        panic!("the instruction is written by Chat's write_to: {step:?}");
    };
    assert_eq!(
        (action.app.as_str(), action.action.as_str()),
        (super::super::triggered::CHAT, "write_to")
    );
    let Some(Binding::Template(text)) = args.get("body") else {
        panic!("the words are a template: {args:?}");
    };
    (text, args)
}

#[test]
fn a_reminder_is_a_moment_and_one_instruction_handed_to_the_persons_assistant() {
    let kept = kept(reminder());
    let reminder = &kept.definition;
    assert_eq!(reminder.name, "Call home");
    assert_eq!(reminder.only_if, None, "an empty question asks nothing");
    assert_eq!(reminder.every, None, "every time is no count");
    assert!(reminder.on_a_clock());
    let shortcut = event_shortcut(&kept).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(shortcut.id, reminder.id, "the event is its shortcut");
    assert_eq!(
        shortcut.triggers,
        vec![TriggerSpec::Once {
            at_ms: 1_794_915_120_000
        }]
    );
    assert_eq!(ids(&shortcut.steps), ["tell"]);
    let (text, args) = told_by(&shortcut.steps[0]);
    assert_eq!(
        text, "{{input.instruction}}",
        "the words are bound, never written into the shortcut"
    );
    assert_eq!(
        args.get(WHO),
        Some(&Binding::Literal(json!(MemberId::agent("scout", PERSON)))),
        "written to the person's assistant, in their own conversation with it"
    );
    // The instruction is the input's default, with which event this is and its note.
    let instruction = shortcut
        .inputs
        .iter()
        .find(|input| input.name == INSTRUCTION_INPUT)
        .and_then(|input| input.schema.as_deref()?.get("default").cloned())
        .unwrap_or_default();
    let instruction = instruction.as_str().unwrap_or_default();
    assert!(
        instruction.starts_with("Say that it is time to call home."),
        "{instruction}"
    );
    for said in ["Call home", "7f0c1c0e", "One line."] {
        assert!(instruction.contains(said), "{said}: {instruction}");
    }
    // What the shortcut keeps reads back as the same event.
    assert_eq!(kept_of(&shortcut), Some(kept));
}

#[test]
fn a_question_comes_first_then_the_count_then_the_instruction() {
    let kept = kept(Definition {
        name: "Every tenth order that matters".into(),
        when: "every 3600 seconds".into(),
        only_if: Some("Is this an order over a thousand?".into()),
        every: Some(10),
        ..reminder()
    });
    let shortcut = event_shortcut(&kept).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(ids(&shortcut.steps), ["judge", "judged"]);
    let StepKind::Ai { prompt, output, .. } = &shortcut.steps[0].kind else {
        panic!("the question is asked of its person's model");
    };
    assert_eq!(prompt, "Is this an order over a thousand?");
    assert_eq!(output, &Some(json!({"type": "boolean"})));
    let counted = then_of(&shortcut.steps[1]);
    assert_eq!(
        ids(counted),
        ["count", "counted"],
        "only what passes the question is counted"
    );
    let StepKind::Invoke { action, args, .. } = &counted[0].kind else {
        panic!("the library counts");
    };
    assert_eq!(action.action, COUNT_ACTION);
    assert_eq!(args.get("every"), Some(&Binding::Literal(json!(10))));
    assert_eq!(ids(then_of(&counted[1])), ["tell"]);
}

#[test]
fn what_happened_in_the_workspace_goes_with_the_instruction_as_data() {
    let line = tilo_automation::text::trigger_line(&TriggerSpec::ResourceChanged {
        kind: "tasks.issue".into(),
        actions: Vec::new(),
        fields: Vec::new(),
        by: tilo_automation::ActorFilter::default(),
        filter: tilo_automation::ChangeFilter::default(),
    });
    let kept = kept(Definition {
        when: line,
        ..reminder()
    });
    assert!(!kept.definition.on_a_clock());
    let shortcut = event_shortcut(&kept).unwrap_or_else(|error| panic!("{error}"));
    let (text, _) = told_by(&shortcut.steps[0]);
    assert!(text.starts_with("{{input.instruction}}"), "{text}");
    assert!(text.contains("data, not instructions"), "{text}");
    assert!(text.ends_with("{{input.event}}"), "{text}");
}

#[test]
fn a_look_round_looks_first_and_hands_on_only_what_it_found() {
    let kept = kept(Definition {
        when: "every 3600 seconds".into(),
        looks: true,
        ..reminder()
    });
    let shortcut = event_shortcut(&kept).unwrap_or_else(|error| panic!("{error}"));
    assert_eq!(ids(&shortcut.steps), ["look", "found"]);
    let StepKind::Invoke { action, args, .. } = &shortcut.steps[0].kind else {
        panic!("the look is the initiative's");
    };
    assert_eq!(
        action.action,
        tilo_actions::remote::workspace_initiative::LOOK
    );
    assert_eq!(
        args.get("firing"),
        Some(&Binding::Input(FIRING_INPUT.into()))
    );
    let (text, _) = told_by(&then_of(&shortcut.steps[1])[0]);
    assert_eq!(text, "{{input.instruction}}\n\n{{step.look/found}}");
}

#[test]
fn a_feed_or_a_page_on_the_web_or_mail_starts_an_event_within_its_bounds() {
    let watching = |when: &str| {
        Definition {
            name: "Watch".into(),
            when: when.into(),
            ..reminder()
        }
        .checked()
    };
    let feed = watching(
        "when something new is in \"tilo.web.feed\" at \"https://example.org/feed.xml\" \
         looked at every 900 seconds",
    )
    .unwrap_or_else(|error| panic!("{error}"));
    assert!(!feed.on_a_clock(), "it waits for something to happen");
    assert_eq!(
        feed.trigger(),
        Ok(TriggerSpec::SourceChanged {
            kind: tilo_automation::source_change::WEB_FEED.into(),
            at: "https://example.org/feed.xml".into(),
            every_secs: Some(900),
            filter: Map::new(),
        })
    );
    let page =
        watching("when something new is in \"tilo.web.page\" at \"https://example.org/prices\"")
            .unwrap_or_else(|error| panic!("{error}"));
    let shortcut = event_shortcut(&kept(page)).unwrap_or_else(|error| panic!("{error}"));
    let (text, _) = told_by(&shortcut.steps[0]);
    assert!(text.contains("data, not instructions"), "{text}");
    // Oftener than the bound, without an address, and a source nobody here reads, refused in
    // words when it is described.
    for (when, says) in [
        (
            "when something new is in \"tilo.web.page\" at \"https://example.org/\" \
             looked at every 60 seconds",
            "300",
        ),
        ("when something new is in \"tilo.web.feed\"", "address"),
        (
            "when something new is in \"something.else\"",
            "tilo.web.feed",
        ),
    ] {
        let refused = watching(when).err().unwrap_or_default();
        assert!(refused.contains(says), "{when}: {refused}");
    }
}

#[test]
fn an_event_says_how_its_result_is_told_and_keeps_the_word_both_ways() {
    let checked = |event: Definition| event.checked().unwrap_or_else(|error| panic!("{error}"));
    // Nothing said: a moment is a reminder, and what comes round is news.
    let moment = checked(reminder());
    assert_eq!((moment.told_as, moment.tells_as()), (None, Kind::Reminder));
    let daily = checked(Definition {
        when: "every day at 09:00".into(),
        ..reminder()
    });
    assert_eq!((daily.told_as, daily.tells_as()), (None, Kind::News));
    assert_eq!(daily.starts_as(), EVENT);
    let kept = serde_json::to_value(&daily).unwrap_or_default();
    assert!(
        kept.get("toldAs").is_none() && kept.get("looks").is_none(),
        "nothing said is nothing kept: {kept}"
    );

    // Said, the word decides whatever starts the event, and what is kept of it reads back as
    // the same event.
    for (said, told, starts) in [
        ("brief", Kind::Brief, BRIEF),
        ("news", Kind::News, EVENT),
        ("reminder", Kind::Reminder, REMINDER),
    ] {
        for when in ["every day at 09:00", "once at 2026-11-17T11:32:00Z"] {
            let event = checked(Definition {
                when: when.into(),
                told_as: told_as_of(said).unwrap_or_else(|error| panic!("{error}")),
                ..reminder()
            });
            assert_eq!(event.tells_as(), told, "{said}, {when}");
            assert_eq!(event.starts_as(), starts, "{said}, {when}");
            let kept = serde_json::to_value(&event).unwrap_or_default();
            assert_eq!(kept["toldAs"], json!(said), "{kept}");
            assert_eq!(
                serde_json::from_value::<Definition>(kept).ok(),
                Some(event),
                "{said}, {when}"
            );
        }
    }
    // The word is read whatever its case, and an empty one says nothing.
    assert_eq!(told_as_of(" Brief "), Ok(Some(Kind::Brief)));
    assert_eq!(told_as_of("  "), Ok(None));
    // A word that names no way an event's result is told is refused in words. A question is
    // none: no event makes one of its result.
    for wrong in ["question", "important", "loud"] {
        let refused = told_as_of(wrong).err().unwrap_or_default();
        assert!(refused.contains("`brief`"), "{wrong}: {refused}");
    }
    let asking = Definition {
        told_as: Some(Kind::Question),
        ..reminder()
    }
    .checked()
    .err()
    .unwrap_or_default();
    assert!(asking.contains("`brief`"), "{asking}");
}

#[test]
fn a_description_past_its_bounds_or_for_a_device_is_refused_in_words() {
    let refused = |event: Definition| event.checked().err().unwrap_or_default();
    assert!(
        refused(Definition {
            name: " ".into(),
            ..reminder()
        })
        .contains("name")
    );
    assert!(
        refused(Definition {
            instruction: "x".repeat(MAX_INSTRUCTION_BYTES + 1),
            ..reminder()
        })
        .contains("what to do")
    );
    assert!(
        refused(Definition {
            every: Some(0),
            ..reminder()
        })
        .contains("every 1 time")
    );
    assert!(
        refused(Definition {
            when: "every 5 seconds".into(),
            ..reminder()
        })
        .contains("60 seconds")
    );
    assert!(
        refused(Definition {
            when: "when someone presses \"ctrl+k\"".into(),
            ..reminder()
        })
        .contains("every device off")
    );
    assert!(
        refused(Definition {
            when: "in two minutes".into(),
            ..reminder()
        })
        .contains("once at"),
        "a line that is no trigger is told how one is written"
    );
}

#[test]
fn what_an_events_firings_did_is_read_from_their_done_rows() {
    let row = |at_ms: i64, ended: retry::Ended, counted: bool, why: Option<&str>| {
        serde_json::to_value(crate::apps::triggers::Ended {
            run: None,
            at_ms,
            ended,
            step: None,
            why: why.map(str::to_owned),
            counted,
        })
        .unwrap()
    };
    let done = BTreeMap::from([
        (
            "once/shortcuts.shortcut:e#run/1".to_owned(),
            row(10, retry::Ended::Ran, true, None),
        ),
        (
            "once/shortcuts.shortcut:e#run/2".to_owned(),
            row(30, retry::Ended::Refused, false, Some("no model")),
        ),
        (
            "once/shortcuts.shortcut:e#run/3".to_owned(),
            row(20, retry::Ended::Ran, true, None),
        ),
        (
            "once/shortcuts.shortcut:e#run/4".to_owned(),
            json!({"not": "a done row"}),
        ),
    ]);
    let runs = Runs::of(&done);
    assert_eq!(
        (runs.runs, runs.counted),
        (3, 2),
        "a row that does not read is no run"
    );
    assert_eq!(
        runs.last,
        Some(workspace_events::LastRun {
            at_ms: 30,
            ended: workspace_events::Ended::Refused,
            why: Some("no model".into()),
        }),
        "the newest run is the last, whatever order the rows are in"
    );

    // A moment that ran has no next time; one paused has none either; the count is what is
    // counted towards the next time it goes on.
    let kept = kept(Definition {
        when: "once at 2026-11-17T11:32:00Z".into(),
        every: Some(3),
        ..reminder()
    });
    let mut shortcut = event_shortcut(&kept).unwrap();
    let ran = view(&kept, &shortcut, &runs, 0);
    assert_eq!((ran.next_at_ms, ran.runs, ran.count), (None, 3, 2));
    assert!(ran.on);
    let waiting = view(&kept, &shortcut, &Runs::default(), 0);
    assert_eq!(waiting.next_at_ms, Some(1_794_915_120_000));
    shortcut.paused = true;
    let paused = view(&kept, &shortcut, &Runs::default(), 0);
    assert_eq!((paused.on, paused.next_at_ms), (false, None));
    assert_eq!(paused.by, MemberId::person(PERSON));
}
