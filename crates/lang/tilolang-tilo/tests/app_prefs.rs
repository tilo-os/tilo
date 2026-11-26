//! An app's own preferences: declared in its package, read from the settings service
//! (`settings/apps/<app>`) in its views, and changed through the fields' setters, exactly as
//! Settings changes a section. An app reaches no other app's preferences.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::indexing_slicing,
    clippy::panic,
    reason = "test assertions"
)]

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex, OnceLock};

use serde_json::json;
use tilolang::IdsLock;
use tilolang_runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_runtime::{EditOutcome, ResourceRef, Value, ViewInstance};
use tilolang_tilo::host::Host;
use tilolang_tilo::platform::{self, PublishedSource, Reading, Roots};

const SOURCE: &str = r#"app APP "Preferences"

/// What this app keeps for the person.
prefs Preferences {
    /// How large the text is.
    font_size: Int = 13
    bell: Bool = false
}

view window(p: Preferences) {
  stack {
    text("{p.font_size}") as size
    text("bell {p.bell}") as bell_text
    toggle(bind: p.bell) as bell
  }
}
"#;

/// The settings service, as this test reaches it: what each resource publishes now, and every
/// setter the app invoked.
#[derive(Clone, Default)]
struct Service {
    inner: Arc<Inner>,
}

/// One setter the app invoked: the resource, the verb and its arguments.
type Sent = (String, String, serde_json::Map<String, serde_json::Value>);

#[derive(Default)]
struct Inner {
    /// Each resource's newest reading, waiting to be handed to a runtime.
    readings: Mutex<BTreeMap<String, Arc<Mutex<Option<String>>>>>,
    /// Each app's stored preferences, as its setters wrote them.
    stored: Mutex<BTreeMap<String, serde_json::Map<String, serde_json::Value>>>,
    /// Every setter the app invoked: its resource, verb and arguments.
    sent: Mutex<Vec<Sent>>,
}

impl Service {
    /// The slot `resource` publishes through.
    fn slot(&self, resource: &str) -> Arc<Mutex<Option<String>>> {
        Arc::clone(
            self.inner
                .readings
                .lock()
                .unwrap()
                .entry(resource.to_owned())
                .or_default(),
        )
    }

    /// Publish `reading` of `resource`, as the session does when the settings moved.
    fn publish(&self, resource: &str, reading: serde_json::Value) {
        *self.slot(resource).lock().unwrap() = Some(reading.to_string());
    }

    /// Every setter the app invoked at `resource`.
    fn sent(&self, resource: &str) -> Vec<(String, serde_json::Map<String, serde_json::Value>)> {
        self.inner
            .sent
            .lock()
            .unwrap()
            .iter()
            .filter(|(at, _, _)| at == resource)
            .map(|(_, verb, arguments)| (verb.clone(), arguments.clone()))
            .collect()
    }
}

struct Watcher {
    slot: Arc<Mutex<Option<String>>>,
}

impl Reading for Watcher {
    fn latest(&self) -> Option<Result<String, String>> {
        self.slot.lock().unwrap().take().map(Ok)
    }
}

impl PublishedSource for Service {
    fn follow(
        &self,
        resource: &str,
        _wake: tilo_rt::wake::WakeHandle,
    ) -> Result<Box<dyn Reading>, String> {
        Ok(Box::new(Watcher {
            slot: self.slot(resource),
        }))
    }

    fn invoke(
        &self,
        resource: &str,
        action: &str,
        arguments: &serde_json::Map<String, serde_json::Value>,
    ) -> Result<(), String> {
        // Each preference has its setter (`set-<field>`), which writes the whole new value into
        // the app's preferences; nothing else changes one.
        if !resource.starts_with("settings/apps/") {
            return Err(format!("`{resource}` is no app's preferences"));
        }
        let field = action
            .strip_prefix("set-")
            .ok_or_else(|| format!("`{action}` is no preference's setter"))?
            .replace('-', "_");
        let value = arguments
            .get("value")
            .cloned()
            .ok_or_else(|| format!("`{action}` takes the preference's value"))?;
        let initial = self
            .slot(resource)
            .lock()
            .unwrap()
            .clone()
            .and_then(|reading| serde_json::from_str::<serde_json::Value>(&reading).ok())
            .and_then(|reading| reading.as_object().cloned())
            .unwrap_or_default();
        let reading = {
            let mut stored = self.inner.stored.lock().unwrap();
            let preferences = stored.entry(resource.to_owned()).or_insert(initial);
            preferences.insert(field, value);
            let reading = serde_json::Value::Object(preferences.clone());
            drop(stored);
            reading
        };
        self.publish(resource, reading);
        self.inner.sent.lock().unwrap().push((
            resource.to_owned(),
            action.to_owned(),
            arguments.clone(),
        ));
        Ok(())
    }
}

