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
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;
use std::sync::Arc;

use tilo_ui_host::AppWindow;
use tilolang_tilo::host::Host;
use tilolang_tilo::instance::{self, Config, Instance, Mode, Outgoing, Platform, queue_commits};
use tilolang_tilo::personal::{Keep, Personal};
use tilolang_tilo::runtime::{OutsideSink, ResourceRef};
use tilolang_tilo::{HostEffect, OpenEffect};
use ui::framework::{ResourceSurface, RunMode, WorkspaceRecord};
use ui::tilo_actions::{ActionAddress, ControlInvocation, InvocationId};
use ui::tilo_resource::Invoke;
use ui::widget::controls::ControlResult;
use ui::widget::{AppModel, Element, ModelPath, Widget};

use crate::names::Names;
use crate::resource::{Live, Workspace, WorkspaceState, caller_of};
use ui::tilo_resource::profiling::Phase;
use ui::widget::DrawChild;

/// The two vocabularies the window answers on the door: what an agent asks of the host, and
/// showing what a search hit names.
fn door_endpoints(
    host: &Rc<RefCell<Option<Host>>>,
    door_work: &Rc<Cell<bool>>,
    outgoing: &Outgoing,
    open_requests: &Rc<RefCell<Vec<OpenEffect>>>,
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
            opening.borrow_mut().push(effect);
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

    /// The platform's phrases, which the standard library's frames say, in the person's
    /// language.
    fn phrase(&self, id: &str, args: &[(String, String)]) -> String {
        tilo_ui_host::i18n::word(id, args, id)
    }

    /// When the person's language, or a catalogue, moves, what the window published is
    /// published again in it.
    fn words_revision(&self) -> u64 {
        tilo_ui_host::i18n::revision()
    }

    /// The package's words, and the one language its window speaks: the person's when the
    /// package ships it, the package's source language otherwise, so the app's copy, the
    /// chrome's words and the toolkit's dates all read in the same language.
    fn add_words(&self, window: bool, catalogues: &[(String, String)]) {
        tilo_ui_host::i18n::ensure();
        if window {
            ui::i18n::speak_window_language(catalogues);
        } else {
            for (locale, text) in catalogues {
                ui::i18n::I18nManager::add_shared_resource(locale, text.clone());
            }
        }
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

    /// The person's zone by name, so a day begins and a repeating item keeps its hour across a
    /// change of clocks, as the toolkit's dates read them.
    fn zone_name(&self) -> Option<String> {
        ui::time::local_zone_name()
    }

    /// The day the person's region starts its week on, as the toolkit's dates start it.
    fn week_start(&self) -> i64 {
        i64::from(
            ui::format::region::current()
                .first_day
                .weekday()
                .num_days_from_monday(),
        )
    }

    /// A day or a time of day in a sentence, as the toolkit's lists show them, so a row's line
    /// and a notice carry no stored text.
    fn shown(&self, kind: &str, value: &str) -> String {
        tilo_ui_host::date::shown_text(kind, value)
    }

    /// The clock the person reads: theirs, from Settings, so a time an app writes
    /// (`a.fetched.clock()`) reads as the clock its window shows.
    fn clock(&self, hour: u32, minute: u32) -> String {
        ui::clock::hour_minute(hour, minute)
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
    /// The history verbs the instance's views asked for, addressed at the record's resource
    /// where the framework publishes them, waiting for the door.
    history: VecDeque<ControlInvocation>,
    /// What a control's Undo or Redo asked, as the window's Undo controls publish it, until the
    /// next frame hands it to the same door as the views' history verbs.
    undo: ui::undo::UndoAccess,
    /// A later build this window took and has not drawn yet: its first frame says so.
    undrawn: Option<Undrawn>,
    /// How many times the window had published on the door when its controls were last bound
    /// to what it published ([`crate::resource::published_since`]).
    bound: u64,
    /// The workspace's record of each person's own state, when the window has it: the view state
    /// they set and what they have seen, so the place they picked is still theirs after a restart
    /// and on their other devices ([`Personal`]).
    personal: Option<WorkspaceRecord<Personal>>,

    /// Input or a new document value changed what this widget paints: it
    /// repaints once, and rests after the frame draws it.
    dirty: bool,
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
                root: tilolang_tilo::host::RootView::default(),
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
            dirty: true,
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
            history: VecDeque::new(),
            undo: ui::undo::UndoAccess::new(),
            undrawn: None,
            bound: 0,
            personal: None,
        }
    }

    /// What search hits and agents asked the window to show, until it shows them.
    #[must_use]
    pub fn open_requests(&self) -> Rc<RefCell<Vec<OpenEffect>>> {
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

    /// This window keeping the person's own state of the workspace in `record`: the view state
    /// they set in their views, and what they have seen. What their entries hold is
    /// given to the views when the window shows ([`Self::bring_personal`]), and what they change
    /// is kept back into them ([`Self::keep_personal`]).
    #[must_use]
    pub fn keeping_personal(mut self, record: Option<WorkspaceRecord<Personal>>) -> Self {
        self.personal = record;
        self
    }

    /// Give the views what the record keeps of this person: how their window shows its resource,
    /// where they are in it, and what they have seen. A slot already holding its value is left
    /// alone, so this runs every frame.
    fn bring_personal(&mut self, record: &WorkspaceRecord<Personal>) {
        let state = record.get();
        let kept = state.of(&self.me);
        self.instance.set_view_states(
            kept.slots
                .iter()
                .map(|slot| (slot.key(), slot.value.clone())),
        );
        // Everyone's marks, not this person's alone: a conversation shows who read to a line.
        self.instance.set_seen(
            state.seen.iter().map(|seen| {
                (
                    seen.person.clone(),
                    seen.resource.clone(),
                    Some(seen.mark.clone()),
                )
            }),
            &kept
                .runs
                .iter()
                .map(|reviewed| reviewed.run.clone())
                .collect::<Vec<_>>(),
        );
    }

    /// Keep what this person changed of their own state since the last frame in the record: the
    /// view state they set, and what they marked seen. A change the record cannot take
    /// stays on this device, told once, rather than stopping the window.
    fn keep_personal(&mut self, record: &WorkspaceRecord<Personal>) {
        let keep_it = |keep: &Keep, what: &str| {
            let invoke = match Invoke::<Personal>::apply(keep) {
                Ok(invoke) => invoke,
                Err(why) => {
                    tracing::debug!(%why, "the change does not encode");
                    return;
                }
            };
            if let Err(error) = record.invoke(&invoke) {
                tracing::warn!(%error, "personal: {what} stays on this device");
            }
        };
        let writes = self.instance.take_view_writes();
        let seen = self.instance.take_seen();
        for (key, value) in writes {
            let keep = match Keep::view(&key, &value) {
                Ok(keep) => keep,
                Err(why) => {
                    tracing::debug!(%why, "the slot is no person's state");
                    continue;
                }
            };
            keep_it(&keep, "a view's state");
        }
        for write in seen {
            // Someone else's mark is not this person's to keep.
            let keep = match write {
                tilolang_tilo::runtime::seen::SeenWrite::Resource {
                    person,
                    resource,
                    mark,
                } if person.eq_ignore_ascii_case(&self.me) => Keep::Seen {
                    person: self.me.clone(),
                    resource,
                    mark,
                },
                tilolang_tilo::runtime::seen::SeenWrite::Run { person, run }
                    if person.eq_ignore_ascii_case(&self.me) =>
                {
                    let mut runs: Vec<String> = record
                        .get()
                        .of(&self.me)
                        .runs
                        .into_iter()
                        .map(|reviewed| reviewed.run)
                        .collect();
                    if !runs.contains(&run) {
                        runs.push(run);
                        runs.sort();
                    }
                    Keep::Runs {
                        person: self.me.clone(),
                        runs,
                    }
                }
                _ => continue,
            };
            keep_it(&keep, "what the person saw");
        }
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

    /// Run the action the app declared the plain key `key` for in this window, as pressing it
    /// does: the key reaches the app through the control that runs the action. Whether the
    /// window offers it. A test's way in, and the person's key.
    pub fn run_shortcut(&mut self, key: &str) -> bool {
        let ran = self.view.run_shortcut(key);
        self.show_sent();
        ran
    }

    /// Ask the app to undo the person's last step, as the window's Undo does: the ask reaches
    /// the instance on its next update, and the record carries it out on the person's history
    /// verb.
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

    /// Whether the window's composer holds a draft in the resource it shows: a conversation's
    /// "… is typing". The draft itself stays here; only that it is under way travels.
    fn composing_payload(
        &self,
    ) -> Option<(
        ui::presence::PresenceKey,
        ui::collaboration::PresencePayload,
    )> {
        let target = self.instance.composing_in()?;
        Some((
            presence_key(&self.app, target),
            ui::collaboration::PresencePayload::Composing { composing: true },
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
        if let Some(taken) = taken {
            return Some(taken);
        }
        // What the views' controls invoked themselves goes before the record's history: the
        // history waits until the change asked for before it is committed, as in a room.
        if let Some(asked) =
            ui::widget::controls::take_attributed_control_invocation(&mut self.view)
        {
            return Some(asked);
        }
        // What a view asked of the record's history goes as any action the window runs does:
        // through the door, which builds it from the published verb and applies it.
        self.history.pop_front().map(|asked| (asked, None))
    }

    fn on_control_invocation_result(&mut self, address: &ActionAddress, result: &ControlResult) {
        let error = result.as_ref().err().map(ToString::to_string);
        if !self.answered(address, error) {
            ui::widget::controls::on_control_invocation_result(&mut self.view, address, result);
        }
    }
}

/// The invocation of the history verb `asked` runs, on the record's resource as this window
/// publishes it: the same vocabulary (`collaboration::history_source::verbs`) the room path
/// answers, so what it takes back is one implementation, whether the instance runs here or in a
/// tilolang host. `None` for an address the platform refuses.
fn history_invocation(app: &str, asked: &instance::HistoryAsk) -> Option<ControlInvocation> {
    let (verb, arguments) = match asked {
        instance::HistoryAsk::Undo => ("undo", serde_json::Map::new()),
        instance::HistoryAsk::Redo => ("redo", serde_json::Map::new()),
        instance::HistoryAsk::UndoStep(change) => (
            "undo",
            serde_json::Map::from_iter([("step".to_owned(), change.clone().into())]),
        ),
        instance::HistoryAsk::UndoRun(run) => (
            "undo",
            serde_json::Map::from_iter([("run".to_owned(), run.clone().into())]),
        ),
    };
    ControlInvocation::from_parts(app, verb, arguments)
}

/// The in-process window's undo and redo are the record's history verbs, exactly as the room
/// path runs them: what a view asks (`undo(v)`, `undo(run)`) and what the person's Undo asks are
/// both applied to the record as a step of their own, through the one verb
/// (`collaboration::history_source`) the platform publishes on the app's resource.
impl ui::undo::UndoControl for Window {
    fn set_undo_status(&mut self, status: &mut dyn FnMut() -> ui::undo::UndoStatus) {
        self.undo.publish(&status());
    }

    /// An Undo a control asked for goes to the record as the person's own step, on the record's
    /// history verb; what it did comes back in the views.
    fn take_undo_request(&mut self) -> Option<ui::undo::UndoRequest> {
        let request = self.undo.take_request()?;
        let asked = match request {
            ui::undo::UndoRequest::Undo => instance::HistoryAsk::Undo,
            ui::undo::UndoRequest::Redo => instance::HistoryAsk::Redo,
        };
        self.history.extend(history_invocation(&self.app, &asked));
        None
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

/// The window is named by what it shows: the item it shows as its page, else the page, the record,
/// the place its composed shell names in its content header. Two windows of the app read apart in
/// the desktop's tabs, the switcher and assistive technology without either being opened.
impl ui::component_lifecycle::WindowTitle for Window {
    fn window_title(&self) -> Option<&str> {
        self.view.title()
    }
}

impl Widget for Window {
    ui::provides!(
        dyn ui::widget::controls::ActionControls,
        dyn ui::undo::UndoControl,
        dyn ui::presence::capability::PublishesPresence,
        dyn ui::component_lifecycle::WindowTitle
    );

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }

    /// Whether this window's sync still has work to do on an idle frame: what its instance owes
    /// — an index the store is deriving or waits for the record of, a change no sync has sent —
    /// changes what its views show, so the frame that says so syncs and the change is shown then
    /// (see [`tilolang_tilo::instance::Instance::owes_a_frame`]).
    fn needs_idle_sync(&self) -> bool {
        self.instance.owes_a_frame() || ui::widget::Widget::needs_idle_sync(&self.view)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "TilolangWindow"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.view]
    }

    fn children_mut(&mut self) -> Vec<&mut dyn Widget> {
        vec![&mut self.view]
    }

    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.dirty = true;
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
        let personal = self.personal.clone();
        if let Some(record) = &personal {
            self.bring_personal(record);
        }
        let changed = {
            let _handled = Phase::start("tl.events");
            self.instance.update()
        };
        let received = self.view.receive();
        // A control whose event runs an action the window publishes is bound to it: a press
        // runs it through the door, and an agent running it is seen there.
        if let Some((bound, published)) = crate::resource::published_since(self.bound) {
            self.bound = bound;
            self.view.bind_published(&published);
        }
        // What a view asked of the record's history (`undo(v)`, `undo(run)`) and the person's
        // Undo and Redo are the record's own history verbs, as the room path runs them: the
        // door builds each from the published verb and applies it to the record.
        for asked in self.instance.take_history() {
            if let Some(invocation) = history_invocation(&self.app, &asked) {
                self.history.push_back(invocation);
            } else {
                tracing::warn!(?asked, "a history verb the window cannot address");
            }
        }
        let child = self.view.update(state, path);
        if let Some(record) = &personal {
            self.keep_personal(record);
        }
        changed || received || child
    }

    /// The body's deadlines, when the time a view read (`today`) next reads differently, and the
    /// pause after the last keystroke that lands the change the person is making: the window
    /// sleeps until then, and the frame then runs what it owed.
    fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        let views = self
            .instance
            .wakes_in()
            .and_then(|wait| ui::time::now().checked_add(wait));
        [views, self.view.next_wake_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.dirty = true;
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
        // A conversation's "… is typing": who presence says is composing a message in the
        // resource this window shows, by their person.
        let composing: Vec<String> = match self.instance.detail_target() {
            Some(target) => presence
                .composing_at(&presence_key(&self.app, target), ui::time::now())
                .into_iter()
                .map(|user| user.0)
                .collect(),
            None => Vec::new(),
        };
        self.instance
            .set_peers_and_composers(peers_of(presence, &self.app, &self.me), &composing);
        // The workspace's policy names people; presence pairs each participant with theirs.
        self.instance
            .set_persons(presence.roster().into_iter().filter_map(|user| {
                let person = presence.person_of(&ui::permissions::UserId(user.id.clone()))?;
                Some((user.id, person.0.clone()))
            }));
        self.view
            .draw_child(scene, transform, text, theme, presence);
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
