//! A press that needs the person's yes is asked where they pressed: the shell's confirm stands
//! at the point of the window the press was in, its answer that does what they pressed for under
//! that point, and that answer is the asked record's own `resolve`.

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{UiNode, Value};
use tilolang_tilo::host::Host;

use crate::shell_staged::{Called, SHELL, ada, staged};

/// The shell staged as its scenario "a press that needs a yes": Notes' Share, pressed at
/// (640, 212) in the Notes window, waits for Ada.
fn shell() -> (Host, Called) {
    staged("a press that needs a yes", &["studio/invocations/share-1"])
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
            given(confirm.props.get("at")),
            Some(&Value::List(vec![Value::Float(640.0), Value::Float(212.0)])),
            "at the point of the window the press was in"
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
            "its main answer, which the confirm stands under the press"
        );
    }

    phase its_confirm_answers_the_records_resolve;
    {
        let (host, called) = shell();
        let desktop = desktop(&host);
        let share = desktop
            .tree()
            .of_kind("ui.Popover")
            .into_iter()
            .flat_map(|confirm| confirm.of_kind("ui.Button"))
            .find(|node| node.props.get("label") == Some(&Value::text("Share")))
            .expect("the confirm's Share")
            .key
            .clone();
        desktop.press(&share).unwrap();
        let calls = called.0.borrow();
        let Some(Invocation::Action { action, args, .. }) = calls.last() else {
            panic!("the confirm answered the record: {calls:?}");
        };
        assert_eq!(action.as_str(), "resolve", "{calls:?}");
        assert_eq!(
            args.iter()
                .find(|(name, _)| name == "resolution")
                .map(|(_, value)| value),
            Some(&Value::text("approve")),
            "pressing again at the same spot says yes"
        );
    }
}
