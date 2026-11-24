//! Settings' Shell page as a person uses it: the widgets the shell keeps and the views that draw
//! its parts. Each is opened and pressed here, and what it sends is what the settings publish,
//! never a copy of them.

#![allow(
    clippy::expect_used,
    clippy::unwrap_used,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::path::Path;
use std::rc::Rc;

use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{Actor, ResourceRef, UiNode, Value, ViewInstance};
use tilolang_tilo::host::Host;

const APP: &str = "so.tilo.settings";

/// The app as it ships, with the platform's packages it reads through loaded, as a window's
/// process loads them.
fn settings() -> (Host, tilolang::Compiled) {
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
        || None,
    )
    .expect("a host");
    host.load_compiled(&compiled).expect("the package loads");
    tilolang_tilo::platform_apps::load(&host, &compiled.meta.uses);
    (host, compiled)
}

/// The resource `doc` of `ty`, as a view of a platform package reads it.
fn resource(doc: &str, ty: &str) -> ResourceRef {
    ResourceRef::parse_id(doc, ty).expect("a resource id")
}

/// Hand `host` the reading `json` of `doc`, as the session publishes it there.
fn reading(host: &Host, doc: &str, ty: &str, json: serde_json::Value) {
    host.runtime()
        .set_reading(&resource(doc, ty), &json.to_string())
        .unwrap_or_else(|why| panic!("{doc} is read: {why}"));
}

/// The app's view `name`, opened as a window opens it.
fn open(host: &Host, name: &str) -> ViewInstance {
    let id = host
        .package(APP)
        .expect("the app is loaded")
        .meta
        .views
        .iter()
        .find(|view| view.name == name)
        .unwrap_or_else(|| panic!("the app declares the view `{name}`"))
        .id
        .clone();
    let mut view = host
        .runtime()
        .open_view(
            &Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            &id,
            Vec::new(),
        )
        .unwrap_or_else(|why| panic!("{name} opens: {why}"));
    view.poll_patches().expect("the view draws");
    view
}

/// The words a view shows, its text as written.
fn texts(view: &ViewInstance) -> Vec<String> {
    crate::fixtures::nodes(view.tree())
        .into_iter()
        .flat_map(|node| node.props.values())
        .filter_map(|value| match value {
            Value::Text(text) => Some(text.clone()),
            Value::Rich(rich) => Some(rich.written_text()),
            _ => None,
        })
        .collect()
}

/// The node of `kind` whose `prop` reads `word`.
fn named<'a>(tree: &'a UiNode, kind: &str, prop: &str, word: &str) -> &'a UiNode {
    tree.of_kind(kind)
        .into_iter()
        .find(|node| node.props.get(prop) == Some(&Value::text(word)))
        .unwrap_or_else(|| panic!("a {kind} whose {prop} reads {word}"))
}

/// The control of `kind` labelled `label`, by the key the window gives it.
fn labelled(tree: &UiNode, kind: &str, label: &str) -> String {
    named(tree, kind, "label", label).key.clone()
}

/// One invocation as it was sent, with its arguments by name.
#[derive(Clone)]
struct Sent(Rc<RefCell<Vec<Invocation>>>);

impl Sent {
    fn new() -> Self {
        Self(Rc::default())
    }

    fn last(&self) -> Invocation {
        self.0
            .borrow()
            .last()
            .cloned()
            .unwrap_or_else(|| panic!("the pane sends a verb"))
    }

    /// The app and action of the last invocation.
    fn called(&self) -> (String, String) {
        let Invocation::Action { app, name, .. } = self.last() else {
            panic!("the pane sends an action: {:?}", self.last());
        };
        (app.to_string(), name)
    }

    fn argument(&self, named: &str) -> Value {
        let Invocation::Action { args, .. } = self.last() else {
            panic!("the pane sends an action: {:?}", self.last());
        };
        args.iter()
            .find(|(name, _)| name == named)
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| panic!("the verb takes `{named}`: {args:?}"))
    }
}

impl Invoker for Sent {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        Invoked::Done(Value::Unit)
    }
}

/// The widgets' choices as the person's settings keep them, each on the shell and shown after a

fn widgets(enabled: &[(&str, bool)]) -> serde_json::Value {
    let widget = |on: bool| {
        serde_json::json!({
            "enabled": on,
            "show": "pause",
            "always_on_strips": [],
            "places": [],
            "width": null,
        })
    };
    let on = |name: &str| {
        enabled
            .iter()
            .find(|(widget, _)| *widget == name)
            .map_or(false, |(_, on)| *on)
    };
    serde_json::json!({
        "music": widget(on("music")),
        "team": widget(on("team")),
        "calendar": widget(on("calendar")),
        "system_monitor": widget(on("system_monitor")),
    })
}

/// The Appearance pane reads the person's appearance, the desktop controls kept for them and on
/// this device, and the widgets; each reads as the shell shows it before a change.
fn appearance_readings(host: &Host) {
    reading(
        host,
        "settings/appearance",
        "so.tilo.settings.sections#Appearance",
        serde_json::json!({}),
    );
    reading(
        host,
        "settings/desktop-controls",
        "so.tilo.settings.sections#DesktopControls",
        serde_json::json!({}),
    );
    reading(
        host,
        "devices/{device}/settings/desktop-controls",
        "so.tilo.settings.sections#DesktopControlsDevice",
        serde_json::json!({}),
    );
    reading(
        host,
        "devices/{device}/settings/widgets",
        "so.tilo.settings.sections#Widgets",
        widgets(&[("music", true), ("system_monitor", true)]),
    );
}

