//! The window of a tilolang app in the app's own process: its instance and its UI host half,
//! side by side over the patch protocol.
//!
//! The app's views run in an instance (`tilolang_tilo::instance`), which holds the data and
//! sends component patches; the window's widgets are the UI host's half
//! (`tilo_ui_host::AppWindow`), which builds the shell from them and reports what the person
//! did. Here both run in one process, joined by an in-process transport that carries the very
//! batches a socket would, validated as the UI host validates them. In a workspace's UI host the
//! same halves run in two processes.
//!
//! What this window adds is the framework's: the workspace record it hands the instance, the door
//! the instance's commits go through (attributed, undoable, replayed by every peer), the
//! person's Undo, presence, and the door's endpoints for agents and search.

#[cfg(test)]
mod tests;

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use tilo_ui_host::AppWindow;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{self, Config, Instance, Mode, Outgoing, Platform, queue_commits};
use tilolang_tilo::runtime::{OutsideSink, ResourceRef};
use tilolang_tilo::{HostEffect, OpenEffect};
use ui::framework::{ResourceSurface, RunMode};
use ui::tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use ui::widget::controls::ControlResult;
use ui::widget::{AppModel, Element, ModelPath, Widget};

use crate::i18n::t_args;
use crate::names::Names;
use crate::resource::{Live, Workspace, WorkspaceState, caller_of};
use ui::tilo_resource::profiling::Phase;

/// The two vocabularies the window answers on the door: what an agent asks of the host, and
/// showing what a search hit names.
fn door_endpoints(
    host: &Rc<RefCell<Option<Host>>>,
    door_work: &Rc<Cell<bool>>,
    outgoing: &Outgoing,
    open_requests: &Rc<RefCell<Vec<String>>>,
) -> (Element, Element) {
    let answering = (Rc::clone(host), Rc::clone(door_work), Rc::clone(outgoing));
    let endpoint = ui::framework::EffectEndpoint::new(
        move |effect: HostEffect, caller: &ui::framework::EffectCaller| {
            let host = answering
                .0
                .borrow()
                .clone()
                .ok_or_else(|| "the tilolang host has not started".to_owned())?;
            let who = caller.invoker.as_ref().map(caller_of);
            let first = host.next_ticket();
            let answered = host.answer(effect, who.as_ref());
            queue_commits(&host, &answering.2, first, Some(caller.invocation));
            answering.1.set(true);
            answered
        },
    );
    let opening = Rc::clone(open_requests);
    let open_endpoint = ui::framework::EffectEndpoint::new(
        move |effect: OpenEffect, _: &ui::framework::EffectCaller| {
            let OpenEffect::Open { anchor } = effect;
            opening.borrow_mut().push(anchor);
            Ok(ui::tilo_actions::ActionOutput::default())
        },
    );
    (Box::new(endpoint), Box::new(open_endpoint))
}

/// What the instance asks of the app's process: the installed package and its later builds,
/// the platform's phrases and clock, waking the window, and publishing on the door.
struct InProcess;

/// What the live window publishes on the door while it lives.
struct Published(Live);

impl instance::Publication for Published {
    fn update(&self) {
        self.0.update();
    }

    fn pending(&self) -> bool {
        self.0.pending()
    }
}

impl Platform for InProcess {
    fn load(&self, host: &Host) -> u64 {
        crate::app::load(host)
    }

    fn latest(&self) -> Option<instance::Update> {
        crate::updates::latest().map(|update| instance::Update {
            generation: update.generation,
            bundle: update.bundle,
        })
    }

    fn follow_updates(&self) {
        crate::updates::subscribe(ui::wake::WakeHandle::new());
    }

    fn follows_updates(&self) -> bool {
        crate::updates::followed()
    }

    fn surface_changed(&self, app: &str, bundle: &tilolang_tilo::bundle::Bundle) {
        match bundle.surface() {
            Ok(surface) => ui::app_surface::register_package(app, &surface),
            // The surface registered before stays: receivers keep deciding against it.
            Err(error) => tracing::error!(app, %error, "the updated package's surface is unusable"),
        }
    }

