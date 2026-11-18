//! What the agent's own log keeps of a task.
//!
//! The agent writes its log to stderr, which the launcher keeps: a file on the person's
//! device. A request is the person's words, a call's arguments are what the model wrote for
//! them (a sentence, a document's text, where a model errs a password), and what a tool
//! answers is what the workspace holds. So none of them stands in a line of the log, at any
//! level: a line keeps the tool by its name, each argument by its name and the bytes of its
//! value, and a request, an answer and a failure by their sizes.

use super::*;

/// What the person asks for, what a call writes, what a misshapen call writes: each made up
/// for this test, and each a word nothing else in a task says.
const ASKED: &str = "made-up-asked-7f3a91";
const WROTE: &str = "made-up-wrote-c20d4e";
const MISCALLED: &str = "made-up-miscalled-5b8e17";
/// What the fixture's application answers a step it refuses.
const REFUSED: &str = "type-text was refused";

/// The lines a log wrote, as they would stand in its file.
#[derive(Clone, Default)]
struct Lines(Arc<Mutex<Vec<u8>>>);

impl std::io::Write for Lines {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Lines {
    type Writer = Self;

    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}

/// Everything the agent's log says while it carries `f`'s task out, at every level.
fn logged(f: &mut Fixture) -> String {
    let lines = Lines::default();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(lines.clone())
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .finish();
    // How the task ends is other tests' to say: this one reads what was logged on the way.
    tracing::subscriber::with_default(subscriber, || f.carried());
    let said = lines.0.lock().unwrap();
    String::from_utf8_lossy(&said).into_owned()
}

/// What the model was told its call answered: the tool result its next request carries.
fn answered(f: &Fixture) -> String {
    let bodies = f.script.bodies.lock().unwrap().clone();
    let messages = bodies
        .last()
        .and_then(|body| body["messages"].as_array().cloned())
        .unwrap_or_default();
    messages
        .iter()
        .flat_map(|message| message["content"].as_array().cloned().unwrap_or_default())
        .find(|block| block["type"] == "tool_result")
        .and_then(|block| block["content"].as_str().map(str::to_owned))
        .unwrap_or_else(|| panic!("the call gets a tool result: {messages:?}"))
}

tilo_test_env::scenario! {
    fn the_log_names_a_call_and_holds_nothing_of_it;

    phase nothing_asked_written_or_answered_stands_in_a_line;
    // Three tasks, each logged whole: a call that is carried out, a step its application
    // refuses, and a call refused for its shape. The lines this test reads are shown to have
    // been written before anything is said of what they hold.
    {
        let mut ran = fixture(
            &format!("write {ASKED} into the document"),
            World::default(),
            vec![
                turn(&[invoke("type-text", &serde_json::json!({"text": WROTE}))], 16),
                said_turn("I typed it into the document."),
            ],
        );
        let ran_log = logged(&mut ran);

        let mut refused = fixture(
            "write a line into the document",
            World {
                refuse: vec!["type-text".into()],
                ..World::default()
            },
            vec![
                turn(&[invoke("type-text", &serde_json::json!({"text": WROTE}))], 16),
                said_turn("The document did not take the line."),
            ],
        );
        let refused_log = logged(&mut refused);

        let mut miscalled = fixture(
            "what is on my task list?",
            World::default(),
            vec![
                turn(
                    &[(tools::INVOKE, serde_json::json!({"parameters": {"text": MISCALLED}}))],
                    24,
                ),
                turn(&[finish("Three things, listed in your Tasks.", true)], 24),
            ],
        );
        let miscalled_log = logged(&mut miscalled);

        for (log, line) in [
            (&ran_log, "task request"),
            (&ran_log, "agent tool call"),
            (&ran_log, "agent tool result"),
            (&refused_log, "agent invoke failed"),
            (&refused_log, "agent tool result"),
            (&miscalled_log, "the model's calls were refused"),
        ] {
            assert!(log.contains(line), "the log has the line {line:?}:\n{log}");
        }
        let told = answered(&refused);
        assert!(
            told.contains(REFUSED),
            "the model is told the application's own words: {told}"
        );

        assert!(
            !ran_log.contains(ASKED),
            "what the person asked stands in the log:\n{ran_log}"
        );
        assert!(
            !ran_log.contains(WROTE),
            "what a call wrote stands in the log:\n{ran_log}"
        );
        assert!(
            !refused_log.contains(WROTE),
            "what a refused step's call wrote stands in the log:\n{refused_log}"
        );
        assert!(
            !refused_log.contains(REFUSED),
            "what a tool answered stands in the log:\n{refused_log}"
        );
        assert!(
            !miscalled_log.contains(MISCALLED),
            "what a misshapen call wrote stands in the log:\n{miscalled_log}"
        );

        // What a line keeps of a call: its tool, and each argument by its name and its size.
        assert!(
            ran_log.contains("tool=invoke"),
            "a call's line names its tool:\n{ran_log}"
        );
        assert!(
            ran_log.contains("args=operation:"),
            "a call's line names each argument with the bytes of its value:\n{ran_log}"
        );
    }
}
