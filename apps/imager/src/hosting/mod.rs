//! What runs the installer's windows before Tilo exists.
//!
//! Each window is a widget with a typed vocabulary of effects ([`Window`]). Its controls press
//! effects through plain closures ([`Presses`]); a caller outside the window — on Unix, the
//! action frames `console-control` serves, and the tests — invokes the same effects and the
//! [`Store`]'s own actions on the one resource the windows publish. Work a window starts off its
//! thread runs on a worker that wakes the window when it reports or ends, and the effect that
//! started it answers its caller then.

#[cfg(unix)]
mod console;
mod effect;
mod run;
mod store;
#[cfg(test)]
pub(crate) mod testing;

use std::cell::{Cell, RefCell};
use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use serde_json::Value;
use tilo_resource::{Effect, EffectStatus, LiveRow};
use ui::tilo_actions::{EncodedAction, ResourceSpec};
use ui::widget::Widget;

pub(crate) use effect::{EffectAnswer, EffectCx, Effected, JobProgress, Progress, Reply};
pub(crate) use run::{Flow, Opened, run};
#[cfg(test)]
pub(crate) use run::{SHOT_VIEWPORT, scenario_store, scenarios, window};
pub(crate) use store::Store;

use crate::resource::Imager;

/// A window the host runs: a widget, the effects it answers, and what it publishes of itself.
pub(crate) trait Window: Widget + Sized + 'static {
    type Effect: Effect;

    /// The presses, workers and resource the window shares with its host.
    fn port(&self) -> &Port<Self>;

    /// Answer `effect`: at once, or later through a worker started on `cx`.
    fn effect(&mut self, effect: Self::Effect, cx: &mut EffectCx<'_, Self>) -> EffectAnswer;

    /// The rows the window publishes beneath the resource: its drives, its job.
    fn rows(&self) -> Vec<LiveRow> {
        Vec::new()
    }

    /// How the window's effects stand now, where that differs from their declarations.
    fn statuses(&self) -> Vec<EffectStatus> {
        Vec::new()
    }

    /// How each published row's effects stand now, by the row's id.
    fn row_statuses(&self) -> Vec<(String, Vec<EffectStatus>)> {
        Vec::new()
    }

    /// Show the resource as it is now: it changed since the window last showed it.
    fn present(&mut self, _state: &Imager) {}

    /// Say why an effect the person pressed did not run.
    fn refused(&mut self, reason: &str) {
        ui::control_notice::say_in_window(reason);
    }

    /// What the window shows, as data: what a console's `state_dump` answers.
    fn state(&self) -> Value {
        Value::Null
    }
}

/// What a control queues: an effect for its window to run once the event that pressed it is
/// handled.
pub(crate) struct Presses<E>(Rc<RefCell<Vec<E>>>);

impl<E> Clone for Presses<E> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<E> Default for Presses<E> {
    fn default() -> Self {
        Self(Rc::default())
    }
}

impl<E> Presses<E> {
    pub(crate) fn press(&self, effect: E) {
        self.0.borrow_mut().push(effect);
    }

    /// A closure for a control's press that queues `effect`.
    pub(crate) fn on(&self, effect: E) -> impl Fn() + use<E>
    where
        E: Clone + 'static,
    {
        let presses = self.clone();
        move || presses.press(effect.clone())
    }

    fn take(&self) -> Vec<E> {
        std::mem::take(&mut *self.0.borrow_mut())
    }
}

