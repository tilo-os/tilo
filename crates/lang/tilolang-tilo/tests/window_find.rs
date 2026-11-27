//! A window's find in the shell's bar: what its app found and offers reaches the field from the
//! window's own reading, and every act — stepping, an option, replacing, widening — is one of the
//! session's published verbs, so a press, a key and an agent move the same search.
//!
//! The bar's controls are `ui.FindControls`, the described component over the window's own find:
//! the shell draws them and the app remains the one search.

#![cfg(target_os = "linux")]

use tilolang_runtime::delegate::Invocation;
use tilolang_runtime::{UiNode, Value};
use tilolang_tilo::host::Host;

use crate::shell_staged::{Called, SHELL, ada, staged};

/// A window of Notes in front with its find expanded in the bar, the second of five matches
/// current, as its app last answered.
const SCENARIO: &str = "a window found in";

fn shell() -> (Host, Called) {
    staged(SCENARIO)
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

/// The bar's find controls, as the shell composed them.
fn controls(desktop: &tilolang_runtime::ViewInstance) -> &UiNode {
    let tree = desktop.tree();
    let found = tree.of_kind("ui.FindControls");
    assert_eq!(found.len(), 1, "the bar holds one find");
    found[0]
}

/// Every call the shell made, by the verb it names (the resource it is published on left off) and
/// its arguments, an optional argument as the value it holds.
fn calls(called: &Called) -> Vec<(String, Vec<(String, Value)>)> {
    fn plain(value: &Value) -> Value {
        match value {
            Value::Opt(Some(inner)) => plain(inner),
            other => other.clone(),
        }
    }
    called
        .0
        .borrow()
        .iter()
        .map(|call| {
            let Invocation::Action { action, args, .. } = call else {
                panic!("the bar's find calls actions only: {call:?}");
            };
            let verb = action
                .as_str()
                .rsplit('/')
                .next()
                .unwrap_or_default()
                .to_owned();
            (
                verb,
                args.iter()
                    .map(|(name, value)| (name.clone(), plain(value)))
                    .collect(),
            )
        })
        .collect()
}

fn given(value: Option<&Value>) -> Option<&Value> {
    match value? {
        Value::Opt(inner) => inner.as_deref(),
        value => Some(value),
    }
}

tilo_test_env::scenario! {
    fn a_windows_find_in_the_bar_reads_what_its_app_found_and_calls_its_own_verbs;

    phase the_field_shows_the_match_of_how_many_the_app_found;
    {
        let (host, _) = shell();
        let shown = desktop(&host);
        let bar = controls(&shown);
        assert_eq!(given(bar.props.get("current")), Some(&Value::Int(2)));
        assert_eq!(given(bar.props.get("total")), Some(&Value::Int(5)));
        assert_eq!(
            given(bar.props.get("acts_on")),
            Some(&Value::Text("document".into())),
            "a document's matches are stepped"
        );
        assert_eq!(
            given(bar.props.get("replaces")),
            Some(&Value::Bool(true)),
            "the note replaces, and the bar offers what it does"
        );
        assert_eq!(given(bar.props.get("regex")), Some(&Value::Bool(true)));
        assert_eq!(
            given(bar.props.get("match_case")),
            Some(&Value::Bool(false)),
            "the app runs its find as the person last left it"
        );
    }

    phase stepping_case_and_replacing_are_the_sessions_own_verbs;
    {
        let (host, called) = shell();
        let shown = desktop(&host);
        let key = controls(&shown).key.clone();
        shown
            .fire_with(&key, "step", Some(&Value::Bool(true)))
            .expect("the next match");
        shown.fire_with(&key, "step", Some(&Value::Bool(false)))
            .expect("the one before");
        shown
            .fire_with(&key, "option", Some(&Value::Text("case".into())))
            .expect("matching case");
        shown
            .fire_with(
                &key,
                "replace_all",
                Some(&Value::Text("Launch".into())),
            )
            .expect("replace all");
        let made = calls(&called);
        let named = |name: &str| {
            made
                .iter()
                .filter(|(action, _)| action == name)
                .collect::<Vec<_>>()
        };
        let steps = named("bar-search-step");
        assert_eq!(steps.len(), 2, "{made:?}");
        assert_eq!(
            steps[0].1,
            vec![
                ("tab".to_owned(), Value::Text("p1".into())),
                ("forward".to_owned(), Value::Bool(true)),
            ],
            "the next match, on the window the bar is"
        );
        assert_eq!(
            steps[1].1,
            vec![
                ("tab".to_owned(), Value::Text("p1".into())),
                ("forward".to_owned(), Value::Bool(false)),
            ],
            "and the one before"
        );
        let options = named("bar-search-option");
        assert_eq!(options.len(), 1, "{made:?}");
        assert_eq!(
            options[0].1,
            vec![
                ("tab".to_owned(), Value::Text("p1".into())),
                ("option".to_owned(), Value::Text("case".into())),
            ],
            "the option the person chose"
        );
        let replaced = named("bar-search-replace");
        assert_eq!(replaced.len(), 1, "{made:?}");
        assert_eq!(
            replaced[0].1,
            vec![
                ("tab".to_owned(), Value::Text("p1".into())),
                ("with".to_owned(), Value::Text("Launch".into())),
                ("all".to_owned(), Value::Bool(true)),
            ],
            "the replacement and whether every match takes it"
        );
    }

    phase widening_hands_the_query_to_the_system_search_scoped;
    {
        let (host, called) = shell();
        let shown = desktop(&host);
        let key = controls(&shown).key.clone();
        let typing = |value: &str| {
            shown
                .fire_with(&key, "changed", Some(&Value::Text(value.into())))
                .expect("what is typed");
            shown
                .fire_with(&key, "widen", Some(&Value::Text("app".into())))
                .expect("the app's search");
        };
        typing("Ship");
        let made = calls(&called);
        let (action, args) = made
            .iter()
            .find(|(action, _)| action == "search")
            .unwrap_or_else(|| panic!("the app's search opens: {made:?}"));
        assert_eq!(action, "search");
        let argument = |name: &str| {
            args.iter()
                .find(|(key, _)| key == name)
                .map(|(_, value)| value.clone())
        };
        assert_eq!(argument("query"), Some(Value::Text("Ship".into())));
        assert_eq!(argument("scope"), Some(Value::Text("app".into())));
        assert_eq!(
            argument("app"),
            Some(Value::Text("so.tilo.notes".into())),
            "the search reaches the app the window shows"
        );

        let (host, called) = shell();
        let shown = desktop(&host);
        let key = controls(&shown).key.clone();
        shown
            .fire_with(&key, "changed", Some(&Value::Text("Ship".into())))
            .expect("what is typed");
        shown
            .fire_with(&key, "widen", Some(&Value::Text("everywhere".into())))
            .expect("everything");
        let made = calls(&called);
        let (_, args) = made
            .iter()
            .find(|(action, _)| action == "search")
            .unwrap_or_else(|| panic!("the search opens: {made:?}"));
        assert_eq!(
            args.iter().find(|(key, _)| key == "scope").map(|(_, value)| value),
            Some(&Value::Text("everywhere".into())),
            "and everywhere is everything"
        );
    }

    phase the_escape_lets_go_and_the_field_closes;
    {
        let (host, called) = shell();
        let shown = desktop(&host);
        let key = controls(&shown).key.clone();
        shown.fire(&key, "close").expect("the escape");
        assert_eq!(
            calls(&called),
            vec![(
                "bar-search-close".to_owned(),
                vec![("tab".to_owned(), Value::Text("p1".into()))],
            )]
        );
    }
}
