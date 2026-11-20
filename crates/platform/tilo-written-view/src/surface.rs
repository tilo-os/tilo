//! The widget that draws one written view. See the crate's docs.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use serde_json::{Map, Value as Json, json};
use tilo_actions::{Actor, MemberId};
use tilo_app_window::AppWindow;
use tilo_render_protocol::patch::local;
use tilolang_tilo::host::RootView;
use tilolang_tilo::instance::wire::resource;
use tilolang_tilo::instance::{Config, Instance, Mode};
use tilolang_tilo::runtime::delegate::{Invocation, Invoked, Invoker};
use tilolang_tilo::runtime::{
    ActionInfo, Authority as _, Decision, MemorySink, ModuleHash, ResourceRef, Value,
};
use ui::widget::{AppModel, DrawChild as _, Element, ModelPath, Widget};
use ui::written_view::{Shown, WrittenView};

use crate::apps::{self, App};
use crate::platform::ViewPlatform;
use crate::state::{Asked, State};
use crate::workspace::{Read, Workspace};
use crate::wrapper::{self, SHOWN};

/// How often the window and its instance answer each other in one frame before the frame is
/// drawn: a press, what it asked, a reading that came, the views it moved.
const EXCHANGES: usize = 8;

/// `view`, shown over `workspace`: its widget and what it waits on. A view whose package does
/// not open (a fault of this build, never of the view's own text, which the view itself says)
/// is the toolkit's empty state saying so.
pub(crate) fn shown(view: &WrittenView, workspace: Box<dyn Workspace>) -> Shown {
    let state = Rc::new(State::default());
    let widget: Element = match Surface::open(view, workspace, Rc::clone(&state)) {
        Ok(surface) => Box::new(surface),
        Err(why) => {
            tracing::error!(why, "a written view did not open");
            Box::new(ui::widget::EmptyState::new(tilo_app_window::i18n::t(
                "tilolang-open-failed",
            )))
        }
    };
    Shown { widget, state }
}

/// What the view asked of an app: kept for the surface's next turn, which decides it.
struct Asks(Rc<RefCell<Vec<Invocation>>>);

impl Invoker for Asks {
    fn invoke(&self, invocation: &Invocation) -> Invoked {
        self.0.borrow_mut().push(invocation.clone());
        // Decided and sent, or kept as a suggestion, on the surface's next turn: what it changes
        // reaches the view as the app's next reading.
        Invoked::Done(Value::Unit)
    }
}

pub(crate) struct Surface {
    view: AppWindow,
    instance: Instance,
    workspace: Box<dyn Workspace>,
    /// The apps the view names.
    apps: Vec<Rc<App>>,
    /// The documents of their own the view names (`@word_processor.Document:uuid:…`), by their
    /// app's own id and their own, each with the type the view reads it as.
    named: BTreeMap<(String, String), String>,
    /// The resources the view met and held no reading of, as the runtime asked for them.
    wanted: Rc<RefCell<Vec<ResourceRef>>>,
    /// Who wrote the view.
    author: Actor,
    asked: Rc<RefCell<Vec<Invocation>>>,
    state: Rc<State>,
}

