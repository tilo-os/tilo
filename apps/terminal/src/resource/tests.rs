//! The session record: what it holds, what stays out of it, and the read
//! of the live screen through the view.

use super::*;
use ui::framework::testing::TestApp;
use ui::tilo_resource::{Invoke, Replica};

fn feed(app: &mut TestApp<TerminalShell>, bytes: &[u8]) {
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.feed(bytes))
        .expect("the terminal surface is hosted");
    app.settle();
}

fn target_action(target: &SessionTarget) -> Invoke<Terminal> {
    Terminal::RECORD_TARGET.with(target.encode())
}

#[test]
fn read_reports_the_live_screen_and_where_the_session_runs() {
    let mut app = TestApp::<TerminalShell>::new().expect("terminal");
    app.invoke(target_action(&SessionTarget::default_environment()))
        .expect("recorded target");
    feed(&mut app, b"\x1b]2;build\x07recorded output\r\n");
    let read = app
        .call("read-screen", serde_json::json!({}))
        .expect("read is published")
        .expect("read answers with what the session shows");
    assert!(read["output"].as_str().unwrap().contains("recorded output"));
    assert_eq!(read["title"], "build");
    assert_eq!(
        read["target"],
        serde_json::to_value(SessionTarget::default_environment()).unwrap()
    );
    assert_eq!(read["cursor"]["row"], 1);
    assert!(
        read.get("lifecycle").is_none(),
        "the read names no local process lifecycle"
    );

    let screen = Screen {
        main: vec![std::rc::Rc::new(crate::screen::Line {
            text: "é".repeat(40_000),
            attrs: Vec::new(),
        })],
        ..Screen::default()
    };
    let (output, truncated) = readable_output(&screen);
    assert!(truncated);
    assert_eq!(output.len(), 64 * 1024);
    assert!(output.chars().all(|character| character == 'é'));
}

/// What a process prints is live: output of any volume writes nothing to
/// the record and adds no version; the record keeps where the session runs.
#[test]
fn output_stays_out_of_the_record() {
    let mut app = TestApp::<TerminalShell>::new().expect("terminal");
    feed(&mut app, b"first\r\n");
    let versions = app.versions().len();
    let heads = app.read_document(|doc| doc.clone().get_heads());
    for batch in 0..50 {
        let mut out = Vec::new();
        for line in 0..20 {
            out.extend_from_slice(
                format!("\x1b]2;step {batch}\x07line {batch}:{line}\r\n").as_bytes(),
            );
        }
        feed(&mut app, &out);
    }
    assert_eq!(app.versions().len(), versions, "output adds no version");
    assert_eq!(
        app.read_document(|doc| doc.clone().get_heads()),
        heads,
        "output writes nothing"
    );
    let state = app.state();
    assert!(state.pane_id().len() == 36, "the process claimed its pane");
    assert!(!app.undo(), "nothing the session recorded is an undo step");
}

#[test]
fn the_record_takes_valid_facts_only() {
    let mut session = Replica::<Terminal>::new();
    assert!(
        session
            .apply(Terminal::RECORD_TARGET.with("not a target".to_owned()))
            .is_err()
    );
    assert!(
        session
            .apply(Terminal::CLAIM_PANE.with("pane".to_owned()))
            .is_err()
    );
    assert!(
        session
            .apply(Terminal::RECORD_RELAY_SESSION.with(Some("xyz".to_owned())))
            .is_err()
    );
    let relay = "0123456789abcdef0123456789abcdef".to_owned();
    session
        .apply(Terminal::RECORD_RELAY_SESSION.with(Some(relay.clone())))
        .unwrap();
    session.apply(target_action(&SessionTarget::Host)).unwrap();
    let state = session.state().unwrap();
    assert_eq!(state.relay(), Some(relay.as_str()));
    assert_eq!(state.session_target(), Some(SessionTarget::Host));
    session
        .apply(Terminal::RECORD_RELAY_SESSION.with(None::<String>))
        .unwrap();
    assert_eq!(session.state().unwrap().relay(), None);
}

/// Two devices claiming a pane at once converge on one; an agent records
/// in one call.
#[test]
fn replicas_converge_on_one_pane() {
    let mut ours = Replica::<Terminal>::new();
    let mut theirs = ours.fork();
    ours.apply(Terminal::CLAIM_PANE.with(uuid::Uuid::new_v4().to_string()))
        .unwrap();
    theirs
        .apply(Terminal::CLAIM_PANE.with(uuid::Uuid::new_v4().to_string()))
        .unwrap();
    ours.sync_with(&mut theirs).unwrap();
    let pane = ours.state().unwrap().pane_id().to_owned();
    assert!(!pane.is_empty());
    assert_eq!(theirs.state().unwrap().pane_id(), pane);

    let mut app = TestApp::<TerminalShell>::new().expect("terminal");
    let target = SessionTarget::default_environment().encode();
    app.call("record-target", serde_json::json!({ "target": target }))
        .unwrap();
    assert_eq!(
        app.state().session_target(),
        Some(SessionTarget::default_environment())
    );
}

#[test]
fn a_session_nested_in_another_record_stays_under_its_object() {
    let mut doc = Doc::new();
    let panel = ui::tilo_resource::Slot::Key(&automerge::ROOT, "terminal_panel")
        .object(&mut doc, automerge::ObjType::Map)
        .unwrap();
    let pane = uuid::Uuid::new_v4().to_string();
    let action = Terminal::CLAIM_PANE.with(pane.clone()).build(0).unwrap();
    Terminal::apply_under(&action, &mut doc, &panel).unwrap();
    assert_eq!(Terminal::read_under(&doc, &panel).unwrap().pane_id(), pane);
    assert!(
        Terminal::read_under(&doc, &automerge::ROOT)
            .unwrap()
            .pane
            .is_none(),
        "nothing lands at the document root"
    );
}