/// The settings service every phase reads through: one per test binary, since a process installs
/// its source once. Each phase's app has an id of its own, so no phase reads another's.
fn service() -> Service {
    static SERVICE: OnceLock<Service> = OnceLock::new();
    SERVICE
        .get_or_init(|| {
            let service = Service::default();
            platform::install(Box::new(service.clone()));
            service
        })
        .clone()
}

/// The app's own preferences as a view of it names them.
fn preferences(app: &str) -> ResourceRef {
    let ty = format!("{app}#Preferences");
    ResourceRef::parse_id(&platform::prefs_address(app), ty.as_str()).expect("a resource id")
}

/// A bound control's edit, sent to the settings service as the app process sends it: the
/// field's setter, with the value the control handed over.
struct Setters {
    roots: Rc<RefCell<Roots>>,
    app: String,
}

impl Invoker for Setters {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        let Invocation::Edit {
            target,
            field,
            value,
            ..
        } = invocation
        else {
            return Invoked::Failed("the app writes no field here".to_owned());
        };
        if let Some(refusal) = platform::refuse_foreign_prefs(&target.id_string(), &self.app) {
            return Invoked::Refused(refusal);
        }
        let (verb, arguments) = platform::prefs_setter(field, value);
        match self
            .roots
            .borrow()
            .invoke(&target.id_string(), &verb, &arguments)
        {
            Ok(()) => Invoked::Done(Value::Unit),
            Err(refusal) => Invoked::Refused(refusal),
        }
    }
}

struct App {
    host: Host,
    view: String,
    roots: Rc<RefCell<Roots>>,
    id: String,
}

impl App {
    fn new(app: &str) -> Self {
        let host = Host::new(
            tilolang_runtime::Actor::from(tilolang_tilo::runtime::ids::MemberId::person("ada")),
            || None,
        )
        .expect("a host");
        let compiled = compiled_for(app);
        let package = host.load_compiled(&compiled).expect("the app loads");
        let view = package
            .meta
            .views
            .iter()
            .find(|view| view.name == "window")
            .expect("the window view")
            .id
            .clone();
        assert_eq!(compiled.meta.prefs, vec![format!("{app}#Preferences")]);
        let mut roots = Roots::default();
        let wake = tilo_rt::wake::WakeHandle::default();
        roots.load(&host, &compiled.meta.uses, &wake);
        roots.load_prefs(&host, &compiled.meta, &wake);
        Self {
            host,
            view,
            roots: Rc::new(RefCell::new(roots)),
            id: app.to_owned(),
        }
    }

    /// Hand the runtime every reading the settings service published since the last call.
    fn settle(&self) -> bool {
        self.roots.borrow_mut().read(&self.host, |_| true)
    }

    fn window(&self) -> ViewInstance {
        self.host
            .open_resource(&preferences(&self.id), Some(&self.view))
            .expect("the window opens")
    }

    fn anchor(&self, name: &str) -> String {
        self.host
            .package(&self.id)
            .expect("the app is loaded")
            .meta
            .views
            .iter()
            .filter(|view| view.name == "window")
            .flat_map(|view| &view.anchors)
            .find(|anchor| anchor.name == name)
            .expect("the anchor")
            .id
            .clone()
    }
}