impl Surface {
    /// Run `view` over `workspace`.
    ///
    /// # Errors
    /// The package views are shown through did not load or open.
    pub(crate) fn open(
        view: &WrittenView,
        mut workspace: Box<dyn Workspace>,
        state: Rc<State>,
    ) -> Result<Self, String> {
        let wrapper = wrapper::get()?;
        let apps = apps::named(&view.source, &view.reads);
        tilo_app_window::i18n::ensure();
        let wake = ui::wake::WakeHandle::new();
        state.woken_by(wake.clone());
        let failed = Rc::new(RefCell::new(None));
        let (app_end, window_end) = local::pair();
        let mut instance = Instance::new(
            Config {
                app: wrapper.compiled.meta.app.clone(),
                me: view.viewer.clone(),
                mode: Mode::Live,
                device: None,
                // What the view's package keeps of its own: nothing, held in memory.
                store: tilolang_tilo::resources::ResourceStore::in_memory(),
                // A view reaches nothing outside the workspace by itself.
                sink: Some(Box::new(MemorySink::new())),
                root: RootView::default(),
            },
            Rc::new(ViewPlatform {
                apps: apps.clone(),
                wrapper: Rc::clone(&wrapper),
                wake,
                failed: Rc::clone(&failed),
            }),
            Rc::new(tilolang_tilo::names::Unnamed),
            Box::new(app_end),
        );
        // The package shows another's state and has no root of its own to open.
        instance.without_root();
        instance.sync();
        if let Some(why) = failed.borrow_mut().take() {
            return Err(why);
        }
        let host = instance
            .host()
            .ok_or("no runtime started for the written view")?;
        // A person's own view is their own code. Anyone else's asks first.
        if view.author.member_id().and_then(MemberId::as_person) == Some(&view.viewer) {
            host.authority()
                .trust(&ModuleHash::written_by(&view.author));
        }
        let target = ResourceRef::root(SHOWN, wrapper.ty.clone());
        let reading = json!({ "definition": view.source, "author": { "actor": view.author } });
        host.runtime()
            .set_reading(&target, &reading.to_string())
            .map_err(|error| error.to_string())?;
        let asked = Rc::new(RefCell::new(Vec::new()));
        let runtime = host.runtime();
        runtime.set_invoker(Asks(Rc::clone(&asked)));
        // A resource the view names and this holds no reading of is asked of the workspace on the
        // surface's next turn, and reads as empty until its reading comes: as what an app keeps
        // for the workspace does where the view opens.
        let wanted = Rc::new(RefCell::new(Vec::new()));
        runtime.set_readings_asked_for({
            let wanted = Rc::clone(&wanted);
            move |named: &ResourceRef| {
                wanted.borrow_mut().push(named.clone());
                Ok(Some(json!({})))
            }
        });
        // The view starts over an empty reading of every record it names, and each record is
        // asked for: it draws at once, shows nothing it has not read, and is drawn again when a
        // record comes. A record the person may not read stays as empty as it started.
        for app in &apps {
            let Some(root) = app
                .root
                .as_ref()
                .filter(|_| apps::reads_record(app, &view.source))
            else {
                continue;
            };
            let record = ResourceRef::root(root.published.clone(), root.ty.clone());
            host.runtime()
                .set_reading(&record, "{}")
                .map_err(|error| format!("{}: {error}", app.title))?;
            workspace.read(&app.id, "", None);
        }
        instance.open_with(&resource(&target), &wrapper.view);
        if instance.detail_target().is_none() {
            return Err("the written view's package did not open".to_owned());
        }
        let mut surface = Self {
            view: AppWindow::part(Box::new(window_end)),
            instance,
            workspace,
            apps,
            named: BTreeMap::new(),
            wanted,
            author: view.author.clone(),
            asked,
            state,
        };
        surface.exchange();
        Ok(surface)
    }

    /// The app the view's package names `package`.
    fn app(&self, package: &str) -> Option<Rc<App>> {
        self.apps
            .iter()
            .find(|app| app.meta.app == package)
            .cloned()
    }

    /// The app whose own id is `id`: the one a reading was asked of.
    fn kept_by(&self, id: &str) -> Option<Rc<App>> {
        self.apps.iter().find(|app| app.id == id).cloned()
    }

    /// Let the window, the instance and the workspace answer each other until none has more to
    /// say. Whether anything moved.
    fn exchange(&mut self) -> bool {
        let mut moved = false;
        for _ in 0..EXCHANGES {
            self.view.flush();
            let read = self.take_readings();
            if read {
                // A reading moved: the view runs again over it.
                self.instance.sync();
            }
            let mut changed = read | self.instance.update();
            changed |= self.ask_named();
            changed |= self.serve();
            changed |= self.view.receive();
            if !changed {
                break;
            }
            moved = true;
        }
        moved
    }