/// The widgets card keeps a widget on the shell or takes it off through the settings' own action,
/// and says when each shows.
#[test]
fn the_widgets_section_keeps_and_shapes_the_shell_widgets() {
    let (host, _) = settings();
    appearance_readings(&host);
    let mut view = open(&host, "appearance_pane");

    let shown = texts(&view);
    for word in ["Widgets", "Music", "People here", "Calendar", "System load"] {
        assert!(shown.iter().any(|text| text == word), "{word}: {shown:?}");
    }
    // The switch shows what the settings keep: Music is on, People here is not.
    let music_toggle = named(view.tree(), "ui.Toggle", "label", "Keep Music on the shell");
    assert_eq!(music_toggle.props.get("shown"), Some(&Value::Bool(true)));
    let people_toggle = named(
        view.tree(),
        "ui.Toggle",
        "label",
        "Keep People here on the shell",
    );
    assert_eq!(people_toggle.props.get("shown"), Some(&Value::Bool(false)));
    let keep_music = music_toggle.key.clone();

    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    view.fire_with(&keep_music, "check", Some(&Value::Bool(false)))
        .expect("the person takes Music off the shell");
    let Invocation::Action { app, name, .. } = sent.last() else {
        panic!("a verb of the settings: {:?}", sent.last());
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.settings.sections", "set-widget-enabled"),
        "the settings keep the choice, as the shell reads it"
    );
    assert_eq!(sent.argument("widget"), Value::text("music"));
    assert_eq!(sent.argument("enabled"), Value::Bool(false));

    // When it shows is the settings' choice too: always, after a pause, or only when asked.
    let always = labelled(view.tree(), "ui.Segment", "Always");
    view.press(&always)
        .expect("the person has Music show always");
    assert_eq!(sent.argument("widget"), Value::text("music"));
    assert_eq!(sent.argument("show"), Value::text("always"));
}

/// The Shell pane lists each part with the view that draws it, and one part's page binds it to a
/// view of the person's app, or shows the shell's own again.
#[test]
fn the_shell_pane_binds_and_resets_the_view_of_a_part() {
    let (host, _) = settings();
    reading(
        &host,
        "settings/shell-views",
        "so.tilo.settings.sections#ShellViews",
        serde_json::json!({
            "bindings": {
                "bar": { "app": "so.tilo.example.bar", "view": "bar" }
            },
            "on_device": {}
        }),
    );
    let mut view = open(&host, "shell_pane");

    let shown = texts(&view);
    for word in [
        "Parts of the shell",
        "The bar",
        "The window menu",
        "so.tilo.example.bar - bar",
        "The view the shell ships",
    ] {
        assert!(shown.iter().any(|text| text == word), "{word}: {shown:?}");
    }

    // The bar's own page: what draws it, and where the person names the app and view instead.
    let bar = named(view.tree(), "ui.ListRow", "title", "The bar");
    let change = labelled(bar, "ui.Button", "Change");
    view.press(&change)
        .expect("the person opens the bar's page");
    view.poll_patches().expect("the page draws");
    let page = texts(&view);
    for word in [
        "Drawn by",
        "so.tilo.example.bar - bar",
        "The app",
        "The view",
    ] {
        assert!(page.iter().any(|text| text == word), "{word}: {page:?}");
    }

    let inputs: Vec<String> = view
        .tree()
        .of_kind("ui.TextInput")
        .into_iter()
        .map(|input| input.key.clone())
        .collect();
    assert_eq!(inputs.len(), 2, "the app and its view: {inputs:?}");
    view.edit(&inputs[0], &Value::text("so.tilo.example.menu"))
        .expect("the app is typed");
    view.edit(&inputs[1], &Value::text("menu"))
        .expect("the view is typed");
    view.poll_patches().expect("the fields draw");

    let sent = Sent::new();
    host.runtime().set_invoker(sent.clone());
    let bind = labelled(view.tree(), "ui.Button", "Bind");
    view.press(&bind).expect("the person binds the part");
    let Invocation::Action { app, name, .. } = sent.last() else {
        panic!("a verb of the settings: {:?}", sent.last());
    };
    assert_eq!(
        (app.as_str(), name.as_str()),
        ("so.tilo.settings.sections", "bind")
    );
    assert_eq!(sent.argument("part"), Value::text("bar"));
    assert_eq!(sent.argument("app"), Value::text("so.tilo.example.menu"));
    assert_eq!(sent.argument("view"), Value::text("menu"));
    assert!(
        matches!(sent.argument("on"), Value::Opt(None)),
        "every device, as the page's choice starts: {:?}",
        sent.argument("on")
    );

    // The shell's own view comes back by the settings' reset.
    let reset = labelled(view.tree(), "ui.Button", "Show the shell's own view again");
    view.press(&reset)
        .expect("the person shows the shell's own again");
    assert_eq!(
        sent.called(),
        ("so.tilo.settings.sections".to_owned(), "reset".to_owned())
    );
    assert_eq!(sent.argument("part"), Value::text("bar"));
    assert!(matches!(sent.argument("on"), Value::Opt(None)));
}