/// What the session recorded is history the person reads in their
/// language.
#[test]
fn history_entries_render_as_the_apps_sentences() {
    let mut app = TestApp::<TerminalShell>::new().expect("terminal");
    app.invoke(target_action(&SessionTarget::Host)).unwrap();
    let sentences = app.sentences(&app.history());
    assert!(
        sentences
            .iter()
            .any(|sentence| sentence == "Moved the session"),
        "{sentences:?}"
    );
}

#[test]
fn the_session_publishes_its_effects_and_the_links_on_screen() {
    let mut app = ui::framework::testing::TestApp::<TerminalShell>::new().expect("terminal");
    let mut output = Vec::new();
    for line in 0..100 {
        output.extend_from_slice(format!("line {line:03}\r\n").as_bytes());
    }
    output.extend_from_slice(
        b"see \x1b]8;;https://example.test/docs\x1b\\docs\x1b]8;;\x1b\\ \
          \x1b]8;;file:///home/me/.ssh/id_ed25519\x1b\\key\x1b]8;;\x1b\\\r\n",
    );
    app.with_surface(|terminal: &mut crate::TerminalView| terminal.feed(&output))
        .expect("the terminal surface is hosted");
    app.settle();

    let published = app.agent_surface().to_string();
    // Showing a run's item is offered on the runs (run_tests).
    for effect in TerminalEffect::declarations()
        .into_iter()
        .filter(|effect| effect.id != "open-item")
    {
        assert!(
            published.contains(&format!("\"{}\"", effect.id)),
            "`{}` is published: {published}",
            effect.id
        );
    }
    assert_eq!(
        published
            .matches("\"resource_kind\":\"terminal.link\"")
            .count(),
        1,
        "the public link is a row, the local file link is not: {published}"
    );

    // Scrolling to the oldest output through the door moves the view.
    app.effect(&TerminalEffect::HistoryTop)
        .expect("history-top is offered while older output is retained");
    let scrolled = app
        .with_surface(|terminal: &mut crate::TerminalView| terminal.scroll_offset())
        .expect("the terminal surface is hosted");
    assert!(scrolled > 0, "the view scrolled back");

    // A reviewed command may not smuggle control characters.
    assert!(
        app.call(
            "run",
            serde_json::json!({"command": "printf first\nprintf second"})
        )
        .is_err()
    );

    // An emulating terminal keeps its own screen when the record is presented.
    let mut widget = TerminalWidget::spawn_headless().expect("headless terminal");
    widget.feed(b"hello");
    ResourceSurface::<Terminal, TerminalEffect>::present(
        &mut widget,
        &std::rc::Rc::new(Terminal::default()),
    );
    assert!(
        widget.grid_text().contains("hello"),
        "the record must not replace the screen of a terminal running its own output"
    );
}

/// A scenario's output on screen through `--shots`' own path, with the
/// view state staged over it.
fn captured<T>(scenario: &str, read: impl FnOnce(&mut crate::TerminalView) -> T) -> T {
    let mut app = ui::framework::testing::TestApp::<TerminalShell>::capture(Some(scenario))
        .unwrap_or_else(|error| panic!("{scenario} stages: {error}"));
    app.with_surface(read)
        .expect("the terminal surface is hosted")
}

#[test]
fn scenarios_stage_their_output_and_view_state() {
    let (open, counter) = captured("find", |terminal| {
        (terminal.find_is_open(), terminal.find_counter())
    });
    assert!(open, "the find scenario opens the find bar");
    assert_eq!(
        counter,
        Some(("build step 042".into(), Some((1, 1)))),
        "one line matches `build step 042`"
    );

    let selected = captured("shell-select", |terminal| terminal.test_selected_text());
    assert_eq!(
        selected.as_deref(),
        Some("primes below 20: 2 3 5 7 11 13 17 19"),
        "the printed line is selected, not the command that echoed it"
    );

    let (scrolled, oldest) = captured("scrollback", |terminal| {
        (
            terminal.scroll_offset(),
            terminal.visible_lines().join("\n"),
        )
    });
    assert_eq!(scrolled, 20);
    assert!(oldest.contains("log entry 040"), "{oldest}");

    assert!(captured("tui", |terminal| terminal.is_alt_screen()));
    assert!(captured("ended", |terminal| terminal.is_disconnected()));
    assert!(captured("empty-menu", |terminal| terminal.context_menu_is_open()));
    let target = captured("host", |terminal| terminal.session_target().cloned());
    assert_eq!(target, Some(crate::target::SessionTarget::Host));
}

#[test]
fn native_replacement_preserves_private_effect_contract() {
    use ui::tilo_resource::Effect as _;
    let effect = TerminalEffect::ReplaceEditorSelection {
        session: "original-session".into(),
        lease: "private-lease".into(),
        text: "private transcript".into(),
    };
    let debug = format!("{effect:?}");
    assert!(!debug.contains("private"));
    assert!(!debug.contains("original-session"));
    let declaration = TerminalEffect::declarations()
        .into_iter()
        .find(|declaration| declaration.id == effect.id())
        .unwrap();
    assert!(declaration.device_only);
    let text = declaration
        .params
        .iter()
        .find(|param| param.name == "text")
        .unwrap();
    assert_eq!(text.ty, ui::tilo_actions::ParamType::Secret);
    assert_eq!(effect.arguments()["text"], "private transcript");
}
