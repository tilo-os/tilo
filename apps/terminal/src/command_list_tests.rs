//! The page's Terminal: a session's commands shown as they ended, and a
//! command typed on the line asked of whoever keeps the session.

use std::sync::Arc;

use tilo_action_services::TestProvider;
use tilo_app_test_host::TestApp;
use ui::framework::{ResourceApp, ViewCx};
use ui::tilo_actions::remote::{
    Caller, InvocationReceipt, InvokeArgument, Pending, RESOURCE_PROVIDER, ReceiptStatus,
};
use ui::tilo_resource::{Bytes, Id};
use ui::widget::Element;

use super::plain;
use crate::resource::{FinishedCommand, Terminal};

/// Terminal as a page mounts it: the command list over the session.
struct Page;

impl ResourceApp for Page {
    type Resource = Terminal;
    const ID: &'static str = ui::apps::TERMINAL;
    const TITLE: &'static str = "Terminal";

    fn view(cx: &mut ViewCx<Terminal>) -> Element {
        super::view(cx)
    }
}

/// The session keeps `command` as ended with `exit`, having printed `printed`.
fn ended(app: &mut TestApp<Page>, id: &str, command: &str, exit: i32, printed: &[u8]) {
    let finished = FinishedCommand {
        command: command.to_owned(),
        cwd: "/workspace/files".to_owned(),
        started_ms: 1_000,
        ended_ms: 3_000,
        exit,
        output: Some(Bytes::of(printed)),
        truncated: false,
        tool: String::new(),
        undoable: false,
        items: Vec::new(),
        request: None,
        id: Id::from(id),
    };
    app.invoke(Terminal::RECORD_COMMAND.with(finished)).unwrap();
}

/// The command row titled `command`, as assistive technology reads it: its
/// whole name, which names who typed it, and what is said beside it, how it
/// ended.
fn row(app: &mut TestApp<Page>, command: &str) -> Option<(String, String)> {
    let harness = app.harness();
    let node = harness.a11y_node(harness.a11y_node_named(command)?)?;
    Some((node.label()?.to_owned(), node.value()?.to_owned()))
}

#[test]
fn output_reads_as_the_screen_shows_it() {
    assert_eq!(
        plain(b"\x1b[1;31merror\x1b[0m: it broke\r\n\x1b]0;title\x07ok\n"),
        "error: it broke\nok\n"
    );
    assert_eq!(plain(b"10%\r50%\r100%\ndone"), "100%\ndone");
}

#[test]
fn each_command_shows_with_what_it_printed_and_how_it_ended() {
    let mut app = TestApp::<Page>::for_person("ada").unwrap();
    assert!(
        app.texts().iter().any(|text| text == "No commands yet"),
        "{:?}",
        app.texts()
    );
    ended(
        &mut app,
        "c1",
        "cargo build",
        101,
        b"\x1b[1;31merror\x1b[0m: it broke\n",
    );
    ended(&mut app, "c2", "ls", 0, b"src\n");
    app.settle();
    let texts = app.texts();
    for shown in ["error: it broke", "src"] {
        assert!(texts.iter().any(|text| text == shown), "{shown}: {texts:?}");
    }
    // A command is a row titled with its line; who typed it and how it ended
    // are said beside that title.
    let build = row(&mut app, "$ cargo build").unwrap_or_else(|| panic!("{texts:?}"));
    assert_eq!(build.0, "$ cargo build, ada", "{texts:?}");
    assert!(build.1.contains("Exit 101 after 2 sec"), "{build:?}");
    assert!(row(&mut app, "$ ls").is_some(), "{texts:?}");
    // A session the server keeps takes the next command here.
    assert!(
        !texts
            .iter()
            .any(|text| text.contains("someone else's computer")),
        "{texts:?}"
    );
}

#[test]
fn a_typed_command_is_asked_of_whoever_keeps_the_session() {
    let provider = Arc::new(TestProvider::new().answering(|_, request| {
        Pending::ready(Ok(InvocationReceipt {
            invocation: request.invocation,
            status: ReceiptStatus::Completed {
                output: ui::tilo_actions::ActionOutput {
                    value: Some(serde_json::json!({
                        "id": "c9", "exit": 0, "command": "echo hi", "output": "hi\n"
                    })),
                    resources: Vec::new(),
                },
            },
        }))
    }));
    let _served = ui::RemoteActions::enter(provider.clone(), Caller::local_person("ada"));
    let mut app = TestApp::<Page>::for_person("ada").unwrap();
    app.harness().click_named("terminal-command").unwrap();
    app.harness().type_text("echo hi");
    app.harness().key("Enter");
    app.settle();
    let asked = provider.asked();
    let request = asked
        .iter()
        .find(|request| request.target.address.action == Terminal::RUN.id)
        .unwrap_or_else(|| panic!("the command was asked: {asked:?}"));
    let document = app.open_resources()[0].route().to_owned();
    assert_eq!(
        request.target.provider,
        format!("{RESOURCE_PROVIDER}:{}:{document}", ui::apps::TERMINAL)
    );
    let InvokeArgument::Json(arguments) = &request.argument else {
        panic!("the command line is its argument: {:?}", request.argument);
    };
    assert_eq!(arguments["command"], "echo hi");
    // Until the session holds it, the command shows with how it ended.
    app.settle();
    let texts = app.texts();
    let echo = row(&mut app, "$ echo hi").unwrap_or_else(|| panic!("{texts:?}"));
    assert!(echo.1.contains("Exit 0"), "{echo:?}");
}

#[test]
fn a_session_on_someones_computer_shows_its_commands_without_a_line() {
    let mut app = TestApp::<Page>::for_person("ada").unwrap();
    app.invoke(Terminal::CLAIM_PANE.with((
        "6f1c2b8e-0000-4000-8000-000000000001".to_owned(),
        Some("laptop".to_owned()),
    )))
    .unwrap();
    ended(&mut app, "c1", "make", 0, b"done\n");
    app.settle();
    let texts = app.texts();
    assert!(row(&mut app, "$ make").is_some(), "{texts:?}");
    assert!(
        texts
            .iter()
            .any(|text| text.contains("someone else's computer")),
        "{texts:?}"
    );
    assert!(app.harness().click_named("terminal-command").is_err());
}
