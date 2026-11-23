//! Reporting a problem: the shell's picker chooses the window a report is about, and the report
//! opens about that window. Cancel leaves with nothing attached.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, Value};
use tilolang_tilo::host::Host;

/// What the shell asks the session, in order.
struct Asked(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Asked {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

impl Clone for Asked {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

/// The shell's package, over a host that runs it, with two windows of one workspace published
/// and nothing asked of the session yet.
fn shell() -> (Host, Asked, ResourceRef) {
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        || None,
    )
    .unwrap();
    // Only what the picker reads and calls: the windows the device publishes, and the session's
    // package, whose actions the shell's calls name (its `focus` and its `report_problem`
    // shortcut). Both must load, or the test says so.
    let platform = tilolang_tilo::platform_apps::platform_packages().unwrap();
    for app in [
        tilolang_tilo::platform_apps::DEVICE_APP,
        tilolang_tilo::platform_apps::SESSION_APP,
    ] {
        let meta = platform
            .iter()
            .find(|meta| meta.app == app)
            .unwrap_or_else(|| panic!("the platform publishes {app}"))
            .clone();
        host.runtime()
            .load_foreign(meta)
            .unwrap_or_else(|why| panic!("{app}: {why}"));
    }
    let compiled = tilolang_tilo::shell::compile().unwrap_or_else(|why| panic!("{why}"));
    let package = compiled.last().expect("the shell's packages");
    host.load_compiled(package).unwrap();
    host.runtime()
        .set_reading(&windows(), &windows_reading())
        .unwrap();
    let asked = Asked(Rc::default());
    host.runtime().set_foreign_invoker(asked.clone());
    let root = root_of_type(&host, "Shell");
    // The person's layout is the session's to write; a shell whose layout says nothing offers
    // every window it has.
    root_of_type(&host, "Layout");
    (host, asked, root)
}

/// The shell's own root resource of the type `name` (`Shell`, `Layout`), made the first time it
/// is wanted as the host makes it.
fn root_of_type(host: &Host, name: &str) -> ResourceRef {
    let app = "so.tilo.shell";
    let ty = host
        .package(app)
        .unwrap_or_else(|| panic!("the shell's package"))
        .meta
        .types
        .iter()
        .find(|ty| ty.name == name)
        .unwrap_or_else(|| panic!("the shell declares {name}"))
        .id
        .clone();
    host.root_resource(app, &ty).unwrap()
}

/// The windows the session publishes: the ones a person meets on the desktop in front.
fn windows() -> ResourceRef {
    ResourceRef::parse_id("session:windows", "so.tilo.device#Windows").expect("a windows root")
}

fn windows_reading() -> String {
    json!({
        "all": [
            {
                "key": "1", "title": "Groceries", "origin_app": "Notes", "app_key": "so.tilo.notes",
                "strip": "S1", "strip_key": "s1", "workspace": "Work", "workspace_key": "w1",
                "group_key": "1", "tab_index": 0, "share": 1.0, "local": true, "place": 0,
                "recency": 2, "focused": false, "preview": "live", "not_responding": false,
            },
            {
                "key": "2", "title": "Inbox", "origin_app": "Mail", "app_key": "so.tilo.mail",
                "strip": "S1", "strip_key": "s1", "workspace": "Work", "workspace_key": "w1",
                "group_key": "2", "tab_index": 0, "share": 1.0, "local": true, "place": 1,
                "recency": 1, "focused": true, "preview": "live", "not_responding": false,
            },
        ]
    })
    .to_string()
}

/// The id the shell's package gives the action `name`.
fn action(host: &Host, name: &str) -> String {
    host.package("so.tilo.shell")
        .unwrap()
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the shell declares {name}"))
        .id
        .clone()
}

/// Run the shell's action `name` on its document `root`, as a press or an agent's call does: the
/// document first, then the arguments it declares.
fn invoke(host: &Host, root: &ResourceRef, name: &str, args: Vec<Value>) -> Value {
    let mut args = vec![Value::resource(root)];
    args.extend(args);
    match host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (action(host, name).as_str(), args),
        )
        .unwrap_or_else(|why| panic!("{name}: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

/// What the shell asked the session, by the app and action of each invocation.
fn invoked(asked: &Asked) -> Vec<(String, String)> {
    asked
        .0
        .borrow()
        .iter()
        .map(|invocation| match invocation {
            Invocation::Action { app, action, .. } => {
                (app.as_str().to_owned(), action.as_str().to_owned())
            }
            other => panic!("an action invocation: {other:?}"),
        })
        .collect()
}

/// The window the shell asked the session to bring forward and open a report about, by its tab.
fn window_reporting(asked: &Asked) -> Option<String> {
    let invocations = asked.0.borrow();
    invocations.iter().find_map(|invocation| match invocation {
        Invocation::Action {
            action,
            args,
            target,
            ..
        } if action.as_str() == "focus" => args
            .iter()
            .find_map(|(_, value)| match value {
                Value::Ref(window) => Some(window.target.id_string()),
                _ => None,
            })
            .or_else(|| target.as_ref().map(ResourceRef::id_string)),
        _ => None,
    })
}

/// How many windows the shell's report is about.
fn picked(host: &Host, root: &ResourceRef) -> usize {
    let value = host.runtime().read(root, "report_picks").unwrap();
    host.runtime()
        .project(&value)
        .as_array()
        .map_or(0, Vec::len)
}

tilo_test_env::scenario! {
    fn a_report_is_about_the_window_the_person_picks;

    phase the_picker_opens_over_the_windows_and_nothing_is_picked_yet;
    {
        let (host, asked, root) = shell();
        invoke(&host, &root, "report_problem", vec![]);
        assert_eq!(
            host.runtime().read(&root, "reporting").unwrap(),
            Value::Bool(true),
            "the picker is up"
        );
        assert_eq!(picked(&host, &root), 0, "nothing is picked yet");
        assert!(invoked(&asked).is_empty(), "the picker asks the session for nothing yet");
    }

    phase a_pick_names_the_window_and_opens_the_report_about_it;
    {
        let (host, asked, root) = shell();
        invoke(&host, &root, "report_problem", vec![]);
        invoke(&host, &root, "pick_report_window", vec![Value::text("2")]);
        assert_eq!(
            host.runtime().read(&root, "reporting").unwrap(),
            Value::Bool(false),
            "a pick answers the picker"
        );
        assert_eq!(picked(&host, &root), 1, "the report is about the window the person picked");
        assert_eq!(
            window_reporting(&asked).as_deref(),
            Some("desktop:tab:2"),
            "the picked window comes forward, and its report box opens about it"
        );
        assert_eq!(
            invoked(&asked)
                .iter()
                .map(|(_, action)| action.as_str())
                .collect::<Vec<_>>(),
            ["focus", "shortcut"],
            "the window first, then the report on it"
        );
    }

    phase a_rectangle_picks_the_window_it_covers;
    {
        let (host, asked, root) = shell();
        invoke(&host, &root, "report_problem", vec![]);
        // Across the second card of the picker's three-across grid.
        invoke(
            &host,
            &root,
            "pick_report_region",
            vec![Value::List(vec![
                Value::Int(520),
                Value::Int(0),
                Value::Int(520),
                Value::Int(360),
            ])],
        );
        assert_eq!(picked(&host, &root), 1, "the drag picks the window under it");
        assert_eq!(
            window_reporting(&asked).as_deref(),
            Some("desktop:tab:2"),
            "the window under the rectangle"
        );
    }

    phase a_rectangle_over_no_window_keeps_the_picker_up_with_its_outline;
    {
        let (host, asked, root) = shell();
        invoke(&host, &root, "report_problem", vec![]);
        invoke(
            &host,
            &root,
            "pick_report_region",
            vec![Value::List(vec![
                Value::Int(5000),
                Value::Int(5000),
                Value::Int(100),
                Value::Int(100),
            ])],
        );
        assert_eq!(
            host.runtime().read(&root, "reporting").unwrap(),
            Value::Bool(true),
            "the picker stays up"
        );
        assert_eq!(picked(&host, &root), 0);
        assert!(invoked(&asked).is_empty(), "no report opens over nothing");
        assert_eq!(
            host.runtime().read(&root, "report_area").unwrap(),
            Value::List(vec![
                Value::Int(5000),
                Value::Int(5000),
                Value::Int(100),
                Value::Int(100)
            ]),
            "the outline says what was covered"
        );
    }

    phase cancel_leaves_with_nothing_attached;
    {
        let (host, asked, root) = shell();
        invoke(&host, &root, "report_problem", vec![]);
        invoke(&host, &root, "cancel_report", vec![]);
        assert_eq!(
            host.runtime().read(&root, "reporting").unwrap(),
            Value::Bool(false)
        );
        assert_eq!(picked(&host, &root), 0, "nothing is attached");
        assert!(invoked(&asked).is_empty(), "no report opens");
    }
}