    /// The platform's phrases, which the standard library's frames say.
    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        let args: Vec<(&str, ui::i18n::ArgValue)> = args
            .iter()
            .map(|(name, value)| (name.as_str(), value.as_str().into()))
            .collect();
        t_args(id, &args)
    }

    /// The toolkit's clock, as every time the window shows is: a capture pins it, so a
    /// scenario's messages are stamped with the capture's time.
    fn now(&self) -> String {
        ui::clock::local_now()
            .with_timezone(&chrono::Utc)
            .format("%Y-%m-%dT%H:%M:%SZ")
            .to_string()
    }

    /// The zone of the toolkit's clock: the person's, which "tomorrow 9am" is read in.
    fn zone(&self) -> i64 {
        i64::from(ui::clock::local_now().offset().local_minus_utc())
    }

    fn waker(&self) -> tilolang_tilo::background::Waker {
        let wake = ui::wake::WakeHandle::new();
        Arc::new(move || wake.wake())
    }

    fn publish(
        &self,
        host: &Host,
        wake: tilolang_tilo::background::Waker,
    ) -> Option<Box<dyn instance::Publication>> {
        Some(Box::new(Published(Live::register(host, wake))))
    }
}

/// The names of the workspace, as the instance shows them.
struct WorkspaceNames(Names);

impl tilolang_tilo::runtime::NameHost for WorkspaceNames {
    fn name(&self, who: &tilolang_tilo::runtime::Principal) -> String {
        self.0.label(&who.0)
    }
}

/// A later build the window took, until its first frame.
struct Undrawn {
    generation: u64,
    read_at: std::time::Instant,
}

/// The window. See the module docs.
pub struct Window {
    app: String,
    me: String,
    mode: RunMode,
    instance: Instance,
    /// The UI host's half: the shell, the Ask bar and the door's endpoints.
    view: AppWindow,
    names: Names,
    /// Where the indexes of the workspaces this window shows are kept: `None` keeps them in memory.
    indexes: Option<std::path::PathBuf>,
    /// The workspace this window shows, which names where its indexes are kept.
    workspace: Option<String>,
    /// The record of the workspace it shows, when that is not the workspace's record of the app (a
    /// member's own): its indexes are its own.
    record: Option<String>,
    /// The workspace's record, while it is read off the frame and the window shows what the indexes
    /// hold ([`Self::opening`]).
    opening: Option<ui::record::RecordOpening>,
    undo: ui::undo::UndoAccess,
    /// A later build this window took and has not drawn yet: its first frame says so.
    undrawn: Option<Undrawn>,
}

impl Window {
    /// The window of the package `app` for the participant `me`.
    #[must_use]
    pub fn new(app: String, me: String, mode: RunMode) -> Self {
        let names = Names::new(&me);
        let (app_end, window_end) = tilo_render_protocol::patch::local::pair();
        let instance = Instance::new(
            Config {
                app: app.clone(),
                me: me.clone(),
                mode: match mode {
                    RunMode::Live => Mode::Live,
                    RunMode::Past => Mode::Past,
                    RunMode::Capture => Mode::Capture,
                },
                device: None,
                index_dir: None,
                record_loader: None,
                sink: None,
            },
            Rc::new(InProcess),
            Rc::new(WorkspaceNames(names.clone())),
            Box::new(app_end),
        );
        let (endpoint, open_endpoint) = door_endpoints(
            &instance.shared_host(),
            &instance.door_work(),
            &instance.outgoing(),
            &instance.open_requests(),
        );
        let view = AppWindow::new(Box::new(window_end), vec![endpoint, open_endpoint]);
        Self {
            app,
            me,
            mode,
            instance,
            view,
            names,
            indexes: None,
            workspace: None,
            record: None,
            opening: None,
            undo: ui::undo::UndoAccess::new(),
            undrawn: None,
        }
    }

    /// The anchors search hits and agents asked the window to show, until it shows them.
    #[must_use]
    pub fn open_requests(&self) -> Rc<RefCell<Vec<String>>> {
        self.instance.open_requests()
    }

    /// This window on the device `device`: the one that carries out the outside effects its
    /// workspace queues.
    #[must_use]
    pub fn on_device(mut self, device: &str) -> Self {
        self.instance.set_device(Some(device.to_owned()));
        self
    }

