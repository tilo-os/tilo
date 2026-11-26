//! Weather's window over its places: the commands its bar offers once, and what a reading's
//! temperature and moment say.

use tilolang_runtime::{Actor, InvokeOutcome, UiNode, Value};

use crate::fixtures::{Replica, WEATHER, Workspace};

const APP: &str = "so.tilo.weather";

/// Ada's replica on one device, with a place she follows: what a person shares, so the window
/// has a place to show and its bar offers what stands on that place.
fn week() -> Replica {
    let (ada, _) = Replica::on_device("ada", "ada-1", Workspace::new(), &[WEATHER]);
    // The window reads the person's units from Settings, which this host publishes as a session
    // does: the section's defaults, so the view runs rather than waiting on a reading.
    for meta in tilolang_tilo::platform::named_platform_packages(WEATHER.0).unwrap() {
        ada.host.runtime().load_foreign(meta).unwrap();
    }
    let locale = tilolang_runtime::ResourceRef::parse_id(
        tilolang_tilo::platform::SETTINGS_LOCALE,
        "so.tilo.settings.sections#Locale",
    )
    .unwrap();
    ada.host
        .runtime()
        .set_reading(&locale, tilolang_tilo::platform::SETTINGS_LOCALE_DEFAULTS)
        .unwrap();
    // Opening the app on this device makes the roots it keeps of its own, as a window does.
    let root = ada.host.root_of(APP).unwrap();
    ada.host
        .root_resource(
            APP,
            &crate::fixtures::type_id(&ada.host.package(APP).unwrap().meta, "Readings"),
        )
        .unwrap();
    call(
        &ada,
        "save_place",
        vec![
            Value::resource(&root),
            Value::text("Amsterdam"),
            Value::Float(52.374),
            Value::Float(4.8897),
        ],
    );
    ada.commit();
    ada
}

/// The answer of the app's action `action`, run as this replica's person.
fn call(ada: &Replica, action: &str, args: Vec<Value>) -> Value {
    let meta = &ada.host.package(APP).unwrap().meta;
    let id = crate::fixtures::action_id(meta, action);
    match ada
        .host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (id.as_str(), args),
        )
        .expect("the action runs")
    {
        InvokeOutcome::Done(value) => value,
        other => panic!("{action}: {other:?}"),
    }
}

/// The window's `actions` part: what its bar offers.
fn actions(view: &tilolang_runtime::ViewInstance) -> UiNode {
    view.tree()
        .of_kind("part")
        .into_iter()
        .find(|part| part.props.get("name") == Some(&Value::text("actions")))
        .cloned()
        .unwrap_or_else(|| panic!("the window's actions: {:#?}", view.tree()))
}

fn label(node: &UiNode) -> Option<String> {
    match node.props.get("label") {
        Some(Value::Text(text)) => Some(text.clone()),
        _ => None,
    }
}

tilo_test_env::scenario! {
    fn weathers_window_offers_each_command_once;

    phase each_place_command_stands_in_the_bar_once;
    // The bar keeps the app's controls once each: one way to add a place, one to read its
    // forecast again, one to keep it first, and one to take it away. A declared action the view
    // never placed stands behind the bar's own "…", never a second control of the row.
    {
        let ada = week();
        let mut view = ada.view(APP);
        view.poll_patches().unwrap();
        let part = actions(&view);
        let shown: Vec<String> = part
            .of_kind("ui.Button")
            .into_iter()
            // A declared action the view never placed stands behind the bar's own "…".
            .filter(|button| button.props.get("in_menu") != Some(&Value::Bool(true)))
            .filter_map(label)
            .collect();
        for once in ["Add place", "Refresh", "Show first", "Delete place"] {
            assert_eq!(
                shown.iter().filter(|word| *word == once).count(),
                1,
                "\"{once}\" stands in the bar once: {shown:?}"
            );
        }
        for gone in ["Move to top", "Save a place", "Remove place"] {
            assert!(
                !shown.iter().any(|word| word == gone),
                "\"{gone}\" is not a second control of the row: {shown:?}"
            );
        }
    }

}
