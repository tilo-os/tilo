//! A press that needs the person's yes is asked where they pressed: the shell's question stands
//! beside the control the press answered, in the window it was pressed in, and that answer is the
//! asked record's own `resolve`. Escape, a press off it and its own no each decline the record. A
//! call that leaves the system asks the same, beside its button, and offers no Shift that would
//! skip it.

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{UiNode, Value};
use tilolang_tilo::host::Host;

use crate::shell_staged::{Called, SHELL, ada, staged};

/// The shell staged as its scenario "a press that needs a yes": Notes' Share, pressed at its
/// button in the Notes window, waits for Ada.
fn shell() -> (Host, Called) {
    staged("a press that needs a yes")
}

/// The desktop as the shell draws the person's layout.
fn desktop(host: &Host) -> tilolang_runtime::ViewInstance {
    let meta = &host.package(SHELL).unwrap().meta;
    let layout = meta
        .types
        .iter()
        .find(|ty| ty.name == "Layout")
        .expect("the shell declares its layout")
        .id
        .clone();
    let layout = host.root_resource(SHELL, &layout).unwrap();
    let view = meta
        .views
        .iter()
        .find(|view| view.name == "shell_view")
        .expect("the shell draws the layout")
        .id
        .clone();
    host.runtime()
        .open_view(&ada(), &view, vec![Value::resource(&layout)])
        .unwrap()
}

/// Every text the nodes under `tree` say, as their props carry it.
fn texts(tree: &UiNode) -> Vec<String> {
    let mut out = Vec::new();
    let mut at = vec![tree];
    while let Some(node) = at.pop() {
        out.extend(node.props.values().filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        }));
        at.extend(node.children.iter());
    }
    out
}

/// `value`, out of the option it may be given in.
fn given(value: Option<&Value>) -> Option<&Value> {
    match value? {
        Value::Opt(inner) => inner.as_deref(),
        value => Some(value),
    }
}

tilo_test_env::scenario! {
    fn a_press_that_needs_a_yes_is_asked_where_it_was_pressed;

    phase the_confirm_stands_at_the_press_with_what_the_call_would_do;
    {
        let (host, _) = shell();
        let desktop = desktop(&host);
        let tree = desktop.tree();
        assert!(
            tree.of_kind("ui.ModalDialog").is_empty(),
            "no question in the middle of the window"
        );
        let [confirm] = tree.of_kind("ui.Popover")[..] else {
            panic!("one confirm stands at the press: {tree:#?}");
        };
        assert_eq!(
            given(confirm.props.get("anchor")),
            Some(&Value::List(vec![
                Value::Float(592.0),
                Value::Float(196.0),
                Value::Float(96.0),
                Value::Float(32.0)
            ])),
            "beside the control of the window the press answered, never over it"
        );
        let shown = texts(confirm);
        for fact in ["Mia Clayton", "Can edit", "Release planning"] {
            assert!(
                shown.iter().any(|text| text.contains(fact)),
                "the confirm shows what the call would do ({fact}): {shown:?}"
            );
        }
        assert!(
            shown.iter().any(|text| text == "Hold Shift to skip"),
            "Undo takes a share back, so Shift skips it next time: {shown:?}"
        );
        let share = confirm
            .of_kind("ui.Button")
            .into_iter()
            .find(|node| node.props.get("label") == Some(&Value::text("Share")))
            .expect("the confirm answers with what was pressed for");
        assert_eq!(
            given(share.props.get("role")),
            Some(&Value::text("primary")),
            "its main answer"
        );
    }

    phase its_confirm_answers_the_records_resolve;
    {
        let (host, called) = shell();
        let desktop = desktop(&host);
        desktop.press(&confirm_button(&desktop, "Share")).unwrap();
        assert_eq!(
            answers(&called),
            ["approve"],
            "pressing again at the same spot says yes, and only that"
        );
    }

    phase its_yes_reaches_the_record_where_nothing_publishes_its_resolve;
    {
        // A session publishes no verb on an asked record: the record is the workspace's, and
        // its answer goes to the dispatch as the host's own action on it, its yes as its no.
        let (host, called) = staged("a press that needs a yes");
        let desktop = desktop(&host);
        desktop
            .press(&confirm_button(&desktop, "Share"))
            .unwrap_or_else(|why| panic!("the yes reaches the record: {why}"));
        assert_eq!(answers(&called), ["approve"], "the yes is the record's resolve");
        desktop
            .press(&confirm_button(&desktop, "Don't allow"))
            .unwrap_or_else(|why| panic!("the no reaches the record: {why}"));
    }

    phase escape_or_a_press_outside_it_declines;
    {
        // Escape and a press off the panel are the popover's one dismissal.
        let (host, called) = shell();
        let desktop = desktop(&host);
        let confirm = desktop.tree().of_kind("ui.Popover")[0].key.clone();
        desktop.fire(&confirm, "dismiss").unwrap();
        assert_eq!(
            answers(&called),
            ["decline"],
            "closing the question says no to the record, and nothing of the call runs"
        );
    }

    phase its_dont_allow_declines;
    {
        let (host, called) = shell();
        let desktop = desktop(&host);
        desktop
            .press(&confirm_button(&desktop, "Don't allow"))
            .unwrap();
        assert_eq!(
            answers(&called),
            ["decline"],
            "the confirm's own no says no to the record, and nothing of the call runs"
        );
    }
}