type Finish<W> = Box<dyn FnOnce(&mut W, &mut EffectCx<'_, W>) -> EffectAnswer + Send>;

struct Finished<W: Window> {
    ticket: u64,
    job: Option<&'static str>,
    finish: Finish<W>,
}

struct Shared<W: Window> {
    store: Store,
    wake: ui::WakeHandle,
    presses: Presses<W::Effect>,
    finished: Arc<Mutex<Vec<Finished<W>>>>,
    waiting: RefCell<BTreeMap<u64, Reply>>,
    jobs: RefCell<BTreeMap<&'static str, Progress>>,
    tickets: Cell<u64>,
    shown: Cell<Option<u64>>,
}

/// What a window shares with its host: the presses its controls queue, the workers it started
/// and the callers they answer, and the resource.
pub(crate) struct Port<W: Window>(Rc<Shared<W>>);

impl<W: Window> Clone for Port<W> {
    fn clone(&self) -> Self {
        Self(Rc::clone(&self.0))
    }
}

impl<W: Window> Port<W> {
    /// A port on `store`, whose workers wake the window through `wake`.
    pub(crate) fn new(store: Store, wake: ui::WakeHandle) -> Self {
        Self(Rc::new(Shared {
            store,
            wake,
            presses: Presses::default(),
            finished: Arc::default(),
            waiting: RefCell::default(),
            jobs: RefCell::default(),
            tickets: Cell::new(0),
            shown: Cell::new(None),
        }))
    }

    /// A port for a window this one shows inside itself: the same resource and wake.
    pub(crate) fn child<C: Window>(&self) -> Port<C> {
        Port::new(self.0.store.clone(), self.0.wake.clone())
    }

    /// A port for a window nobody runs: a shot's, a test's.
    pub(crate) fn detached() -> Self {
        Self::new(Store::in_memory(), ui::WakeHandle::new())
    }

    pub(crate) fn store(&self) -> &Store {
        &self.0.store
    }

    pub(crate) fn presses(&self) -> Presses<W::Effect> {
        self.0.presses.clone()
    }

    pub(crate) fn wake(&self) -> ui::WakeHandle {
        self.0.wake.clone()
    }

    /// Start `name`'s work on a worker; `then` runs on the window when it ends. While it runs,
    /// [`Self::job`] reads what it last reported.
    pub(crate) fn start_job<T: Send + 'static>(
        &self,
        name: &'static str,
        work: impl FnOnce(&Progress) -> T + Send + 'static,
        then: impl FnOnce(&mut W, T) + Send + 'static,
    ) {
        let progress = Progress::new(self.0.wake.clone());
        self.0.jobs.borrow_mut().insert(name, progress.clone());
        self.spawn(Some(name), progress, work, move |window, result, _| {
            then(window, result);
            Ok(Effected::done())
        });
    }

    /// What the job `name` last reported, while it runs.
    pub(crate) fn job(&self, name: &str) -> Option<JobProgress> {
        self.0.jobs.borrow().get(name).map(Progress::read)
    }

    fn ticket(&self) -> u64 {
        let ticket = self.0.tickets.get().wrapping_add(1);
        self.0.tickets.set(ticket);
        ticket
    }

    fn spawn<T: Send + 'static>(
        &self,
        job: Option<&'static str>,
        progress: Progress,
        work: impl FnOnce(&Progress) -> T + Send + 'static,
        then: impl FnOnce(&mut W, T, &mut EffectCx<'_, W>) -> EffectAnswer + Send + 'static,
    ) -> u64 {
        let ticket = self.ticket();
        let finished = Arc::clone(&self.0.finished);
        let wake = self.0.wake.clone();
        std::thread::spawn(move || {
            let result = work(&progress);
            finished
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .push(Finished {
                    ticket,
                    job,
                    finish: Box::new(move |window, cx| then(window, result, cx)),
                });
            wake.wake();
        });
        ticket
    }

    fn take_finished(&self) -> Vec<Finished<W>> {
        std::mem::take(
            &mut *self
                .0
                .finished
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner),
        )
    }
}

/// Run `effect` on `window`, answering `reply` (none for a control's press).
fn run_effect<W: Window>(window: &mut W, effect: W::Effect, reply: Option<Reply>) {
    let port = window.port().clone();
    let mut cx = EffectCx::new(&port, reply);
    let answer = window.effect(effect, &mut cx);
    settle(window, cx, answer);
}

fn settle<W: Window>(window: &mut W, mut cx: EffectCx<'_, W>, answer: EffectAnswer) {
    match (answer, cx.take_reply()) {
        (Ok(Effected::Pending), _) | (Ok(Effected::Done(_)), None) => {}
        (Ok(Effected::Done(output)), Some(reply)) => reply.finish(Ok(output)),
        (Err(reason), Some(reply)) => reply.finish(Err(reason)),
        (Err(reason), None) => window.refused(&reason),
    }
}

/// Take up what `window`'s controls pressed, what its workers finished and how the resource
/// changed. Whether anything was.
pub(crate) fn serve<W: Window>(window: &mut W) -> bool {
    let port = window.port().clone();
    let mut served = false;
    loop {
        let presses = port.0.presses.take();
        let finished = port.take_finished();
        if presses.is_empty() && finished.is_empty() {
            break;
        }
        served = true;
        for effect in presses {
            run_effect(window, effect, None);
        }
        for Finished {
            ticket,
            job,
            finish,
        } in finished
        {
            if let Some(job) = job {
                port.0.jobs.borrow_mut().remove(job);
            }
            let reply = port.0.waiting.borrow_mut().remove(&ticket);
            let mut cx = EffectCx::new(&port, reply);
            let answer = finish(window, &mut cx);
            settle(window, cx, answer);
        }
    }
    let revision = port.0.store.revision();
    if port.0.shown.get() != Some(revision) {
        port.0.shown.set(Some(revision));
        let state = port.0.store.state();
        window.present(&state);
        served = true;
    }
    served
}

/// A window as the console and the tests reach it, wherever it stands in the tree.
pub(crate) trait Served {
    /// The operation its effects travel under.
    fn operation(&self) -> &'static str;
    /// Add its effects, as they stand, and its rows to `resource`.
    fn publish(&self, resource: &mut ResourceSpec);
    /// Run the effect `encoded` names, answering `reply`.
    fn invoke(&mut self, encoded: &EncodedAction, reply: Reply);
    fn state(&self) -> Value;
    /// The window itself, for a test to look at.
    #[cfg(test)]
    fn window_mut(&mut self) -> &mut dyn std::any::Any;
}

/// `W` as the console and the tests reach it.
pub(crate) struct Hosted<W: Window>(pub(crate) W);

