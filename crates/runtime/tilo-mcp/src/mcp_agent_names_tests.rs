//! An app is named as a model writes it, a name read more freely reaches nothing more, and a
//! refusal names the call that works.
use super::*;

/// The word processor as a model writes it: its id, the last part of its id and its name, with
/// a hyphen, an underscore, a blank or nothing between its words, in any case, and the word
/// its name begins with.
const WRITER: [&str; 9] = [
    "word-processor",
    "word_processor",
    "Word Processor",
    "word processor",
    "WordProcessor",
    "WORD_PROCESSOR",
    "Word",
    "so.tilo.word-processor",
    "So.Tilo.Word_Processor",
];

/// How often the workspace was asked to open the word processor.
fn opened(notes: &Notes) -> usize {
    notes
        .invoked()
        .iter()
        .filter(|(resource, action, _)| {
            resource == "app.so.tilo.word-processor" && action == "open"
        })
        .count()
}

/// `word_processor`, `Word Processor`, `Word` and `so.tilo.word-processor` are one app in every
/// tool: a model that writes the name its own way is not sent back to look the spelling up.
#[tokio::test]
async fn an_app_is_one_app_however_a_model_spells_its_name() {
    let notes = Notes::new(true).installed(Installed::WithWriter);
    let server = agent(&notes);
    // The app that is open: its id in any case, and the start of its name.
    for spelled in ["notes", "Notes", "So.Tilo.Notes", "note"] {
        let (text, failed) = tool(&server, "tilo_ls", json!({"path": spelled})).await;
        assert!(!failed && text.contains("#new-page"), "{spelled}: {text}");
    }
    // The installed one is found by every spelling, and what a call on it reaches is it.
    for spelled in WRITER {
        let (text, failed) = tool(&server, "tilo_ls", json!({"path": spelled})).await;
        // A listing of an installed app starts it and lists it: what is listed is the word
        // processor, however its name was spelled.
        assert!(
            !failed && text.contains("(so.tilo.word-processor)"),
            "{spelled}: {text}"
        );
        let before = opened(&notes);
        let open = json!({"path": format!("{spelled}#open")});
        let (text, _) = tool(&server, "tilo_do", open).await;
        assert_eq!(
            opened(&notes),
            before.saturating_add(1),
            "{spelled}: the workspace was asked to open the word processor: {text}"
        );
    }
    // A search narrowed to an app takes the name the same way.
    let narrowed = json!({"query": "Groceries", "app": "note"});
    let (text, failed) = tool(&server, "tilo_find", narrowed).await;
    assert!(!failed && !text.contains("no app"), "{text}");
    // A word that is no app's name and begins none is still no app.
    let (text, failed) = tool(&server, "tilo_ls", json!({"path": "processor"})).await;
    assert!(failed && text.contains("unknown_app"), "{text}");
}

/// A name read more freely reaches nothing more: an app the workspace does not let this agent
/// open stays closed to it under every spelling, in every tool that would start it, and the
/// workspace is asked nothing on the agent's behalf.
#[tokio::test]
async fn a_name_spelled_freely_opens_nothing_the_agent_may_not_open() {
    let notes = Notes::new(true).installed(Installed::WriterClosedToAgent);
    let server = agent(&notes);
    for spelled in WRITER {
        for (name, arguments) in [
            ("tilo_do", json!({"path": format!("{spelled}#open")})),
            (
                "tilo_do",
                json!({"path": format!("{spelled}#append-paragraph"), "args": {"text": "Sloe."}}),
            ),
            ("tilo_view", json!({"path": spelled})),
        ] {
            let (text, failed) = tool(&server, name, arguments).await;
            assert!(
                failed && text.contains("may not open"),
                "{name} {spelled}: {text}"
            );
        }
        // It is named to the agent as closed, never as an app to start.
        let (text, _) = tool(&server, "tilo_ls", json!({"path": spelled})).await;
        assert!(
            text.contains("may not open") && !text.contains("#open"),
            "{spelled}: {text}"
        );
    }
    assert!(
        !notes
            .invoked()
            .iter()
            .any(|(resource, _, _)| resource.contains("word-processor")),
        "nothing was asked of the workspace on the agent's behalf"
    );
}

/// A refusal carries the call that works, so the next step is the work and not a look: a view
/// where no window answers, a sealed document's own path where none is read outside its app,
/// a program where none is carried out.
#[tokio::test]
async fn a_refusal_names_the_call_that_works() {
    let notes = Notes::new(true);
    let server = agent(&notes);
    let (text, failed) = tool(&server, "tilo_view", json!({"path": "notes"})).await;
    assert!(
        failed
            && text.contains("no window shows notes")
            && text.contains(r#"tilo_read {"path":"notes"}"#)
            && text.contains(r#"tilo_link {"path":"notes"}"#)
            && !text.contains("lists them"),
        "{text}"
    );
    let sealed = format!(
        "{}:so.tilo.notes:abc",
        tilo_actions::remote::RESOURCE_PROVIDER
    );
    let (text, failed) = tool(&server, "tilo_read", json!({ "path": sealed })).await;
    assert!(
        failed
            && text.contains(tilo_actions::remote::RESOURCE_READS_UNAVAILABLE)
            && text.contains(r#"tilo_ls {"path":"notes"}"#),
        "{text}"
    );
    let program = json!({"program": "notes.notebook.notes"});
    let (text, failed) = tool(&server, "tilo", program).await;
    assert!(
        failed
            && text.contains("no programs")
            && text.contains("tilo_do")
            && text.contains("undo task"),
        "{text}"
    );
}