    /// Hand the view every record that was read since the last turn, and ask for each again once
    /// it changes. Whether a reading moved.
    fn take_readings(&mut self) -> bool {
        let Some(host) = self.instance.host() else {
            return false;
        };
        let mut moved = false;
        for (id, document, read) in self.workspace.take_read() {
            let Some(app) = self.kept_by(&id) else {
                continue;
            };
            // What was read: what the app keeps for the workspace, or a document of its own that
            // the view names, read as the type the view names it by.
            let of = if document.is_empty() {
                app.root
                    .as_ref()
                    .map(|root| (root.published.clone(), root.ty.clone()))
            } else {
                self.named
                    .get(&(id.clone(), document.clone()))
                    .map(|ty| (document.clone(), ty.clone()))
            };
            let Some((published, ty)) = of else {
                continue;
            };
            let (reading, revision) = match read {
                Read::Record { revision, value } => {
                    let reading = if document.is_empty() {
                        app.reading(&value)
                    } else {
                        app.reading_as(&ty, &value)
                    };
                    (reading, revision)
                }
                // No record yet: the view reads it as it would start, and is told when one comes.
                Read::Absent { revision } => (json!({}), revision),
                Read::Refused => {
                    self.state.refuse(&app.title);
                    continue;
                }
                Read::Unread(why) => {
                    tracing::debug!(app = id, why, "an app's record was not read for a view");
                    self.state.leave_unread(&app.title);
                    continue;
                }
            };
            let target = ResourceRef::root(published, ty);
            match host.runtime().set_reading(&target, &reading.to_string()) {
                Ok(changed) => moved |= changed,
                Err(error) => {
                    tracing::warn!(app = id, %error, "a view did not take an app's reading");
                }
            }
            self.workspace.read(&id, &document, Some(revision));
        }
        moved
    }

    /// Ask the workspace for every document of an app that the view names and holds no reading
    /// of yet (`@word_processor.Document:uuid:…`), as the runtime asked for them. A document is
    /// read as the person at the window, as what an app keeps for the workspace is, so the
    /// workspace's grants decide it. Whether any was asked for.
    fn ask_named(&mut self) -> bool {
        let wanted: Vec<ResourceRef> = self.wanted.borrow_mut().drain(..).collect();
        let mut asked = false;
        for named in wanted {
            // An item of a document comes within its document's reading.
            if named.item.is_some() {
                continue;
            }
            // Only a type one of the view's apps declares, and not what the app keeps for the
            // workspace, which is asked for where the view opens.
            let Some(app) = self.apps.iter().find(|app| {
                app.meta
                    .types
                    .iter()
                    .any(|declared| declared.id == named.ty)
                    && app
                        .root
                        .as_ref()
                        .is_none_or(|root| root.published != named.doc)
            }) else {
                continue;
            };
            let key = (app.id.clone(), named.doc.clone());
            if self.named.contains_key(&key) {
                continue;
            }
            self.workspace.read(&key.0, &key.1, None);
            self.named.insert(key, named.ty.clone());
            asked = true;
        }
        asked
    }


    /// Decide what the view asked since the last turn, and send what the person allowed. Whether
    /// there was anything.
    fn serve(&mut self) -> bool {
        let asked: Vec<Invocation> = self.asked.borrow_mut().drain(..).collect();
        let allowed = self.state.take_allowed();
        let any = !asked.is_empty() || !allowed.is_empty();
        for invocation in &allowed {
            self.send(invocation);
        }
        for invocation in asked {
            match self.decide(&invocation) {
                Decision::Allow => self.send(&invocation),
                Decision::Ask => {
                    if let Some(asked) = self.described(&invocation) {
                        self.state.suggest(asked, self.author.clone(), invocation);
                    }
                }
                Decision::Deny => {
                    tracing::info!(
                        what = invocation.name(),
                        "a view asked for what is not allowed"
                    );
                }
            }
        }
        for why in self.workspace.take_failed() {
            tracing::warn!(why, "what a view asked was not done");
        }
        any
    }