    /// This window keeping the indexes of the workspaces it shows under `dir`, so the workspace opens
    /// next time over them rather than reading its whole record.
    #[must_use]
    pub fn keep_indexes_in(mut self, dir: std::path::PathBuf) -> Self {
        self.indexes = Some(dir);
        self.place_indexes();
        self
    }

    /// This window in the workspace `workspace`, as its launch names it: where its indexes are kept is
    /// known before the workspace's record is.
    #[must_use]
    pub fn in_workspace(mut self, workspace: &str) -> Self {
        self.workspace = Some(workspace.to_owned());
        self.place_indexes();
        self
    }

    /// This window showing `record` of its workspace, a record other than the workspace's record
    /// of the app (the person's own): it keeps that record's indexes apart from the workspace's.
    #[must_use]
    pub fn of_record(mut self, record: &str) -> Self {
        self.record = Some(record.to_owned());
        self.place_indexes();
        self
    }

    /// Where the indexes of this window's workspace are kept, when they are kept on disk: a past
    /// version and a capture build theirs in memory.
    fn place_indexes(&mut self) {
        let dir = match (self.mode, &self.indexes, &self.workspace) {
            (RunMode::Live, Some(indexes), Some(workspace)) => Some(
                tilolang_tilo::index::record_dir(indexes, workspace, self.record.as_deref()),
            ),
            _ => None,
        };
        self.instance.set_index_dir(dir);
    }

    /// This window shown before the workspace's record, which `record` reads off the frame: it opens
    /// over the workspace's indexes, and a write, or a field no index holds, waits for the record.
    /// Its view takes it over once the record is had ([`Self::taken_over`]).
    #[must_use]
    pub fn opening(mut self, record: ui::record::RecordOpening) -> Self {
        let reading = record.clone();
        self.instance
            .set_record_loader(Box::new(move || reading.document()));
        self.opening = Some(record);
        self
    }

    /// This window, shown before the workspace's record, taken over by the view of the record: it
    /// reads the record from now on, through the framework as every window does.
    #[must_use]
    pub fn taken_over(mut self) -> Self {
        self.opening = None;
        self.instance.took_record();
        self
    }

    /// This window deciding what its runtime invokes by the person's standing in the workspace as
    /// `source` reads it, the workspace's policy the door decides by.
    #[must_use]
    pub fn with_standing(
        self,
        source: impl Fn() -> Option<ui::permissions::Standing> + 'static,
    ) -> Self {
        self.instance.set_standing(Rc::new(source));
        self
    }

    /// This window keeping its package's types with an origin in step with `origin`'s service.
    #[must_use]
    pub fn with_origin(mut self, origin: tilolang_tilo::origin::SharedOrigin) -> Self {
        self.instance.add_origin(origin);
        self
    }

    /// This window sending the outside effects it settles to `sink` rather than the development
    /// log.
    #[must_use]
    pub fn with_sink(mut self, sink: impl OutsideSink + 'static) -> Self {
        self.instance.set_sink(Box::new(sink));
        self
    }

    /// The instance's batch shown: what it sent since the window last looked.
    fn show_sent(&mut self) -> bool {
        self.instance.flush();
        self.view.receive()
    }

    /// Queue what the runtime committed for the door.
    pub fn commit_planned(&mut self) -> Vec<u64> {
        let tickets = self.instance.commit_planned();
        self.show_sent();
        tickets
    }

    /// The door answered an invocation the window handed over: done, or refused with `error`.
    /// Whether it was the window's own (a commit).
    pub fn answered(&mut self, address: &ActionAddress, error: Option<String>) -> bool {
        let own = self.instance.answered(address, error);
        self.show_sent();
        own
    }

    // ----- for tests -----------------------------------------------------------------------

    /// The root view's tree, as the window holds it.
    #[must_use]
    pub fn root_tree(&self) -> Option<tilo_ui_host::UiNode> {
        self.view.root_tree()
    }

    /// The tree of the window after the root's.
    #[must_use]
    pub fn detail_tree(&self) -> Option<tilo_ui_host::UiNode> {
        self.view.detail_tree()
    }

    /// The composed shell, as the window last built it.
    #[must_use]
    pub fn composed(&self) -> Option<&tilo_ui_host::UiNode> {
        self.view.composed()
    }