tilo_test_env::scenario! {
    fn a_send_that_leaves_asks_beside_its_button_and_runs_only_on_the_yes;

    phase it_stands_beside_the_send_and_nothing_sent;
    {
        let (host, called) = staged("a send that needs a yes");
        assert!(answers(&called).is_empty(), "nothing was answered before the person saw it");
        let desktop = desktop(&host);
        let tree = desktop.tree();
        assert!(
            tree.of_kind("ui.ModalDialog").is_empty(),
            "no question in the middle of the window"
        );
        let [confirm] = tree.of_kind("ui.Popover")[..] else {
            panic!("one question stands beside the Send: {tree:#?}");
        };
        assert_eq!(
            given(confirm.props.get("anchor")),
            Some(&Value::List(vec![
                Value::Float(858.0),
                Value::Float(644.0),
                Value::Float(88.0),
                Value::Float(32.0)
            ])),
            "beside the Send button the person pressed, never over it"
        );
        let shown = texts(confirm);
        assert!(
            shown.iter().any(|text| text.contains("grace@hopper.example")),
            "it says whom the letter goes to: {shown:?}"
        );
        assert!(
            !shown.iter().any(|text| text == "Hold Shift to skip"),
            "Shift never skips what leaves the system: {shown:?}"
        );
        assert!(
            confirm
                .of_kind("ui.Button")
                .into_iter()
                .any(|node| node.props.get("label") == Some(&Value::text("Send"))),
            "its main answer is what was pressed for: {shown:?}"
        );
    }

    phase its_yes_sends_once;
    {
        let (host, called) = staged("a send that needs a yes");
        let desktop = desktop(&host);
        desktop.press(&confirm_button(&desktop, "Send")).unwrap();
        assert_eq!(
            answers_of(&called, "studio/send-1"),
            ["approve"],
            "the yes is the record's resolve, and the call runs once"
        );
    }

    phase its_no_sends_nothing;
    {
        let (host, called) = staged("a send that needs a yes");
        let desktop = desktop(&host);
        desktop
            .press(&confirm_button(&desktop, "Don't allow"))
            .unwrap();
        assert_eq!(
            answers_of(&called, "studio/send-1"),
            ["decline"],
            "the way out runs nothing"
        );
    }

    phase escape_withdraws_it;
    {
        let (host, called) = staged("a send that needs a yes");
        let desktop = desktop(&host);
        let confirm = desktop.tree().of_kind("ui.Popover")[0].key.clone();
        desktop.fire(&confirm, "dismiss").unwrap();
        assert_eq!(
            answers_of(&called, "studio/send-1"),
            ["decline"],
            "closing the question says no to the record, and nothing of the call runs"
        );
    }
}

/// The key of the confirm's button labelled `label`.
fn confirm_button(desktop: &tilolang_runtime::ViewInstance, label: &str) -> String {
    desktop
        .tree()
        .of_kind("ui.Popover")
        .into_iter()
        .flat_map(|confirm| confirm.of_kind("ui.Button"))
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("the confirm's {label}"))
        .key
        .clone()
}

