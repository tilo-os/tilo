//! The conversation's summary, kept while a turn is answered: one question to a model once the
//! reply has its words, and one `keep-summary` with what is kept of the answer.

use std::sync::Mutex;

use super::*;

/// A model as a test scripts it: it answers every single question with the same words and keeps
/// what it was asked.
struct Summariser {
    says: Result<&'static str, &'static str>,
    asked: Mutex<Vec<(String, String)>>,
}

impl Summariser {
    fn saying(says: Result<&'static str, &'static str>) -> Arc<Self> {
        Arc::new(Self {
            says,
            asked: Mutex::new(Vec::new()),
        })
    }

    fn asked(&self) -> Vec<(String, String)> {
        self.asked.lock().unwrap().clone()
    }
}

impl crate::transform::TransformModel for Summariser {
    fn answer(
        &self,
        kind: agent_provider_config::Interaction,
        instructions: &str,
        input: &str,
        _: u32,
        _: Duration,
    ) -> Result<String, String> {
        assert_eq!(kind, agent_provider_config::Interaction::OneShot);
        self.asked
            .lock()
            .unwrap()
            .push((instructions.to_owned(), input.to_owned()));
        self.says.map(str::to_owned).map_err(str::to_owned)
    }
}

/// What a turn asks its agent to take into the summary: two messages after a summary of one line.
fn fold() -> runtime_collab::recall::Fold {
    runtime_collab::recall::Fold {
        conversation: "loop".into(),
        summary: "The launch is on Friday.".into(),
        lines: "Person: who owns the list?\nYou: Ada does.".into(),
        through: "m7".into(),
        through_ts: 70,
    }
}

/// A turn that answers in one response, in a workspace whose turn offers `keep-summary` or not.
fn answering(summary: bool, fold: Option<runtime_collab::recall::Fold>) -> Fixture {
    let mut f = fixture(
        "and who signs it off?",
        World {
            chat: Some("asked".into()),
            summary,
            ..World::default()
        },
        vec![turn(&[finish("Bo signs it off.", true)], 24)],
    );
    f.run.asked.fold = fold;
    f
}

tilo_test_env::scenario! {
    fn a_turn_keeps_its_conversations_summary_after_it_answered;

    phase the_summary_is_extended_once_the_reply_has_its_words;
    // The reply is told its words, then a model is asked once for what the messages add, then
    // the summary is kept through Chat's own verb, and only then the turn ends.
    {
        let mut f = answering(true, Some(fold()));
        let model = Summariser::saying(Ok("Ada owns the list."));
        f.session.set_single_shot(model.clone());
        f.session.run_task(&f.run).unwrap();
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded { message: "Bo signs it off.".into() }
        );
        let asked = model.asked();
        let [(instructions, input)] = asked.as_slice() else {
            panic!("one question, not {asked:?}");
        };
        assert!(instructions.contains("what these messages add"), "{instructions}");
        assert_eq!(
            input,
            "Summary so far:\nThe launch is on Friday.\n\nMessages after it, oldest first:\n\
             Person: who owns the list?\nYou: Ada does."
        );
        let sent = f.sent.borrow();
        let kept = recorded(&sent, "keep-summary");
        let [kept] = kept.as_slice() else {
            panic!("one summary kept, not {kept:?}");
        };
        assert_eq!(kept["channel"], "loop");
        assert_eq!(kept["through"], "m7");
        assert_eq!(kept["through_ts"], 70);
        assert_eq!(kept["text"], "The launch is on Friday.\nAda owns the list.");
        // The person had their answer before the summary was asked for: the reply's words were
        // recorded while the turn still went, and its end came after.
        let work = recorded(&sent, "record-work");
        let said = work
            .iter()
            .position(|record| record.get("say") == Some(&Value::from("Bo signs it off.")))
            .expect("the reply was told its words");
        assert_eq!(work[said]["work"]["state"], "doing");
        assert_eq!(work.last().unwrap()["work"]["state"], "provider_complete");
    }

    phase a_turn_that_asks_for_nothing_asks_no_model;
    {
        let mut f = answering(true, None);
        let model = Summariser::saying(Ok("unused"));
        f.session.set_single_shot(model.clone());
        f.session.run_task(&f.run).unwrap();
        assert_eq!(model.asked(), Vec::new());
        let sent = f.sent.borrow();
        assert!(recorded(&sent, "keep-summary").is_empty());
        assert!(
            recorded(&sent, "record-work")
                .iter()
                .all(|record| record["work"]["state"] != "doing"),
            "a turn with no summary to keep ends as it always did"
        );
    }

    phase where_the_summary_could_not_be_kept_no_model_is_asked;
    // A workspace whose turn offers no `keep-summary` cannot keep one: the question would be
    // paid for and thrown away, so it is not asked, and the turn ends as it would have.
    {
        let mut f = answering(false, Some(fold()));
        let model = Summariser::saying(Ok("unused"));
        f.session.set_single_shot(model.clone());
        f.session.run_task(&f.run).unwrap();
        assert_eq!(model.asked(), Vec::new());
        assert!(recorded(&f.sent.borrow(), "keep-summary").is_empty());
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded { message: "Bo signs it off.".into() }
        );
    }

    phase a_model_that_does_not_answer_leaves_the_summary_and_the_turn_as_they_were;
    {
        let mut f = answering(true, Some(fold()));
        let model = Summariser::saying(Err("no model provider is configured in Settings"));
        f.session.set_single_shot(model.clone());
        f.session.run_task(&f.run).unwrap();
        assert_eq!(model.asked().len(), 1);
        assert!(recorded(&f.sent.borrow(), "keep-summary").is_empty());
        assert_eq!(
            terminal(&f.sent.borrow()),
            TurnUpdate::Succeeded { message: "Bo signs it off.".into() }
        );
    }
}

tilo_test_env::scenario! {
    fn an_agent_told_how_to_work_is_told_so_before_every_request;

    phase its_keepers_instruction_follows_what_every_agent_is_told;
    {
        let mut f = fixture(
            "what is due?",
            World::default(),
            vec![turn(&[finish("Nothing today.", true)], 24)],
        );
        f.run.asked.agent_instruction = Some("Answer in German.\nKeep it short.".into());
        f.session.run_task(&f.run).unwrap();
        let bodies = f.script.bodies.lock().unwrap().clone();
        let system = bodies[0]["messages"][0]["content"].to_string();
        assert!(system.contains("Answer in ONE response"), "{system}");
        assert!(
            system.contains("What the person who keeps you told you about how to work for them"),
            "{system}"
        );
        assert!(system.contains("Answer in German."), "{system}");
    }

    phase an_agent_told_nothing_reads_what_it_always_read;
    {
        let mut f = fixture(
            "what is due?",
            World::default(),
            vec![turn(&[finish("Nothing today.", true)], 24)],
        );
        f.session.run_task(&f.run).unwrap();
        let bodies = f.script.bodies.lock().unwrap().clone();
        let system = bodies[0]["messages"][0]["content"].to_string();
        assert!(!system.contains("told you about how to work"), "{system}");
    }
}