impl<W: Window> Served for Hosted<W> {
    fn operation(&self) -> &'static str {
        W::Effect::OPERATION
    }

    fn publish(&self, resource: &mut ResourceSpec) {
        let window = &self.0;
        let mut actions: Vec<_> = W::Effect::declarations()
            .iter()
            .filter(|declaration| !declaration.row)
            .filter_map(|declaration| W::Effect::spec(declaration.id))
            .collect();
        let statuses = window.statuses();
        for spec in &mut actions {
            for status in &statuses {
                if status.item().is_none() && status.id() == spec.id {
                    status.overlay(spec);
                }
            }
        }
        resource.actions.extend(actions);
        let mut rows: Vec<ResourceSpec> = window
            .rows()
            .into_iter()
            .map(LiveRow::into_resource)
            .collect();
        let row_statuses = window.row_statuses();
        tilo_resource::status::apply_row_statuses(&mut rows, &|id| {
            row_statuses
                .iter()
                .find(|(row, _)| row == id)
                .map(|(_, statuses)| statuses.clone())
                .unwrap_or_default()
        });
        resource.children.extend(rows);
    }

    fn invoke(&mut self, encoded: &EncodedAction, reply: Reply) {
        match W::Effect::from_payload(&encoded.payload) {
            Ok(effect) => {
                run_effect(&mut self.0, effect, Some(reply));
                serve(&mut self.0);
            }
            Err(error) => reply.finish(Err(error.to_string())),
        }
    }

    fn state(&self) -> Value {
        self.0.state()
    }

    #[cfg(test)]
    fn window_mut(&mut self) -> &mut dyn std::any::Any {
        &mut self.0
    }
}

/// Visit every window in `root`'s tree that answers effects, outermost first, until `visit`
/// answers `true`. Whether it did.
pub(crate) fn each_served(
    root: &mut dyn Widget,
    visit: &mut dyn FnMut(&mut dyn Served) -> bool,
) -> bool {
    if let Some(served) = ui::widget::capability::capability_mut::<dyn Served>(root)
        && visit(served)
    {
        return true;
    }
    root.children_mut()
        .into_iter()
        .any(|child| each_served(child, visit))
}

/// The resource `root`'s windows publish together: the store's, with every window's effects and
/// rows.
pub(crate) fn published(store: &Store, root: &mut dyn Widget) -> ResourceSpec {
    let mut resource = store.published();
    each_served(root, &mut |window| {
        window.publish(&mut resource);
        false
    });
    resource
}

/// What the outermost window shows, as data.
pub(crate) fn state(root: &mut dyn Widget) -> Value {
    let mut state = Value::Null;
    each_served(root, &mut |window| {
        state = window.state();
        true
    });
    state
}

/// Run `encoded`, an admitted invocation of the resource's own action or one window's effect,
/// answering `reply`.
pub(crate) fn invoke(store: &Store, root: &mut dyn Widget, encoded: &EncodedAction, reply: Reply) {
    if encoded.type_id == store.action_type() {
        reply.finish(store.apply_encoded(encoded));
        return;
    }
    let mut reply = Some(reply);
    each_served(root, &mut |window| {
        if window.operation() != encoded.type_id {
            return false;
        }
        if let Some(reply) = reply.take() {
            window.invoke(encoded, reply);
        }
        true
    });
    if let Some(reply) = reply {
        reply.finish(Err(format!(
            "no window here offers `{}` now",
            encoded.type_id
        )));
    }
}

/// Ctrl+Z and Ctrl+Shift+Z undo and redo the person's steps on the resource.
impl<W: Window> ui::undo::LocalUndo for Hosted<W> {
    fn local_undo_status(&self) -> Option<ui::undo::LocalUndoStatus> {
        let (can_undo, can_redo) = self.0.port().store().undo_status();
        Some(ui::undo::LocalUndoStatus::Ready(ui::undo::UndoStatus {
            can_undo,
            can_redo,
            undo_summary: None,
            redo_summary: None,
        }))
    }

    fn handle_local_undo_request(&mut self, request: ui::undo::UndoRequest) -> bool {
        let store = self.0.port().store().clone();
        let done = match request {
            ui::undo::UndoRequest::Undo => store.undo(),
            ui::undo::UndoRequest::Redo => store.redo(),
        };
        if done {
            serve(&mut self.0);
        }
        done
    }
}

impl<W: Window> ui::widget::Decorator for Hosted<W> {
    type Inner = W;

    fn inner(&self) -> &W {
        &self.0
    }

    fn inner_mut(&mut self) -> &mut W {
        &mut self.0
    }

    ui::provides!(dyn Served, dyn ui::undo::LocalUndo; decorated);

    fn sync(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) {
        serve(&mut self.0);
        self.0.sync(state, path);
    }

    fn update(&mut self, state: &ui::widget::AppModel, path: &ui::widget::ModelPath) -> bool {
        let served = serve(&mut self.0);
        self.0.update(state, path) || served
    }

    fn handle_event(
        &mut self,
        event: &winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        self.0.handle_event(event, ctx);
        serve(&mut self.0);
    }

    fn settle(&mut self, edits: &mut ui::widget::Edits<'_>, path: &automerge::ObjId) {
        self.0.settle(edits, path);
        serve(&mut self.0);
    }
}
