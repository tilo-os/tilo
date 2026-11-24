//! Settings' app-level actions show the pane a search page or setting names, as search's rows
//! run them from outside the window, on the app's published resource.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilo_action_policy::MemberAccess;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::host::Host;
use tilolang_tilo::runtime::{Actor, Value, ViewInstance};

const APP: &str = "so.tilo.settings";

/// The app as it ships: its source, its ids and the platform's packages it reads through.
fn settings() -> (Host, tilolang::Compiled) {
    settings_with(None)
}

/// [`settings`], the person at the window deciding by `access` (an owner's, say): what a pane
/// offers them and what its verbs reach.
fn settings_with(access: Option<MemberAccess>) -> (Host, tilolang::Compiled) {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../apps/tl/settings");
    let source = tilolang::PackageSource::read_dir(&dir).expect("the package reads");
    let ids = tilolang::IdsLock::parse(
        &std::fs::read_to_string(dir.join("app.ids")).expect("the package's ids"),
    )
    .expect("its ids parse");
    let metas =
        tilolang_tilo::platform_apps::platform_packages().expect("the platform's packages read");
    let compiled =
        tilolang::compile_package_source(&source, &ids, &metas).expect("the package compiles");
    let host = Host::new(
        Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
        move || access.clone(),
    )
    .expect("a host");
    host.load_compiled(&compiled)
        .expect("the package loads");
    (host, compiled)
}

/// The pane the window shows, as the app's own document holds it.
fn pane(host: &Host) -> String {
    let root = host.root_of(APP).expect("the app's root");
    match host.runtime().read(&root, "pane").expect("a pane") {
        Value::Text(text) => text,
        other => panic!("not text: {other:?}"),
    }
}

/// Run the app-level action `name` with `args`, as a search row does from outside the window.
fn invoke(host: &Host, name: &str, args: Vec<Value>) -> Value {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .actions
        .iter()
        .find(|action| action.name == name)
        .unwrap_or_else(|| panic!("the app publishes no `{name}`"))
        .id
        .clone();
    match host
        .runtime()
        .invoke_action(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            (id.as_str(), args),
        )
        .unwrap_or_else(|why| panic!("{name}: {why}"))
    {
        tilolang_runtime::InvokeOutcome::Done(value) => value,
        other => panic!("{name}: {other:?}"),
    }
}

#[test]
fn a_setting_opens_the_pane_it_is_on_and_a_page_its_own() {
    let (host, _) = settings();
    let _ = host.root_of(APP).expect("the app's root");
    assert_eq!(pane(&host), "appearance", "the window starts on Appearance");

    invoke(&host, "show_setting", vec![Value::text("keyboard.layout")]);
    assert_eq!(pane(&host), "input", "a keyboard setting is on Input");

    invoke(&host, "show_setting", vec![Value::text("a11y.high_contrast")]);
    assert_eq!(pane(&host), "accessibility");

    invoke(&host, "show_setting", vec![Value::text("notifications.banners")]);
    assert_eq!(pane(&host), "notifications");

    invoke(&host, "open_pane", vec![Value::text("external_accounts")]);
    assert_eq!(pane(&host), "accounts", "the index's page slug is Accounts");

    invoke(&host, "open_pane", vec![Value::text("keyboard")]);
    assert_eq!(pane(&host), "input");

    invoke(&host, "open_pane", vec![Value::text("displays")]);
    assert_eq!(pane(&host), "displays");
}

/// The lines a view shows, its text as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            _ => None,
        })
        .collect()
}

/// The key of the control of `kind` whose label `label` reads, as the window lays it out.
fn labelled(tree: &tilolang_runtime::UiNode, kind: &str, label: &str) -> String {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get("label") == Some(&Value::text(label)))
        .unwrap_or_else(|| panic!("a {kind} labelled {label}"))
        .key
        .clone()
}

/// One invocation as it was sent, with its arguments by name.
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// What a host keeps of the workspace's content reads in mebibytes, the unit its owners set the
/// limits in, and only an owner is offered to set them.
#[test]
fn the_content_pane_shows_what_the_host_keeps_in_mebibytes() {
    use tilolang_tilo::content::WorkspaceContent;

    let (host, _) = settings();
    host.give_content(&WorkspaceContent {
        file_bytes: 25 * 1_048_576,
        member_bytes: 250 * 1_048_576,
        hourly_bytes: 250 * 1_048_576,
        refusals: Vec::new(),
        host_person: None,
    });
    invoke(&host, "open_pane", vec![Value::text("content")]);
    assert_eq!(pane(&host), "content");
    let mut view = host.open_main(APP).expect("the window opens");
    view.poll_patches().expect("the window draws");
    let shown = texts(&view);
    assert!(shown.contains(&"25 MiB".to_owned()), "{shown:?}");
    assert!(
        shown.iter().filter(|text| *text == "250 MiB").count() == 2,
        "a member's share and their hour: {shown:?}"
    );
    assert!(
        shown
            .iter()
            .any(|text| text == "Only the workspace's owners can change these."),
        "a member reads them and changes none: {shown:?}"
    );
    assert!(
        view.tree().of_kind("ui.Button").is_empty(),
        "a member is offered no limit to change: {:?}",
        texts(&view)
    );
}

/// An owner changes the limits where the workspace keeps them: the pane's Save sends the
/// workspace's own `content.set-policy` with the sizes it shows, in bytes.
#[test]
fn an_owner_sets_the_limits_the_pane_shows() {
    use tilo_action_policy::WorkspacePolicy;
    use tilolang_tilo::content::WorkspaceContent;

    let owner = MemberAccess::person(WorkspacePolicy::new("team", "ada"), "ada");
    let (host, _) = settings_with(Some(owner));
    host.give_content(&WorkspaceContent {
        file_bytes: 25 * 1_048_576,
        member_bytes: 250 * 1_048_576,
        hourly_bytes: 250 * 1_048_576,
        refusals: Vec::new(),
        host_person: None,
    });
    tilolang_tilo::platform_apps::load(&host, &[tilolang_tilo::platform_apps::WORKSPACE_APP.to_owned()]);
    invoke(&host, "open_pane", vec![Value::text("content")]);
    let mut view = host.open_main(APP).expect("the window opens");
    view.poll_patches().expect("the window draws");

    let sent = Rc::new(RefCell::new(Vec::new()));
    host.runtime().set_invoker(Sent(Rc::clone(&sent)));

    view.press(&labelled(view.tree(), "ui.Button", "Change"))
        .expect("the owner opens the limits");
    view.poll_patches().expect("the fields draw");
    assert!(
        texts(&view).iter().any(|text| text == "New limits"),
        "{:?}",
        texts(&view)
    );
    view.press(&labelled(view.tree(), "ui.Button", "Save limits"))
        .expect("the owner saves them");
    let sent = sent.borrow();
    let [
        Invocation::Action {
            app, name, args, ..
        },
    ] = sent.as_slice()
    else {
        panic!("one verb of the workspace: {sent:?}");
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("tilo.workspace", "set-policy"),
        "the workspace's own action, as its surface publishes it"
    );
    let arg = |wanted: &str| {
        args.iter()
            .find(|(named, _)| named == wanted)
            .map(|(_, value)| value.clone())
    };
    assert_eq!(arg("file_bytes"), Some(Value::Int(25 * 1_048_576)));
    assert_eq!(arg("member_bytes"), Some(Value::Int(250 * 1_048_576)));
    assert_eq!(arg("hourly_bytes"), Some(Value::Int(250 * 1_048_576)));
}