    /// The resource the window after the root's shows.
    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.instance.detail_target()
    }

    /// Report what a person did: an event of a control of the composed shell, by its key there.
    pub fn act(&self, event: tilo_ui_host::reconcile::UiEvent) {
        self.view.act(event);
    }

    /// The shell.
    #[must_use]
    pub fn shell_ref(&self) -> Option<&ui::widget::NavShell> {
        self.view.shell_ref()
    }

    /// The shell, to change it as a person would.
    pub fn shell_mut(&mut self) -> Option<&mut ui::widget::NavShell> {
        self.view.shell_mut()
    }

    /// The UI host's half of the window.
    #[must_use]
    pub fn view(&self) -> &AppWindow {
        &self.view
    }

    /// The instance the window runs.
    #[must_use]
    pub fn instance(&self) -> &Instance {
        &self.instance
    }

    /// The host, once the window has the record.
    #[must_use]
    pub fn host(&self) -> Option<Host> {
        self.instance.host()
    }

    #[cfg(test)]
    fn current_host(&self) -> Option<Host> {
        self.instance.host()
    }

    /// Close the view after the root's, as Back does.
    #[cfg(test)]
    fn close_detail(&mut self) {
        self.instance.close_detail();
        self.show_sent();
    }
}

/// Where in the package a participant is: the resource their window shows beside the root.
fn presence_key(app: &str, target: &ResourceRef) -> ui::presence::PresenceKey {
    ui::presence::PresenceKey::Resource {
        app: app.to_owned(),
        id: target.id_string(),
        kind: target.ty.clone(),
    }
}

/// The participants other than `me` at each resource of the package `app`.
fn peers_of(presence: &ui::presence::PresenceCtx, app: &str, me: &str) -> HashMap<String, String> {
    presence
        .selections
        .iter()
        .filter_map(|(key, at)| match key {
            ui::presence::PresenceKey::Resource { app: of, id, .. } if of == app => at
                .iter()
                .find(|(user, _)| user.0 != me)
                .map(|(user, _)| (id.clone(), user.0.clone())),
            _ => None,
        })
        .collect()
}

impl ui::presence::capability::PublishesPresence for Window {
    /// The resource the window shows beside its root, so a collaborator's list shows this
    /// participant at its row.
    fn presence_payload(
        &self,
    ) -> Option<(
        ui::presence::PresenceKey,
        ui::collaboration::PresencePayload,
    )> {
        let detail = self.instance.detail_target()?;
        Some((
            presence_key(&self.app, detail),
            ui::collaboration::PresencePayload::Selection { start: 0, end: 0 },
        ))
    }
}

impl ui::widget::controls::ActionControls for Window {
    fn take_control_invocation(&mut self) -> Option<ControlInvocation> {
        self.take_attributed_control_invocation()
            .map(|(invocation, _)| invocation)
    }

    fn take_attributed_control_invocation(
        &mut self,
    ) -> Option<ui::widget::controls::AttributedControlInvocation> {
        let taken: Option<(ControlInvocation, Option<InvocationId>)> =
            self.instance.take_invocation();
        self.show_sent();
        match taken {
            Some(taken) => Some(taken),
            None => ui::widget::controls::take_attributed_control_invocation(&mut self.view),
        }
    }

    fn on_control_invocation_result(&mut self, address: &ActionAddress, result: &ControlResult) {
        let error = result.as_ref().err().map(ToString::to_string);
        if !self.answered(address, error) {
            ui::widget::controls::on_control_invocation_result(&mut self.view, address, result);
        }
    }
}

impl ui::undo::UndoControl for Window {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> ui::undo::UndoStatus) {
        self.undo.publish(&status());
    }

    fn take_undo_request(&mut self) -> Option<ui::undo::UndoRequest> {
        self.undo.take_request()
    }
}

impl ResourceSurface<Workspace> for Window {
    fn present(&mut self, _state: &Rc<WorkspaceState>) {
        // The record moved: what the views read may have changed, and outside effects may wait.
        self.instance.moved();
    }

    fn stage(&mut self, scenario: Option<&str>) {
        self.instance.stage(scenario);
    }

    /// A change mark in the window asks the shell to show the task run that made the change.
    fn take_host_intent(&mut self) -> Option<ui::widget::AppHostIntent> {
        let run = self.view.take_shown_run()?;
        Some(ui::widget::AppHostIntent::ShowTaskRun { group: run })
    }
}