    /// What the workspace's policy says of `invocation`, asked by the person at the window
    /// through code its author wrote. An action is never sent before this answers `Allow`.
    fn decide(&self, invocation: &Invocation) -> Decision {
        let Invocation::Action {
            app,
            action,
            class,
            author,
            ..
        } = invocation
        else {
            // A view assigns no field of a record it does not hold.
            return Decision::Deny;
        };
        let (Some(host), Some(declared)) = (self.instance.host(), self.app(app.as_str())) else {
            return Decision::Deny;
        };
        let chain: Vec<ModuleHash> = author.iter().cloned().collect();
        host.authority().decide(
            host.viewer(),
            &chain,
            &ActionInfo {
                action: action.clone(),
                app: app.clone(),
                class: Clone::clone(class),
                own: false,
                module: declared.module.clone(),
            },
        )
    }

    /// What `invocation` would do, as the person is asked about it: the app it would change, the
    /// action in the app's own words, and everything the action is given, exactly as
    /// [`Self::send`] sends it. The words are the action's, never those of the control that
    /// asked.
    fn described(&self, invocation: &Invocation) -> Option<Asked> {
        let Invocation::Action {
            app,
            action,
            destructive,
            ..
        } = invocation
        else {
            return None;
        };
        let declared = self.app(app.as_str())?;
        let (_, arguments) = sent(&declared, invocation)?;
        Some(Asked {
            app: declared.title.clone(),
            action: declared.label(action),
            arguments: arguments
                .iter()
                .map(|(name, value)| (name.clone(), spoken(value)))
                .collect(),
            destructive: *destructive,
        })
    }

    /// Send `invocation` to its app's own API, as the person at the window.
    fn send(&mut self, invocation: &Invocation) {
        let Invocation::Action { app, action, .. } = invocation else {
            return;
        };
        let Some(declared) = self.app(app.as_str()) else {
            return;
        };
        let Some((resource, arguments)) = sent(&declared, invocation) else {
            return;
        };
        self.workspace
            .invoke(&declared.id, &resource, declared.verb(action), arguments);
    }
}

/// Where `invocation` goes in the app `declared` and what it is given there, by the names the
/// app's own action takes: what is sent, and so what the person is asked about.
fn sent(declared: &App, invocation: &Invocation) -> Option<(String, Map<String, Json>)> {
    let Invocation::Action { action, target, .. } = invocation else {
        return None;
    };
    let resource = target
        .as_ref()
        .map(ResourceRef::id_string)
        .or_else(|| declared.root.as_ref().map(|root| root.published.clone()))?;
    Some((
        resource,
        declared.arguments(action, invocation.api_arguments()),
    ))
}

/// An argument's value as a person reads it: text as it is, anything else as it is sent.
fn spoken(value: &Json) -> String {
    match value {
        Json::String(text) => text.clone(),
        other => other.to_string(),
    }
}

#[cfg(test)]
impl Surface {
    /// One turn, as a frame takes it.
    pub(crate) fn turn(&mut self) {
        self.exchange();
    }

    /// The nodes of kind `kind` the view shows, each its key and the text it holds.
    pub(crate) fn nodes(&self, kind: &str) -> Vec<(String, Option<String>)> {
        fn walk(
            node: &tilolang_tilo::runtime::UiNode,
            kind: &str,
            out: &mut Vec<(String, Option<String>)>,
        ) {
            if node.kind == kind {
                let text = match node.props.get("value") {
                    Some(Value::Text(text)) => Some(text.clone()),
                    _ => None,
                };
                out.push((node.key.clone(), text));
            }
            for child in &node.children {
                walk(child, kind, out);
            }
        }
        let mut out = Vec::new();
        if let Some(view) = self
            .instance
            .view(tilo_render_protocol::patch::Level::Detail)
        {
            walk(view.borrow().tree(), kind, &mut out);
        }
        out
    }

