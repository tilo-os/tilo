//! Changing a waiting call before approving it: the shell's question card opens the form its
//! ask offers, prefilled with the call as it was asked, and its approve answers the record with
//! only what the person changed, by each field's path in the call's arguments.

use std::cell::RefCell;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const SHELL: &str = "so.tilo.shell";

/// What the shell's views called, in order.
#[derive(Clone, Default)]
struct Called(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Called {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

fn ada() -> Actor {
    Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada"))
}

/// The shell's package over a host that runs it, with the platform's packages its views read,
/// staged as its scenario "a call to change before approving": Scout's alarm at 07:00 waiting
/// for Ada, whose form offers its time, its label and whether it rings on weekdays only.
fn shell() -> (Host, Called) {
    let host = Host::new(ada(), || None).unwrap();
    for meta in tilolang_tilo::platform_apps::platform_packages().unwrap() {
        let app = meta.app.clone();
        host.runtime()
            .load_foreign(meta)
            .unwrap_or_else(|why| panic!("{app}: {why}"));
    }
    let compiled = tilolang_tilo::shell::compile().unwrap_or_else(|why| panic!("{why}"));
    host.load_compiled(compiled.last().expect("the shell's packages"))
        .unwrap();
    let called = Called::default();
    host.runtime().set_foreign_invoker(called.clone());
    host.root_of(SHELL).unwrap();
    let scenario = host
        .package(SHELL)
        .unwrap()
        .meta
        .scenarios
        .iter()
        .find(|scenario| scenario.name == "a call to change before approving")
        .expect("the shell stages a call to change")
        .action
        .clone();
    host.runtime()
        .invoke_action(&ada(), (scenario.as_str(), vec![]))
        .unwrap();
    (host, called)
}

/// The questions waiting, as the notification centre lists them.
fn waiting(host: &Host) -> ViewInstance {
    let view = host
        .package(SHELL)
        .unwrap()
        .meta
        .views
        .iter()
        .find(|view| view.name == "asking_list")
        .expect("the shell lists what waits")
        .id
        .clone();
    host.runtime().open_view(&ada(), &view, vec![]).unwrap()
}

fn labelled<'a>(tree: &'a UiNode, kind: &str, label: &str) -> &'a UiNode {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a {kind} labelled {label}"))
}

/// The arguments of the record's `resolve` the shell called, by name.
fn resolved(called: &Called) -> Vec<(String, Value)> {
    let calls = called.0.borrow();
    let Some(Invocation::Action { action, args, .. }) = calls.last() else {
        panic!("the card answered the record: {calls:?}");
    };
    assert_eq!(action.as_str(), "resolve", "{calls:?}");
    args.clone()
}

fn argument<'a>(args: &'a [(String, Value)], name: &str) -> Option<&'a Value> {
    args.iter()
        .find(|(named, _)| named == name)
        .map(|(_, value)| value)
}

/// Open the card's form: what it lets the person change, in fields.
fn changing(card: &mut ViewInstance) {
    let change = labelled(card.tree(), "ui.Button", "Change").key.clone();
    card.press(&change).unwrap();
    card.poll_patches().unwrap();
}

tilo_test_env::scenario! {
    fn a_person_changes_a_waiting_call_in_its_form_as_they_approve_it;

    phase the_form_opens_prefilled_with_the_call_as_asked;
    {
        let (host, _) = shell();
        let mut card = waiting(&host);
        assert!(card.tree().of_kind("ui.TextInput").is_empty(), "the form waits to be opened");
        changing(&mut card);
        let inputs: Vec<String> = card
            .tree()
            .of_kind("ui.TextInput")
            .iter()
            .map(|input| input.key.clone())
            .collect();
        assert_eq!(inputs.len(), 2, "a field per text it offers");
        assert_eq!(card.bound_value(&inputs[0]), Some(Value::text("07:00")));
        assert_eq!(card.bound_value(&inputs[1]), Some(Value::text("Wake up")));
        assert_eq!(card.tree().of_kind("ui.Toggle").len(), 1, "a flag turns");
    }

    phase the_approve_carries_only_what_the_person_changed;
    {
        let (host, called) = shell();
        let mut card = waiting(&host);
        changing(&mut card);
        let at = card.tree().of_kind("ui.TextInput")[0].key.clone();
        card.edit(&at, &Value::text("07:30")).unwrap();
        card.poll_patches().unwrap();
        let approve = labelled(card.tree(), "ui.Button", "Set alarm").key.clone();
        card.press(&approve).unwrap();
        let args = resolved(&called);
        assert_eq!(argument(&args, "resolution"), Some(&Value::text("approve")));
        let edits = argument(&args, "edits").expect("the approve says what changed");
        assert_eq!(
            host.runtime().project(edits),
            serde_json::json!([{"path": ["alarm", "at"], "text": "07:30"}]),
            "the time alone, by its path: the label and the flag stay as asked"
        );
    }

    phase an_approve_with_nothing_changed_answers_as_asked;
    {
        let (host, called) = shell();
        let card = waiting(&host);
        let approve = labelled(card.tree(), "ui.Button", "Set alarm").key.clone();
        card.press(&approve).unwrap();
        let args = resolved(&called);
        assert_eq!(argument(&args, "resolution"), Some(&Value::text("approve")));
        assert!(
            matches!(argument(&args, "edits"), None | Some(Value::Opt(None))),
            "nothing changed: {args:?}"
        );
    }
}