/// Where the keyboard starts when a question stands in the shell's window: on the answer the
/// person's own press started, never on "always let", and on the way out where nobody here
/// started it.
#[cfg(target_os = "linux")]
mod focus {
    use super::texts;
    use crate::shell_staged::StagedWindow;

    /// The shell staged as its scenario `scenario`, its desktop in a window that takes the
    /// keyboard as it opens.
    fn staged(scenario: &str) -> StagedWindow {
        let mut theme = tilo_ui_base::Theme::dark();
        theme.reduce_motion = true;
        StagedWindow::staged(scenario, (1280, 800), theme)
    }

    #[test]
    fn a_press_that_needs_a_yes_opens_on_the_answer_that_was_pressed() {
        let shell = staged("a press that needs a yes");
        let tree = shell.instance.root_tree().expect("the window's tree");
        let [confirm] = tree.of_kind("ui.Popover")[..] else {
            panic!("one confirm stands at the press: {tree:#?}");
        };
        assert!(
            texts(confirm)
                .iter()
                .any(|shown| shown == "Always let Notes share notes"),
            "the confirm offers a permanent permission beside its answer"
        );
        let holds = shell.holds_keyboard();
        assert_eq!(
            holds.as_deref(),
            Some("Share"),
            "the keyboard starts on the answer the press under the confirm gives"
        );
        assert_ne!(
            holds.as_deref(),
            Some("Always let Notes share notes"),
            "a permanent permission never takes the keyboard as the confirm opens"
        );
    }

    #[test]
    fn an_irreversible_press_opens_on_the_way_out() {
        let shell = staged("a send that needs a yes");
        assert_eq!(
            shell.holds_keyboard().as_deref(),
            Some("Don't allow"),
            "Enter must never send what nothing here can take back"
        );
    }

    #[test]
    fn an_agents_question_opens_on_the_way_out() {
        let shell = staged("a call that writes another record");
        assert_eq!(
            shell.holds_keyboard().as_deref(),
            Some("Don't allow"),
            "nothing here started the call, so the keyboard starts on declining it"
        );
    }
}

/// Every call the shell made, each the record's `resolve` with the resolution it gave: all that
/// reached the workspace, which runs the asked call only on an approve. Each goes to the record
/// the scenario leaves waiting (`share-1` in `studio`: a record is kept under its invocation's id,
/// the last step of the id the scenario stages), answered by the invocations provider of the
/// workspace its address names, as the session sends it: nothing here publishes `resolve`, as
/// nothing does there.
fn answers(called: &Called) -> Vec<String> {
    answers_of(called, "studio/share-1")
}

/// [`answers`] of the record `record`, as the scenario stages its id.
fn answers_of(called: &Called, record: &str) -> Vec<String> {
    called
        .0
        .borrow()
        .iter()
        .map(|call| {
            let Invocation::Action {
                action,
                args,
                target,
                ..
            } = call
            else {
                panic!("the shell called only the record's resolve: {call:?}");
            };
            assert_eq!(action.as_str(), "resolve", "{call:?}");
            let hole = target
                .as_ref()
                .expect("the call names the record it answers");
            assert!(
                tilolang::is_invocation_type(&hole.ty),
                "the call is on an asked record: {call:?}"
            );
            assert_eq!(
                hole.id_string(),
                record,
                "the asked record is the one answered: {call:?}"
            );
            let resource = record.rsplit_once('/').map_or(record, |(_, last)| last);
            let sent = tilolang_tilo::call::Callees::default()
                .call(call)
                .unwrap_or_else(|_| panic!("the record's call is sent: {call:?}"));
            assert_eq!(
                (
                    &sent["provider"],
                    &sent["workspace"],
                    &sent["resource"],
                    &sent["action"]
                ),
                (
                    &serde_json::json!(tilo_action_services::workspace_invocations::PROVIDER),
                    &serde_json::json!("studio"),
                    &serde_json::json!(resource),
                    &serde_json::json!("resolve"),
                ),
                "the workspace's invocations provider answers it: {sent}"
            );
            match args.iter().find(|(name, _)| name == "resolution") {
                Some((_, Value::Text(resolution))) => resolution.clone(),
                other => panic!("a resolution: {other:?}"),
            }
        })
        .collect()
}