impl Widget for Window {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoControl,
        dyn ui::presence::capability::PublishesPresence
    );

    fn name(&self) -> &'static str {
        "TilolangWindow"
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.view]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.view]
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        let _phase = Phase::start("tl.sync");
        // Shown before the workspace's record, the window reads none: it opens over the indexes.
        let doc = self
            .opening
            .is_none()
            .then(|| ui::doc::document_ref(state, path).0);
        {
            let _views = Phase::start("tl.views");
            self.instance.sync(doc);
        }
        if let Some(generation) = self.instance.take_taken_build() {
            let read_at = crate::updates::latest()
                .filter(|update| update.generation == generation)
                .map_or_else(std::time::Instant::now, |update| update.read_at);
            self.undrawn = Some(Undrawn {
                generation,
                read_at,
            });
        }
        self.view.receive();
        self.view.sync(state, path);
    }

    fn update(&mut self, state: &AppModel, path: &ModelPath) -> bool {
        let _phase = Phase::start("tl.update");
        self.view.flush();
        let changed = {
            let _handled = Phase::start("tl.events");
            self.instance.update()
        };
        let received = self.view.receive();
        // The person's Undo on a notice is the framework's undo of the record, as Ctrl+Z is here.
        for asked in self.instance.take_history() {
            match asked {
                instance::HistoryAsk::Undo => {
                    self.undo.request(ui::undo::UndoRequest::Undo);
                }
                instance::HistoryAsk::Redo => {
                    self.undo.request(ui::undo::UndoRequest::Redo);
                }
                // The framework's undo here takes back the person's last step only; a step or
                // a run named from a view is taken back where a tilolang host runs the
                // instance, on the record's own history verb.
                instance::HistoryAsk::UndoStep(_) | instance::HistoryAsk::UndoRun(_) => {
                    tracing::warn!(?asked, "an undo named from a view needs the tilolang host");
                    self.instance
                        .history_failed(crate::i18n::t("tilolang-undo-unavailable"));
                }
            }
        }
        let child = self.view.update(state, path);
        changed || received || child
    }

    /// The body's deadlines, and when the time a view read (`today`) next reads differently:
    /// the window sleeps until then, and the frame then runs again the views that read it.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let views = self.instance.wakes_in().and_then(|wait| {
            ui::time::now().checked_add(ui::time::Duration::from_secs(wait.as_secs()))
        });
        match (views, self.view.next_wake_deadline()) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        }
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.view.handle_event(event, ctx);
    }

    fn build_layout(
        &mut self,
        tree: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        nodes: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.view.build_layout(tree, theme, nodes)
    }

    fn apply_layout(
        &mut self,
        tree: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.view.apply_layout(tree, node, origin);
    }

    fn get_rect(&self) -> ui::kurbo::Rect {
        self.view.get_rect()
    }

    fn draw(
        &mut self,
        scene: &mut ui::vello::Scene,
        transform: ui::kurbo::Affine,
        text: &mut ui::TextRenderer,
        theme: &ui::Theme,
        presence: &ui::presence::PresenceCtx,
    ) {
        if self.names.follow(presence) {
            self.instance.names_changed();
        }
        self.instance
            .set_peers(peers_of(presence, &self.app, &self.me));
        // The workspace's policy names people; presence pairs each participant with theirs.
        self.instance
            .set_persons(presence.roster().into_iter().filter_map(|user| {
                let person = presence.person_of(&ui::permissions::UserId(user.id.clone()))?;
                Some((user.id, person.0.clone()))
            }));
        self.view.draw(scene, transform, text, theme, presence);
        if let Some(undrawn) = self.undrawn.take() {
            let at = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default();
            eprintln!(
                "tilolang: {} draws build {} at {} ms, {} ms after it arrived",
                self.app,
                undrawn.generation,
                at.as_millis(),
                undrawn.read_at.elapsed().as_millis()
            );
        }
    }

    /// The window paints nothing of its own: what changed is what its body repaints.
    fn collect_damage(&self, regions: &mut Vec<ui::kurbo::Rect>) {
        self.view.collect_damage(regions);
    }

    fn take_structure_dirty(&mut self) -> bool {
        self.view.take_structure_dirty()
    }

    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        self.view.take_structure_change()
    }
}