    /// What the first node of kind `kind` the view shows holds in its prop `prop`.
    fn held(&self, kind: &str, prop: &str) -> Option<Value> {
        fn find(node: &tilolang_tilo::runtime::UiNode, kind: &str, prop: &str) -> Option<Value> {
            if node.kind == kind {
                return node.props.get(prop).cloned();
            }
            node.children
                .iter()
                .find_map(|child| find(child, kind, prop))
        }
        let view = self
            .instance
            .view(tilo_render_protocol::patch::Level::Detail)?;
        let view = view.borrow();
        find(view.tree(), kind, prop)
    }

    /// Every number the first node of kind `kind` holds in its prop `prop`, in order, through
    /// the lists and cases it is given in: a chart's readings.
    pub(crate) fn numbers(&self, kind: &str, prop: &str) -> Vec<f64> {
        fn walk(value: &Value, out: &mut Vec<f64>) {
            match value {
                Value::Float(number) => out.push(*number),
                Value::List(items) | Value::Tuple(items) => {
                    for item in items {
                        walk(item, out);
                    }
                }
                Value::Opt(Some(inner)) => walk(inner, out),
                Value::Enum(case) => {
                    for field in case.fields.values() {
                        walk(field, out);
                    }
                }
                _ => {}
            }
        }
        let mut out = Vec::new();
        if let Some(value) = self.held(kind, prop) {
            walk(&value, &mut out);
        }
        out
    }

    /// The texts the first node of kind `kind` holds in its list prop `prop`, in order.
    pub(crate) fn words(&self, kind: &str, prop: &str) -> Vec<String> {
        match self.held(kind, prop) {
            Some(Value::List(items)) => items
                .into_iter()
                .filter_map(|item| match item {
                    Value::Text(text) => Some(text),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        }
    }

    /// How many widgets named `name` the window drew the view with.
    pub(crate) fn drawn(&self, name: &str) -> usize {
        fn walk(widget: &dyn Widget, name: &str, found: &mut usize) {
            if widget.name() == name {
                *found = found.saturating_add(1);
            }
            widget.for_each_child(&mut |child| walk(child, name, found));
        }
        let mut found = 0;
        walk(&self.view, name, &mut found);
        found
    }

    /// Press the control `key`, as the person at the window does.
    pub(crate) fn press(&mut self, key: &str) {
        let view = self
            .instance
            .view(tilo_render_protocol::patch::Level::Detail)
            .expect("the view is open");
        view.borrow().press(key).expect("the press runs");
        self.exchange();
    }
}

/// The surface stands for the app window's window in the layout and in every walk of the widgets.
impl ui::widget::Decorator for Surface {
    type Inner = dyn Widget;

    fn inner(&self) -> &(dyn Widget + 'static) {
        &self.view
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        &mut self.view
    }

    fn name(&self) -> &'static str {
        "WrittenView"
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.view.receive();
        Widget::sync(&mut self.view, state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        // What the person did, and what was read, runs before what it shows is read.
        let moved = self.exchange();
        let child = Widget::update(&mut self.view, state, path);
        moved || child
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        Widget::handle_event(&mut self.view, event, ctx);
        // What the person did goes to the instance at once, not on the next frame.
        self.view.flush();
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        self.view
            .draw_child(scene, transform, text, theme, presence);
    }

    fn needs_idle_sync(&self) -> bool {
        self.instance.owes_a_frame() || Widget::needs_idle_sync(&self.view)
    }

    fn has_pending_visual_work(&self) -> bool {
        self.instance.owes_a_frame() || Widget::has_pending_visual_work(&self.view)
    }

    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let instance = self
            .instance
            .wakes_in()
            .and_then(|wait| ui::time::now().checked_add(wait));
        match (instance, Widget::next_wake_deadline(&self.view)) {
            (Some(instance), Some(view)) => Some(instance.min(view)),
            (instance, view) => instance.or(view),
        }
    }

    fn take_structure_dirty(&mut self) -> bool {
        Widget::take_structure_dirty(&mut self.view)
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        Widget::take_structure_change(&mut self.view)
    }
}
