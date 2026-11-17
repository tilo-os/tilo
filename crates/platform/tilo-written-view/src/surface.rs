//! The widget that draws one written view. See the crate's docs.

use std::cell::RefCell;
use std::rc::Rc;

use serde_json::json;
use tilo_actions::Actor;
use tilo_render_protocol::patch::local;
use tilo_ui_host::AppWindow;
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
use crate::state::State;
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
            Box::new(ui::widget::EmptyState::new(tilo_ui_host::i18n::t(
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
    /// What the view's package keeps of its own: nothing.
    record: tilo_resource::Doc,
    workspace: Box<dyn Workspace>,
    /// The apps the view names.
    apps: Vec<Rc<App>>,
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
        let apps = apps::named(&view.source);
        tilo_ui_host::i18n::ensure();
        let wake = ui::wake::WakeHandle::new();
        state.woken_by(wake.clone());
        let failed = Rc::new(RefCell::new(None));
        let (app_end, window_end) = local::pair();
        let mut instance = Instance::new(
            Config {
                app: wrapper.compiled.meta.app.clone(),
                me: view.viewer.id().to_owned(),
                mode: Mode::Live,
                device: None,
                index_dir: None,
                record_loader: None,
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
        let record = tilo_resource::Doc::new();
        instance.sync(Some(&record));
        if let Some(why) = failed.borrow_mut().take() {
            return Err(why);
        }
        let host = instance
            .host()
            .ok_or("no runtime started for the written view")?;
        // A person's own view is their own code. Anyone else's asks first.
        if view.author == Actor::Member(view.viewer.clone()) {
            host.authority()
                .trust(&ModuleHash::written_by(&view.author));
        }
        let target = ResourceRef::root(SHOWN, wrapper.ty.clone());
        let reading = json!({ "definition": view.source, "author": { "actor": view.author } });
        host.runtime()
            .set_reading(&target, &reading.to_string())
            .map_err(|error| error.to_string())?;
        let asked = Rc::new(RefCell::new(Vec::new()));
        host.runtime().set_invoker(Asks(Rc::clone(&asked)));
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
            workspace.read(&app.meta.app, None);
        }
        instance.open_with(&resource(&target), &wrapper.view);
        if instance.detail_target().is_none() {
            return Err("the written view's package did not open".to_owned());
        }
        let mut surface = Self {
            view: AppWindow::part(Box::new(window_end)),
            instance,
            record,
            workspace,
            apps,
            author: view.author.clone(),
            asked,
            state,
        };
        surface.exchange();
        Ok(surface)
    }

    fn app(&self, id: &str) -> Option<Rc<App>> {
        self.apps.iter().find(|app| app.meta.app == id).cloned()
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
                self.instance.sync(Some(&self.record));
            }
            let mut changed = read | self.instance.update();
            changed |= self.answer_commits();
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
        for (id, read) in self.workspace.take_read() {
            let Some(app) = self.app(&id) else {
                continue;
            };
            let Some(root) = &app.root else {
                continue;
            };
            let (reading, revision) = match read {
                Read::Record { revision, value } => (app.reading(&value), revision),
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
            let target = ResourceRef::root(root.published.clone(), root.ty.clone());
            match host.runtime().set_reading(&target, &reading.to_string()) {
                Ok(changed) => moved |= changed,
                Err(error) => {
                    tracing::warn!(app = id, %error, "a view did not take an app's reading");
                }
            }
            self.workspace.read(&id, Some(revision));
        }
        moved
    }

    /// The package keeps no record: a step it hands over is answered as refused.
    fn answer_commits(&mut self) -> bool {
        let mut any = false;
        while let Some((invocation, _)) = self.instance.take_invocation() {
            self.instance.answered(
                &invocation.address,
                Some("a written view keeps no record of its own".to_owned()),
            );
            any = true;
        }
        any
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
                    if let Some((app, action)) = self.described(&invocation) {
                        self.state
                            .suggest(app, action, self.author.clone(), invocation);
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

    /// The app `invocation` would change and what it would do, in words.
    fn described(&self, invocation: &Invocation) -> Option<(String, String)> {
        let Invocation::Action { app, action, .. } = invocation else {
            return None;
        };
        let declared = self.app(app.as_str())?;
        Some((declared.title.clone(), declared.label(action)))
    }

    /// Send `invocation` to its app's own API, as the person at the window.
    fn send(&mut self, invocation: &Invocation) {
        let Invocation::Action {
            app,
            action,
            target,
            ..
        } = invocation
        else {
            return;
        };
        let Some(declared) = self.app(app.as_str()) else {
            return;
        };
        let resource = target
            .as_ref()
            .map(ResourceRef::id_string)
            .or_else(|| declared.root.as_ref().map(|root| root.published.clone()));
        let Some(resource) = resource else {
            return;
        };
        let arguments = declared.arguments(action, invocation.api_arguments());
        self.workspace.invoke(
            &declared.meta.app,
            &resource,
            declared.verb(action),
            arguments,
        );
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
            .view(tilo_render_protocol::patch::Lane::Detail)
        {
            walk(view.borrow().tree(), kind, &mut out);
        }
        out
    }

    /// Press the control `key`, as the person at the window does.
    pub(crate) fn press(&mut self, key: &str) {
        let view = self
            .instance
            .view(tilo_render_protocol::patch::Lane::Detail)
            .expect("the view is open");
        view.borrow().press(key).expect("the press runs");
        self.exchange();
    }
}

/// The surface stands for the UI host's window in the layout and in every walk of the widgets.
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