/// Every text the window shows.
fn texts(view: &ViewInstance) -> Vec<String> {
    fn walk(node: &tilolang_runtime::UiNode, out: &mut Vec<String>) {
        if node.kind == "ui.Text"
            && let Some(Value::Text(text)) = node.props.get("value")
        {
            out.push(text.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(view.tree(), &mut out);
    out
}

tilo_test_env::scenario! {
    fn an_apps_preferences_are_declared_read_from_the_settings_and_set_through_their_setters;

    phase a_preference_reads_as_the_app_declared_it_before_the_settings_answer;
    {
        service();
        let app = App::new("test.prefs.declared");
        app.settle();
        assert_eq!(
            texts(&app.window()),
            vec!["13".to_owned(), "bell false".to_owned()]
        );
    }

    phase a_preference_reads_the_reading_the_settings_publish;
    {
        let service = service();
        let app_id = "test.prefs.read";
        let address = platform::prefs_address(app_id);
        service.publish(&address, json!({ "font_size": 20 }));
        let app = App::new(app_id);
        let moved = app.settle();
        assert!(moved, "the reading moved the runtime");
        assert_eq!(
            texts(&app.window()).first().map(String::as_str),
            Some("20")
        );
    }

    phase a_change_through_the_settings_setter_is_seen_live;
    {
        let service = service();
        let app_id = "test.prefs.set";
        let address = platform::prefs_address(app_id);
        let app = App::new(app_id);
        app.settle();
        assert_eq!(
            texts(&app.window()),
            vec!["13".to_owned(), "bell false".to_owned()]
        );
        app.host.runtime().set_invoker(Setters {
            roots: Rc::clone(&app.roots),
            app: app_id.to_owned(),
        });
        // The person turns the switch: the control's edit is `settings/apps/<app>#set-bell`.
        assert!(
            matches!(
                app.window().edit(&app.anchor("bell"), &Value::Bool(true)),
                Ok(EditOutcome::Applied)
            ),
            "the switch's turn is the preference's setter"
        );
        assert_eq!(
            service.sent(&address),
            vec![(
                "set-bell".to_owned(),
                serde_json::Map::from_iter([("value".to_owned(), json!(true))])
            )],
            "the field's own setter, with the whole new value"
        );
        // The settings service published the app's preferences with the change: every view that
        // read them runs again and shows it.
        app.settle();
        assert!(
            texts(&app.window()).contains(&"bell true".to_owned()),
            "the view shows the reading"
        );
    }

    phase another_apps_preferences_are_refused;
    {
        let app_id = "test.prefs.mine";
        let app = App::new(app_id);
        app.settle();
        // The same typed read, pointed at another app's address: no app reads another's.
        let theirs = ResourceRef::parse_id(
            "settings/apps/test.prefs.other",
            format!("{app_id}#Preferences"),
        )
        .expect("a resource id");
        let _ = app.host.runtime().read(&theirs, "font_size");
        app.settle();
        let reading = app
            .host
            .runtime()
            .reading(&theirs)
            .map(|(json, _)| json.to_string())
            .unwrap_or_default();
        assert!(
            reading.contains("preferences of test.prefs.other"),
            "another app's preferences are not read: {reading}"
        );
        let refusal = app
            .roots
            .borrow()
            .invoke(
                "settings/apps/test.prefs.other",
                "set-font-size",
                &serde_json::Map::from_iter([("value".to_owned(), json!(20))]),
            )
            .expect_err("another app's preferences are not changed");
        assert!(
            refusal.contains("preferences of test.prefs.other"),
            "{refusal}"
        );
    }
}

/// `SOURCE` compiled for the app `app`, as `tilolang build` builds it.
fn compiled_for(app: &str) -> tilolang::Compiled {
    let source = SOURCE.replace("APP", app);
    tilolang::compile_package(&source, &IdsLock::new(), &[]).unwrap_or_else(|diagnostics| {
        panic!(
            "{}",
            diagnostics
                .iter()
                .map(|d| format!("{}: {}", d.code, d.message))
                .collect::<Vec<_>>()
                .join("\n")
        )
    })
}

tilo_test_env::scenario! {
    fn an_app_whose_settings_a_system_section_keeps_declares_no_preferences;

    phase the_keyboards_settings_live_in_the_system_section_at_its_address;
    {
        let keyboard = compiled_for("so.tilo.osk");
        assert!(
            !keyboard.meta.prefs.is_empty(),
            "the keyboard's id declares preferences"
        );
        let refusal = tilolang_tilo::bundle::Bundle::from_parts(
            keyboard.wasm.clone(),
            keyboard.ids.to_text(),
            &[],
        )
        .expect_err("the keyboard's settings are a system section's, not an app's preferences");
        let said = refusal.to_string();
        assert!(said.contains("settings.osk-sources"), "{said}");
        assert!(said.contains("settings/apps/so.tilo.osk"), "{said}");
    }

    phase an_ordinary_apps_preferences_install;
    {
        let ours = compiled_for("test.prefs.installed");
        assert!(
            tilolang_tilo::bundle::Bundle::from_parts(ours.wasm.clone(), ours.ids.to_text(), &[])
                .is_ok(),
            "an app the system keeps nothing for declares its own preferences"
        );
    }
}
