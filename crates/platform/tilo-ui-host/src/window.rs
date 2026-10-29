//! One app window as the UI host holds it.
//!
//! The window keeps a copy of each of its app's two views (the root, and the view of the
//! resource `open(r)` or a row went to), as the app sends them over the patch protocol, and
//! shows them as the columns of one shell ([`crate::shell`]): the root's sidebar and overview
//! beside the content and header of the view after it. On a narrow screen the columns become
//! push navigation with Back, by the shell's rules. Everything in the shell is the views' own,
//! built through the one adapter: the window adds no frame, header or list of its own.
//!
//! What holds for every window is the platform's, and so the window's: the overview's
//! selection is the view shown after it; a notice of a destructive action offers Undo; the Ask
//! bar is trusted chrome the app cannot draw.
//!
//! What the person does is sent back as it happens, at most one batch a frame: a component's
//! event by its key, an edit through a binding as a splice of what the control showed (the
//! toolkit's field shows the typing at once), the rows a list shows now.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use tilo_render_protocol::patch::validate::{PatchLimits, Validator};
use tilo_render_protocol::patch::{self as wire, Answer, Lane, ToApp, ToUi, Transport, Words};
use ui::tokens::{MotionSource, Rhythm, TextRole};
use ui::widget::arrange::{Fit, Row, Slot, Stack};
use ui::widget::toast::ToastKind;
use ui::widget::{
    Absent, Button, ButtonRole, DescribedList, Element, NavPane, NavShell, Text, Widget,
};

use crate::i18n::{t, t_args};
use crate::pane::Pane;
use crate::reconcile::{EventSink, Reconciler, UiEvent};
use crate::rows::{BoundValues, RemoteRows};
use crate::shell::DETAIL;
use crate::{ResourceRef, UiNode, Value};

/// Every virtual list of the window's views, by the key of its node in the window's tree.
pub type Registry = Rc<RefCell<HashMap<String, Rc<RefCell<RemoteRows>>>>>;

/// One of the window's two views, as the app last sent it.
struct View {
    /// The resource it shows.
    target: ResourceRef,
    /// Its tree, without the rows of its virtual lists.
    tree: UiNode,
}

/// What the person typed after a press the app has not run yet: held, not sent, until it did.
/// It goes where the keyboard would have been had the press run at once: typed where the
/// keyboard was when they pressed, or with no field focused, it follows the press into the
/// field of what it opened; typed into a field they put the keyboard in since, it stays that
/// field's, of the resource it showed.
struct Typed {
    /// How many presses the window had sent when the person typed it: it waits until the app
    /// ran them.
    after: u64,
    /// The field, by key in the composed tree: none while no field had the keyboard (the press
    /// took it).
    key: Option<String>,
    /// The resource the field's view showed when the person typed into it: none while no field
    /// had the keyboard.
    target: Option<ResourceRef>,
    /// The field had the keyboard since before the press: the person typed after the press
    /// where the keyboard was, not where they put it since.
    follows: bool,
    /// It followed the press into the field the press opened: it goes at that field's end,
    /// after whatever went there before it.
    moved: bool,
    /// What it showed when the person started typing.
    base: String,
    /// What it shows now, with their typing.
    now: String,
    /// What the app sent for the field since (it showed something else meanwhile, such as the
    /// note the press opened).
    sent: Option<String>,
}

fn slot(element: Element, fit: Fit) -> Slot {
    let mut slot = Slot::new(element);
    slot.fit = fit;
    slot
}

/// What moves a column the person's last input brings to the front: a click or a tap slides it
/// in; a key, an agent's action, or nothing the person just did lands it.
fn by_input() -> MotionSource {
    ui::motion::recent_input(ui::time::now()).unwrap_or(MotionSource::Action)
}

/// See the module docs.
pub struct AppWindow {
    transport: Box<dyn Transport>,
    limits: PatchLimits,
    root: Option<View>,
    detail: Option<View>,
    /// Builds the composed shell and reconciles it, reporting what its controls do.
    reconciler: Rc<RefCell<Reconciler>>,
    events: EventSink,
    /// The composed shell last built.
    built: Option<UiNode>,
    /// The rows of the views' virtual lists.
    rows: Registry,
    /// What each bound control shows, as the app last sent it, by key in the composed tree.
    bound: BoundValues,
    /// The text of each bound text control as the app has it, splices sent included: what the
    /// next splice is against.
    texts: HashMap<String, String>,
    /// The last splice of each bound control sent, by key in the composed tree.
    seqs: HashMap<String, u64>,
    /// The shell, the Ask bar and whatever the embedder shows beside them.
    body: Element,
    answers: Rc<RefCell<Vec<Answer>>>,
    /// The Ask bar shown, if one is.
    ask: Option<wire::Ask>,
    /// The form shown, if one is: an action waiting for its arguments.
    form: Option<crate::form::Shown>,
    out: Vec<ToApp>,
    structure_dirty: bool,
    /// Why the window shows the platform's error state: a batch it refused.
    failed: Option<String>,
    /// A key, a press or a touch arrived since the window last looked where the keyboard focus
    /// is.
    input: bool,
    /// The bound control the keyboard focus was in, as last sent, by key in the composed tree.
    focus: Option<String>,
    /// Whether an input method composes in the focused control, as last sent.
    composing: bool,
    /// How often the app asked for the window to come forward since the window last did.
    activations: u32,
    /// How many presses the window sent the app (`ToApp::Fire`), and how many of them the app
    /// said it ran (`ToUi::Took`).
    fired: u64,
    took: u64,
    /// What the person typed after presses the app has not run yet, oldest first.
    typed: std::collections::VecDeque<Typed>,
    /// How many presses the window had sent when the person last pointed (a click, a touch):
    /// what they type after a press they pointed after was typed where they put the keyboard,
    /// not where the keyboard was at the press.
    pointed_after: Option<u64>,
    /// The control to take the keyboard once the window's tree is laid out, by key in the
    /// composed tree ([`Widget::take_focus_request`]).
    focus_request: Option<String>,
    /// How many presses the window had sent when the app last opened a view for the person's
    /// input, until the app says it ran them (`ToUi::Took`).
    opened: Option<u64>,
    /// One `rem` in logical pixels under the theme the window was last laid out in.
    rem: f32,
    /// The space last told the app each lane's view has, in rem.
    root_space: Option<(f32, f32)>,
    detail_space: Option<(f32, f32)>,
}

impl AppWindow {
    /// A window receiving its app's views over `transport`, showing `beside` after its shell
    /// and Ask bar (an embedder's endpoints, which take no room).
    #[must_use]
    pub fn new(transport: Box<dyn Transport>, beside: Vec<Element>) -> Self {
        let mut body = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::Tight)
            .push_slot(slot(
                Box::new(Pane::new(Box::new(NavShell::new()))),
                Fit::fill(1.0),
            ))
            .push_slot(slot(Box::new(Absent::new()), Fit::hug()))
            // The form of an action waiting for its arguments: out of flow, over the window.
            .push_slot(slot(Box::new(Absent::new()), Fit::hug()));
        for element in beside {
            body = body.push_slot(slot(element, Fit::hug()));
        }
        let events: EventSink = Rc::default();
        let rows: Registry = Rc::default();
        let reconciler = {
            // Weakly: the rows' own readers share this reconciler, and a strong hold would keep
            // the rows alive after the window closes.
            let registry = Rc::downgrade(&rows);
            let rows_of: crate::adapter::RowsOf = Rc::new(move |key: &str| {
                registry
                    .upgrade()?
                    .borrow()
                    .get(key)
                    .map(|rows| Rc::clone(rows) as ui::component::SharedRows)
            });
            Reconciler::new(Rc::clone(&events)).with_rows(rows_of)
        };
        Self {
            transport,
            limits: PatchLimits::default(),
            root: None,
            detail: None,
            reconciler: Rc::new(RefCell::new(reconciler)),
            events,
            built: None,
            rows,
            bound: Rc::default(),
            texts: HashMap::new(),
            seqs: HashMap::new(),
            body: Box::new(body),
            answers: Rc::default(),
            ask: None,
            form: None,
            out: Vec::new(),
            structure_dirty: true,
            failed: None,
            input: false,
            focus: None,
            composing: false,
            activations: 0,
            fired: 0,
            took: 0,
            typed: std::collections::VecDeque::new(),
            pointed_after: None,
            focus_request: None,
            opened: None,
            rem: 16.0,
            root_space: None,
            detail_space: None,
        }
    }

    /// Whether the app asked for the window to come forward since this was last asked
    /// ([`ToUi::Activate`]).
    pub fn take_activation(&mut self) -> bool {
        std::mem::take(&mut self.activations) > 0
    }

    // ----- what the app sends --------------------------------------------------------------

    /// Take what the app sent and show it. Whether anything arrived.
    pub fn receive(&mut self) -> bool {
        let mut any = false;
        loop {
            let batch = match self.transport.recv() {
                Ok(Some(batch)) => batch,
                Ok(None) => break,
                Err(error) => {
                    self.fail(&error.to_string());
                    break;
                }
            };
            any = true;
            let messages: Vec<ToUi> = match wire::decode_batch(&batch) {
                Ok(messages) => messages,
                Err(error) => {
                    self.fail(&error.to_string());
                    continue;
                }
            };
            let validator = Validator::new(crate::adapter::catalogue(), self.limits);
            if let Err(error) = validator.to_ui(&messages) {
                self.fail(&error.to_string());
                continue;
            }
            if let Err(error) = self.apply(messages) {
                self.fail(&error);
            }
        }
        any
    }

    /// Refuse what the app sent: the window shows the platform's error state, and takes
    /// nothing more until the app opens its root anew.
    fn fail(&mut self, why: &str) {
        tracing::warn!(why, "a window refused what its app sent");
        if self.failed.is_none() {
            let state = ui::widget::EmptyState::new(t("tilolang-open-failed"));
            if let Some(pane) = self.shell_pane() {
                pane.set(Box::new(state));
            }
            self.built = None;
            self.structure_dirty = true;
        }
        self.failed = Some(why.to_owned());
    }

    /// Whether the window, showing the error state, leaves `message` aside: it takes nothing
    /// until the app opens its root anew.
    fn refuses_while_failed(&self, message: &ToUi) -> bool {
        self.failed.is_some()
            && !matches!(
                message,
                ToUi::Open {
                    lane: Lane::Root,
                    ..
                }
            )
    }

    fn apply(&mut self, messages: Vec<ToUi>) -> Result<(), String> {
        let mut content = false;
        let mut front = None;
        // A view a person's press opened: its first field takes the keyboard.
        let mut opened_for_input = false;
        for message in messages {
            if self.refuses_while_failed(&message) {
                continue;
            }
            match message {
                ToUi::Loading => self.show_loading(),
                ToUi::Activate => self.activations = self.activations.saturating_add(1),
                ToUi::Open {
                    lane,
                    target,
                    tree,
                    front: brought,
                } => {
                    if lane == Lane::Root {
                        self.failed = None;
                    }
                    self.forget_lane(lane);
                    let view = View { target, tree };
                    match lane {
                        Lane::Root => self.root = Some(view),
                        Lane::Detail => self.detail = Some(view),
                    }
                    self.track_rows();
                    content = true;
                    if lane == Lane::Detail && brought == Some(wire::Front::Input) {
                        opened_for_input = true;
                        // Its Took may come in a later batch: what is typed until then follows
                        // the press into it.
                        self.opened = Some(self.fired);
                    }
                    front = brought.or(front);
                }
                ToUi::Close => {
                    if self.detail.take().is_some() {
                        self.forget_lane(Lane::Detail);
                        content = true;
                    }
                }
                ToUi::Front(brought) => front = Some(brought),
                ToUi::Patch { lane, patches } => {
                    let view = match lane {
                        Lane::Root => self.root.as_mut(),
                        Lane::Detail => self.detail.as_mut(),
                    }
                    .ok_or("a patch to a view the window does not show")?;
                    for patch in patches {
                        patch
                            .apply(&mut view.tree)
                            .map_err(|error| error.to_string())?;
                    }
                    self.track_rows();
                    content = true;
                }
                ToUi::Bound {
                    lane,
                    key,
                    value,
                    acked,
                } => self.take_bound(lane, &key, value, acked),
                rows @ (ToUi::Rows { .. } | ToUi::RowsChanged { .. }) => self.take_rows(rows),
                ToUi::Notice { words, kind, undo } => self.notice(&words, kind, undo),
                ToUi::Ask(ask) => self.rebuild_ask(ask.as_ref()),
                ToUi::Form(form) => self.show_form(form.as_deref()),
                ToUi::Took { fired } => {
                    self.took = self.took.max(fired);
                    // What was typed after the presses it ran goes to what they opened, before
                    // what a later press opened (further on in this batch) takes its place.
                    opened_for_input = false;
                    let opened = self.opened.take().is_some();
                    self.send_typed_into(opened);
                }
            }
        }
        self.check_budget()?;
        if content {
            self.compose();
        }
        self.send_typed_into(opened_for_input);
        if self.fired <= self.took {
            self.opened = None;
        }
        if let Some(front) = front {
            self.show_content(match front {
                wire::Front::Input => by_input(),
                wire::Front::Action => MotionSource::Action,
            });
        }
        Ok(())
    }

    /// The window's trees within its budget: one window's nodes are its own to spend.
    /// Rows of a virtual list, or what changed of them, into the list that shows them.
    fn take_rows(&self, message: ToUi) {
        match message {
            ToUi::Rows {
                lane,
                list,
                len,
                start,
                keys,
                rows,
            } => {
                if let Some(held) = self.rows.borrow().get(&composed_key(lane, &list)) {
                    held.borrow_mut()
                        .rows(len as usize, start as usize, keys, rows);
                }
            }
            ToUi::RowsChanged {
                lane,
                list,
                len,
                changes,
            } => {
                if let Some(held) = self.rows.borrow().get(&composed_key(lane, &list)) {
                    held.borrow_mut().changed(len as usize, changes);
                }
            }
            _ => {}
        }
    }

    fn check_budget(&self) -> Result<(), String> {
        let rows: usize = self
            .rows
            .borrow()
            .values()
            .map(|rows| {
                let rows = rows.borrow();
                rows.rendered()
                    .iter()
                    .filter_map(|key| rows.row(key))
                    .map(UiNode::count)
                    .sum::<usize>()
            })
            .sum();
        let nodes = [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
            .map(|view| view.tree.count())
            .sum::<usize>()
            .saturating_add(rows);
        if nodes > self.limits.nodes as usize {
            return Err(format!(
                "the window holds {nodes} nodes, over its budget of {}",
                self.limits.nodes
            ));
        }
        Ok(())
    }

    fn show_loading(&mut self) {
        let loading =
            ui::widget::EmptyState::new(ui::i18n::I18nManager::lookup_plain("arriving-loading"))
                .busy();
        if let Some(pane) = self.shell_pane() {
            pane.set(Box::new(loading));
        }
        self.built = None;
        self.structure_dirty = true;
    }

    /// Forget what the window held of the view of `lane`: it opened anew, or closed.
    fn forget_lane(&mut self, lane: Lane) {
        let prefix = match lane {
            Lane::Root => None,
            Lane::Detail => Some(DETAIL),
        };
        let ours = |key: &String| match prefix {
            Some(prefix) => key.starts_with(prefix),
            None => !key.starts_with(DETAIL),
        };
        self.rows.borrow_mut().retain(|key, _| !ours(key));
        self.bound.borrow_mut().retain(|key, _| !ours(key));
        self.texts.retain(|key, _| !ours(key));
        self.seqs.retain(|key, _| !ours(key));
    }

    /// Keep rows for every virtual list of the views, and drop the rows of lists gone.
    fn track_rows(&self) {
        let mut wanted: Vec<(String, Lane, String, &'static str)> = Vec::new();
        for (view, lane, prefix) in [
            (self.root.as_ref(), Lane::Root, ""),
            (self.detail.as_ref(), Lane::Detail, DETAIL),
        ] {
            let Some(view) = view else { continue };
            for list in virtual_lists(&view.tree) {
                wanted.push((format!("{prefix}{list}"), lane, list, prefix));
            }
        }
        let mut registry = self.rows.borrow_mut();
        registry.retain(|key, rows| {
            wanted
                .iter()
                .any(|(want, lane, list, _)| want == key && rows.borrow().reads(*lane, list))
        });
        for (key, lane, list, prefix) in wanted {
            registry.entry(key).or_insert_with(|| {
                let reader = Rc::new(RefCell::new(Reconciler::sharing(&self.reconciler.borrow())));
                Rc::new(RefCell::new(RemoteRows::new(
                    lane,
                    &list,
                    prefix,
                    reader,
                    Rc::clone(&self.bound),
                )))
            });
        }
    }

    /// What the bound control `key` of `lane` shows now, as the app sends it: taken unless the
    /// person typed into it since what the app took (`acked`).
    fn take_bound(&mut self, lane: Lane, key: &str, value: Value, acked: u64) {
        let key = composed_key(lane, key);
        // The field keeps what the person typed into it while a press was running, while it
        // still shows the resource they typed into.
        let shows = self.target(lane);
        if let Some(typed) = self.typed.iter_mut().find(|typed| {
            typed.key.as_deref() == Some(key.as_str())
                && typed.target.is_some()
                && typed.target == shows
        }) {
            if let Value::Text(text) = &value {
                typed.sent = Some(text.clone());
            }
            return;
        }
        if self.seqs.get(&key).is_some_and(|sent| *sent > acked) {
            return;
        }
        if let Value::Text(text) = &value {
            self.texts.insert(key.clone(), text.clone());
        }
        self.bound.borrow_mut().insert(key, value);
        self.refresh_bound();
    }

    fn notice(&mut self, words: &Words, kind: wire::NoticeKind, undo: bool) {
        let text = say(words);
        let Some(shell) = self.shell() else {
            return;
        };
        match (kind, undo) {
            (_, true) => {
                shell.toast_with_action(text, ToastKind::Info, "undo-2", t("tilolang-undo"));
            }
            (wire::NoticeKind::Error, false) => shell.toast(text, ToastKind::Error),
            (wire::NoticeKind::Info, false) => shell.toast(text, ToastKind::Info),
        }
    }

    // ----- the shell -----------------------------------------------------------------------

    /// The pane the shell stands in.
    fn shell_pane(&mut self) -> Option<&mut Pane> {
        self.body
            .children_mut()
            .into_iter()
            .next()?
            .as_any_mut()?
            .downcast_mut::<Pane>()
    }

    fn shell(&mut self) -> Option<&mut NavShell> {
        self.shell_pane()?
            .content_mut()
            .as_any_mut()?
            .downcast_mut::<NavShell>()
    }

    fn show_content(&mut self, source: MotionSource) {
        if let Some(shell) = self.shell() {
            shell.show_from(NavPane::Content, source);
        }
        self.structure_dirty = true;
    }

    fn set_body_slot(&mut self, index: usize, element: Element, fit: Fit) {
        if let Some(arranged) = self.body.as_any_mut().and_then(|any| {
            any.downcast_mut::<ui::widget::arrange::Arranged<ui::widget::arrange::FlexLayout>>()
        }) {
            let mut slots = arranged.take_slots();
            if let Some(target) = slots.get_mut(index) {
                *target = slot(element, fit);
            }
            arranged.set_slots(slots);
        }
        self.structure_dirty = true;
    }

    /// Bring the shell up to date with the views: the root's shell with the view after it as
    /// its content, reconciled into the widgets it was built into.
    fn compose(&mut self) {
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let root_tree = crate::reconcile::normalize(&root.tree);
        let root_shell = crate::shell::as_shell(&crate::shell::without_rows(&root_tree));
        let detail_shell = self.detail.as_ref().map(|detail| {
            let tree = crate::reconcile::normalize(&detail.tree);
            crate::shell::prefixed(
                &crate::shell::as_shell(&crate::shell::without_rows(&tree)),
                DETAIL,
            )
        });
        let composed = crate::shell::compose(&root_shell, detail_shell.as_ref());
        let composed = crate::shell::header_rules(composed);
        let old = self.built.take();
        let reconciler = Rc::clone(&self.reconciler);
        if let Some(pane) = self.shell_pane() {
            let mut reconciler = reconciler.borrow_mut();
            match &old {
                Some(old) if old.key == composed.key && old.kind == composed.kind => {
                    if let Some(replaced) = reconciler.update_in(pane.content_mut(), old, &composed)
                    {
                        pane.set(replaced);
                    }
                }
                _ => pane.set(reconciler.build(&composed)),
            }
        }
        self.built = Some(composed);
        self.select_shown();
        self.refresh_bound();
    }

    /// Select, in each list of the overview, the row of the view shown after it: the app keeps
    /// no selection beside it.
    fn select_shown(&mut self) {
        let shown = self.detail.as_ref().map(|detail| detail.target.id_string());
        if let Some(shell) = self.shell()
            && let Some(list) = shell.overview_mut::<DescribedList>()
        {
            list.select_presenting(shown.as_deref());
        }
    }

    /// Show what the bound fields and view state hold now in the controls that edit them: the
    /// shell's and the rows its lists show.
    fn refresh_bound(&self) {
        if let Some(built) = &self.built {
            let reconciler = self.reconciler.borrow();
            show_bound(built, "", &self.bound.borrow(), &reconciler);
        }
        for rows in self.rows.borrow().values() {
            rows.borrow().refresh_bound();
        }
    }

    /// The Ask bar: trusted chrome naming who asks for what, with the person's choices.
    fn ask_bar(&self, ask: &wire::Ask) -> Element {
        let by = t_args(
            "tilolang-ask-by",
            &[("invoker", ask.invoker.as_str().into())],
        );
        let text = |id: &str, value: &str, role: TextRole| {
            slot(
                Box::new(Text::new(value).with_role(role).with_id(id)),
                Fit::hug(),
            )
        };
        let answer = |id: &str, role: ButtonRole, to: Answer| {
            let answers = Rc::clone(&self.answers);
            slot(
                Box::new(
                    Button::new(id, &t(id))
                        .role(role)
                        .on_view(move || answers.borrow_mut().push(to)),
                ),
                Fit::hug(),
            )
        };
        let choices = Row::row()
            .fill_width()
            .push_slot(answer(
                "tilolang-ask-confirm",
                ButtonRole::Primary,
                Answer::Allow,
            ))
            .push_slot(answer(
                "tilolang-ask-decline",
                ButtonRole::Secondary,
                Answer::Decline,
            ))
            .push_slot(answer(
                "tilolang-ask-always",
                ButtonRole::Plain,
                Answer::Always,
            ));
        Box::new(
            Stack::stack()
                .gap(Rhythm::Related)
                .with_id("tilolang-ask")
                .push_slot(text(
                    "tilolang-ask-title",
                    &t("tilolang-ask-title"),
                    TextRole::Heading,
                ))
                .push_slot(text("tilolang-ask-action", &ask.action, TextRole::Body))
                .push_slot(text("tilolang-ask-by", &by, TextRole::Caption))
                .push_slot(slot(Box::new(choices), Fit::hug())),
        )
    }

    fn rebuild_ask(&mut self, ask: Option<&wire::Ask>) {
        let bar: Element = match ask {
            Some(ask) => self.ask_bar(ask),
            None => Box::new(Absent::new()),
        };
        self.ask = ask.cloned();
        self.set_body_slot(1, bar, Fit::hug());
    }

    /// The body slot the form stands in.
    const FORM_SLOT: usize = 2;

    /// Show the form `form` of an action waiting for its arguments, or none. The form the
    /// window shows already, sent again as it reads now, is reconciled in place.
    fn show_form(&mut self, form: Option<&wire::Form>) {
        let Some(form) = form else {
            if self.form.take().is_some() {
                self.set_body_slot(Self::FORM_SLOT, Box::new(Absent::new()), Fit::hug());
            }
            return;
        };
        let reconciler = Rc::clone(&self.reconciler);
        if let Some(shown) = self.form.as_mut().filter(|shown| shown.id == form.id) {
            let sheet = self
                .body
                .children_mut()
                .into_iter()
                .nth(Self::FORM_SLOT)
                .and_then(crate::form::sheet_of);
            if let Some(sheet) = sheet {
                shown.update(form, sheet, &mut reconciler.borrow_mut());
                self.structure_dirty = true;
                return;
            }
        }
        let (shown, mut sheet) = crate::form::Shown::build(form, &mut reconciler.borrow_mut());
        // A form of a field or two stands beside the control that ran it, as a popover.
        if crate::form::fits_a_popover(form)
            && let Some((lane, key)) = &form.from
        {
            sheet.set_anchor(rect_of(self.body.as_ref(), &composed_key(*lane, key)));
        }
        self.form = Some(shown);
        self.set_body_slot(Self::FORM_SLOT, Box::new(sheet), Fit::hug());
    }

    /// What the person answered in the form's own controls since the last frame. Cancel closes
    /// it at once; what they typed stays the app's.
    fn take_form_answer(&mut self) {
        let Some(answer) = self.form.as_ref().and_then(crate::form::Shown::take_answer) else {
            return;
        };
        if matches!(answer, ToApp::FormCancel { .. }) {
            self.show_form(None);
        }
        self.out.push(answer);
    }

    // ----- what the person did -------------------------------------------------------------

    /// Send the app what the person did since the last frame: one batch.
    pub fn flush(&mut self) {
        for answer in std::mem::take(&mut *self.answers.borrow_mut()) {
            self.out.push(ToApp::Answer(answer));
        }
        self.take_events();
        self.take_form_answer();
        if self.shell().and_then(NavShell::take_toast_action).is_some() {
            self.out.push(ToApp::Undo);
        }
        if std::mem::take(&mut self.input) {
            self.follow_focus();
        }
        let wanted: Vec<(Lane, String, std::ops::Range<usize>)> = self
            .rows
            .borrow()
            .values()
            .filter_map(|rows| rows.borrow_mut().take_wanted())
            .collect();
        for (lane, list, range) in wanted {
            self.out.push(ToApp::Rows {
                lane,
                list,
                start: u32::try_from(range.start).unwrap_or(u32::MAX),
                end: u32::try_from(range.end).unwrap_or(u32::MAX),
            });
        }
        if self.out.is_empty() {
            return;
        }
        let messages = std::mem::take(&mut self.out);
        match wire::encode_batch(&messages) {
            Ok(batch) => {
                if let Err(error) = self.transport.send(batch) {
                    tracing::warn!(%error, "the app's connection failed");
                }
            }
            Err(error) => tracing::error!(%error, "a frame's batch did not encode"),
        }
    }

    /// What the person did with the window's controls since it was last taken, for the next
    /// batch.
    fn take_events(&mut self) {
        // One queue, in the order the person did things.
        for event in self.events.take() {
            self.event(event);
        }
    }

    /// Tell the app which bound control the keyboard focus is in now, when that changed: what
    /// its presence shows of the person (where they type).
    fn follow_focus(&mut self) {
        let now = self.focused_field();
        if now == self.focus {
            return;
        }
        self.out.push(ToApp::Focus {
            at: now.as_deref().map(lane_of),
        });
        self.focus = now;
    }

    fn event(&mut self, event: UiEvent) {
        if crate::form::in_form(&event) {
            if let Some(edit) = self.form.as_ref().and_then(|form| form.edit(&event)) {
                self.out.push(edit);
            }
            return;
        }
        if let UiEvent::Fire(key, name, _) = &event
            && *name == crate::shell::BACK
            && self.built.as_ref().is_some_and(|built| built.key == *key)
        {
            self.out.push(ToApp::Back);
            return;
        }
        if matches!(event, UiEvent::Press(_) | UiEvent::Fire(..)) {
            self.fired = self.fired.saturating_add(1);
        }
        let message = match event {
            UiEvent::Press(key) => {
                let (lane, key) = lane_of(&key);
                ToApp::Fire {
                    lane,
                    key,
                    event: "press".to_owned(),
                    value: None,
                }
            }
            UiEvent::Fire(key, event, value) => {
                let (lane, key) = lane_of(&key);
                ToApp::Fire {
                    lane,
                    key,
                    event,
                    value,
                }
            }
            UiEvent::Edit(composed, text) => match self.edit(&composed, text) {
                Some(message) => message,
                None => return,
            },
            UiEvent::EditRich(composed, rich) => {
                // What the field shows is what the person typed, until the app takes it.
                self.bound
                    .borrow_mut()
                    .insert(composed.clone(), Value::Rich(rich.clone()));
                let (lane, key) = lane_of(&composed);
                let Some(target) = self.target(lane) else {
                    return;
                };
                ToApp::EditRich {
                    lane,
                    target,
                    key,
                    rich,
                }
            }
            UiEvent::Toggle(key, checked) => {
                let (lane, key) = lane_of(&key);
                let Some(target) = self.target(lane) else {
                    return;
                };
                ToApp::Toggle {
                    lane,
                    target,
                    key,
                    checked,
                }
            }
            UiEvent::Resolve(key, approve) => {
                let (lane, key) = lane_of(&key);
                ToApp::Resolve { lane, key, approve }
            }
            UiEvent::Open(resource) => ToApp::Open { resource },
            UiEvent::OpenReference(reference) => ToApp::OpenReference { reference },
        };
        self.out.push(message);
    }

    /// The bound control `composed` now holds `text`: a splice of what it showed when it edits
    /// text, the new text when it edits another value (a number, a date).
    fn edit(&mut self, composed: &str, text: String) -> Option<ToApp> {
        let (lane, key) = lane_of(composed);
        let edits_text = match self.bound.borrow().get(composed) {
            Some(Value::Text(_)) | None => true,
            Some(Value::Opt(held)) => matches!(held.as_deref(), Some(Value::Text(_)) | None),
            Some(_) => false,
        };
        if !edits_text {
            let target = self.target(lane)?;
            return Some(ToApp::Edit {
                lane,
                target,
                key,
                text,
            });
        }
        let before = self.texts.get(composed).cloned().unwrap_or_default();
        // Typing after a press the app has not run yet waits for it: the press may open
        // something else into the field the person types into.
        if self.fired > self.took {
            let fired = self.fired;
            let target = self.target(lane);
            let same_field =
                |typed: &Typed| typed.key.as_deref() == Some(composed) && typed.target == target;
            // The person moved on to another field meanwhile: what they typed before stays
            // where they typed it.
            if self.typed.back().is_some_and(|typed| {
                typed.after == fired && typed.key.is_some() && !same_field(typed)
            }) && let Some(typed) = self.typed.pop_back()
            {
                self.send_typed_now(typed);
            }
            if !self
                .typed
                .back()
                .is_some_and(|typed| typed.after == fired && same_field(typed))
            {
                let follows = self.pointed_after != Some(self.fired);
                self.typed.push_back(Typed {
                    after: fired,
                    key: Some(composed.to_owned()),
                    target,
                    follows,
                    moved: false,
                    base: before,
                    now: String::new(),
                    sent: None,
                });
            }
            if let Some(typed) = self.typed.back_mut() {
                typed.now.clone_from(&text);
            }
            self.bound
                .borrow_mut()
                .insert(composed.to_owned(), Value::Text(text.clone()));
            self.texts.insert(composed.to_owned(), text);
            return None;
        }
        self.splice(composed, &before, text)
    }

    /// The view a person's press opened takes the keyboard in its first text field, and what
    /// they typed after the press, before it ran, goes there: as it would have, had the press
    /// run at once. What they typed before the press, or into a field they put the keyboard in
    /// since, stays that field's, of the resource it showed.
    fn focus_opened(&mut self) {
        fn first_field(node: &UiNode) -> Option<&UiNode> {
            if node.kind == crate::adapter::kind("TextInput") && node.binding.is_some() {
                return Some(node);
            }
            node.children.iter().find_map(first_field)
        }
        let Some(field) = self
            .detail
            .as_ref()
            .and_then(|detail| first_field(&detail.tree))
            .map(|field| composed_key(Lane::Detail, &field.key))
        else {
            return;
        };
        // What was typed with no field focused after the press the app just ran, not after a
        // later one.
        let took = self.took;
        let target = self.target(Lane::Detail);
        for typed in &mut self.typed {
            if typed.after <= took && typed.follows && !typed.moved {
                if typed.key.is_some() {
                    // Only what the person typed moves, never what the field held before it,
                    // and a line break a body's text ends with does not move into a title.
                    let (_, _, insert) = splice_between(&typed.base, &typed.now);
                    typed.base = String::new();
                    insert.trim_end_matches('\n').clone_into(&mut typed.now);
                }
                typed.moved = true;
                typed.key = Some(field.clone());
                typed.target.clone_from(&target);
            }
        }
        self.focus_request = Some(field);
    }

    /// Hold `event` when it types text while a press runs and no field has the keyboard (the
    /// press took it): it goes to the field the press leaves the keyboard in. Whether it was
    /// held.
    fn hold_typing(&mut self, event: &ui::winit::event::WindowEvent) -> bool {
        fn types_text(widget: &dyn Widget) -> bool {
            (widget.is_focused() && widget.ime_content().is_some())
                || widget.children().into_iter().any(types_text)
        }
        let ui::winit::event::WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        let Some(text) = key
            .text
            .as_deref()
            .filter(|text| !text.is_empty() && !text.chars().any(char::is_control))
        else {
            return false;
        };
        // A press the window's controls reported since the last event counts: the key may
        // follow it in the same frame.
        self.take_events();
        if self.fired <= self.took
            || key.state != ui::winit::event::ElementState::Pressed
            || types_text(self.body.as_ref())
        {
            return false;
        }
        let fired = self.fired;
        match self
            .typed
            .back_mut()
            .filter(|typed| typed.after == fired && typed.key.is_none())
        {
            Some(typed) => typed.now.push_str(text),
            None => self.typed.push_back(Typed {
                after: fired,
                key: None,
                target: None,
                follows: true,
                moved: false,
                base: String::new(),
                now: text.to_owned(),
                sent: None,
            }),
        }
        true
    }

    /// Send what the person typed after each press the app ran since, into the view a press
    /// opened for them when `opened` (its first field takes the keyboard).
    fn send_typed_into(&mut self, opened: bool) {
        if opened {
            self.focus_opened();
        }
        self.send_typed();
    }

    /// Send what the person typed after each press the app ran since.
    fn send_typed(&mut self) {
        let mut sent = false;
        while self
            .typed
            .front()
            .is_some_and(|typed| typed.after <= self.took)
        {
            if let Some(typed) = self.typed.pop_front() {
                self.send_typed_now(typed);
                sent = true;
            }
        }
        if sent {
            self.refresh_bound();
        }
    }

    /// Send what the person typed while a press was running into the field it went into,
    /// against what the field shows now: what they typed keeps its place from the field's end.
    /// Typed while no field had the keyboard, and the press opened no field: it goes to the
    /// field the keyboard is in now, at its end; with none, the window says where it went.
    fn send_typed_now(&mut self, mut typed: Typed) {
        if typed.key.is_none() {
            let Some(field) = self.focused_text_field() else {
                if !typed.now.is_empty() {
                    let words = Words::Plain(t_args(
                        "tilolang-typing-unplaced",
                        &[("typed", typed.now.as_str().into())],
                    ));
                    self.notice(&words, wire::NoticeKind::Info, false);
                }
                return;
            };
            typed.sent = Some(self.texts.get(&field).cloned().unwrap_or_default());
            typed.target = self.target(lane_of(&field).0);
            typed.key = Some(field);
        }
        let Some(key) = typed.key else {
            return;
        };
        if typed.moved {
            typed.sent = Some(self.texts.get(&key).cloned().unwrap_or_default());
        }
        let shown = typed.sent.clone().unwrap_or_else(|| typed.base.clone());
        let (at, delete, insert) = splice_between(&typed.base, &typed.now);
        let after = typed.base.len().saturating_sub(at.saturating_add(delete));
        let mut at = shown.len().saturating_sub(after.saturating_add(delete));
        while !shown.is_char_boundary(at) {
            at = at.saturating_sub(1);
        }
        let mut end = at.saturating_add(delete).min(shown.len());
        while !shown.is_char_boundary(end) {
            end = end.saturating_add(1);
        }
        let mut text = shown.clone();
        text.replace_range(at..end, &insert);
        let (lane, field) = lane_of(&key);
        match typed.target {
            // The lane moved on to another resource: the typing goes to the one it was typed
            // into, and the field the lane shows now keeps what it shows.
            Some(target) if self.target(lane).as_ref() != Some(&target) => {
                let (at, delete, insert) = splice_between(&shown, &text);
                self.out.push(ToApp::Splice {
                    lane,
                    target,
                    key: field,
                    seq: self.seqs.get(&key).copied().unwrap_or_default(),
                    at: u32::try_from(at).unwrap_or(u32::MAX),
                    delete: u32::try_from(delete).unwrap_or(u32::MAX),
                    insert,
                });
            }
            _ => {
                if let Some(message) = self.splice(&key, &shown, text) {
                    self.out.push(message);
                }
            }
        }
    }

    /// The bound control the keyboard is in now, by key in the composed tree.
    fn focused_field(&self) -> Option<String> {
        fn focused(widget: &dyn Widget, bound: &HashMap<String, Value>) -> Option<String> {
            if widget.is_focused()
                && let Some(id) = widget.test_id()
                && bound.contains_key(id)
            {
                return Some(id.to_owned());
            }
            widget
                .children()
                .into_iter()
                .find_map(|child| focused(child, bound))
        }
        focused(self.body.as_ref(), &self.bound.borrow())
    }

    /// The bound text control the keyboard is in, by key in the composed tree.
    fn focused_text_field(&self) -> Option<String> {
        let field = self.focused_field()?;
        matches!(self.bound.borrow().get(&field), Some(Value::Text(_))).then_some(field)
    }

    /// The splice of the bound text control `composed` from `before` to `text`, which it now
    /// shows.
    fn splice(&mut self, composed: &str, before: &str, text: String) -> Option<ToApp> {
        let (lane, key) = lane_of(composed);
        // Of the resource the lane shows now: the app applies it there, whatever it shows by
        // the time it takes it.
        let target = self.target(lane)?;
        let (at, delete, insert) = splice_between(before, &text);
        let seq = self.seqs.entry(composed.to_owned()).or_default();
        *seq = seq.saturating_add(1);
        let seq = *seq;
        // What the field shows is what the person typed, whatever the window shows it again
        // from before the app takes it.
        self.bound
            .borrow_mut()
            .insert(composed.to_owned(), Value::Text(text.clone()));
        self.texts.insert(composed.to_owned(), text);
        Some(ToApp::Splice {
            lane,
            target,
            key,
            seq,
            at: u32::try_from(at).unwrap_or(u32::MAX),
            delete: u32::try_from(delete).unwrap_or(u32::MAX),
            insert,
        })
    }

    /// The resource the view of `lane` shows.
    fn target(&self, lane: Lane) -> Option<ResourceRef> {
        match lane {
            Lane::Root => self.root.as_ref(),
            Lane::Detail => self.detail.as_ref(),
        }
        .map(|view| view.target.clone())
    }

    // ----- for embedders and tests ---------------------------------------------------------

    /// The composed shell, as the window last built it.
    #[must_use]
    pub fn composed(&self) -> Option<&UiNode> {
        self.built.as_ref()
    }

    /// The root view's tree, as the window holds it.
    #[must_use]
    pub fn root_tree(&self) -> Option<UiNode> {
        self.root
            .as_ref()
            .map(|view| crate::reconcile::normalize(&view.tree))
    }

    /// The detail view's tree, as the window holds it.
    #[must_use]
    pub fn detail_tree(&self) -> Option<UiNode> {
        self.detail
            .as_ref()
            .map(|view| crate::reconcile::normalize(&view.tree))
    }

    /// The resource the detail view shows.
    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.detail.as_ref().map(|view| &view.target)
    }

    /// The keys of the virtual lists in the composed tree.
    #[must_use]
    pub fn lists(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.rows.borrow().keys().cloned().collect();
        keys.sort();
        keys
    }

    /// The rows of the virtual list `key` (its key in the composed tree).
    #[must_use]
    pub fn rows_of(&self, key: &str) -> Option<Rc<RefCell<RemoteRows>>> {
        self.rows.borrow().get(key).cloned()
    }

    /// What the bound control `key` (its key in the composed tree) shows, as the app last sent
    /// it and the window took it.
    #[must_use]
    pub fn bound_value(&self, key: &str) -> Option<Value> {
        self.bound.borrow().get(key).cloned()
    }

    /// Why the window shows the platform's error state, when it does.
    #[must_use]
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    /// The Ask bar the window shows, if it shows one.
    #[must_use]
    pub fn asks(&self) -> Option<&wire::Ask> {
        self.ask.as_ref()
    }

    /// Answer the Ask bar, as the person does with its buttons.
    pub fn answer(&self, answer: Answer) {
        self.answers.borrow_mut().push(answer);
    }

    /// Tell the app `message` with the next batch: what the embedder knows that the app does
    /// not, such as the names of the room's participants.
    pub fn tell(&mut self, message: ToApp) {
        self.out.push(message);
    }

    /// Report what a person did: an event of a control of the composed shell, by its key
    /// there.
    pub fn act(&self, event: UiEvent) {
        self.events.push(event);
    }

    /// The shell.
    #[must_use]
    pub fn shell_ref(&self) -> Option<&NavShell> {
        self.body
            .children()
            .into_iter()
            .next()?
            .as_any()?
            .downcast_ref::<Pane>()?
            .content()
            .as_any()?
            .downcast_ref::<NavShell>()
    }

    /// The shell, to change it as a person would.
    pub fn shell_mut(&mut self) -> Option<&mut NavShell> {
        self.shell()
    }
}

/// The key `key` of `lane`'s view in the composed tree.
fn composed_key(lane: Lane, key: &str) -> String {
    match lane {
        Lane::Root => key.to_owned(),
        Lane::Detail => format!("{DETAIL}{key}"),
    }
}

/// Where the widget for the node `key` of the composed tree is drawn, when it is on screen.
fn rect_of(widget: &dyn Widget, key: &str) -> Option<ui::kurbo::Rect> {
    if widget.test_id() == Some(key) {
        let rect = widget.get_rect();
        return (rect.width() > 0.0 && rect.height() > 0.0).then_some(rect);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| rect_of(child, key))
}

/// The view a key of the composed tree is of, and the key in that view.
fn lane_of(key: &str) -> (Lane, String) {
    match key.strip_prefix(DETAIL) {
        Some(own) => (Lane::Detail, own.to_owned()),
        None => (Lane::Root, key.to_owned()),
    }
}

/// The virtual lists of a view's tree, by key.
fn virtual_lists(tree: &UiNode) -> Vec<String> {
    fn walk(node: &UiNode, out: &mut Vec<String>) {
        if crate::is_virtual(&node.kind) {
            out.push(node.key.clone());
        }
        for child in &node.children {
            walk(child, out);
        }
    }
    let mut out = Vec::new();
    walk(tree, &mut out);
    out
}

/// What `words` say, in the person's language.
fn say(words: &Words) -> String {
    match words {
        Words::Plain(text) => text.clone(),
        Words::Phrase { id, args } => {
            let args: Vec<(&str, ui::i18n::ArgValue)> = args
                .iter()
                .map(|(name, value)| (name.as_str(), value.as_str().into()))
                .collect();
            t_args(id, &args)
        }
    }
}

/// The splice from `before` to `after`: where they first differ, how many bytes of `before`
/// go from there, and what comes instead, on character boundaries.
#[must_use]
pub fn splice_between(before: &str, after: &str) -> (usize, usize, String) {
    let mut start = before
        .bytes()
        .zip(after.bytes())
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(start) || !after.is_char_boundary(start) {
        start = start.saturating_sub(1);
    }
    let room = before.len().min(after.len()).saturating_sub(start);
    let mut end = before
        .bytes()
        .rev()
        .zip(after.bytes().rev())
        .take(room)
        .take_while(|(a, b)| a == b)
        .count();
    while !before.is_char_boundary(before.len().saturating_sub(end))
        || !after.is_char_boundary(after.len().saturating_sub(end))
    {
        end = end.saturating_sub(1);
    }
    let delete = before.len().saturating_sub(end).saturating_sub(start);
    let insert = after
        .get(start..after.len().saturating_sub(end))
        .unwrap_or_default()
        .to_owned();
    (start, delete, insert)
}

/// Show what each bound control under `node` is bound to, as `bound` holds it by key in the
/// composed tree (each of `node`'s keys carrying `prefix` there).
pub fn show_bound<S: std::hash::BuildHasher>(
    node: &UiNode,
    prefix: &str,
    bound: &HashMap<String, Value, S>,
    reconciler: &Reconciler,
) {
    if node.binding.is_some() {
        let key = format!("{prefix}{}", node.key);
        if let Some(value) = bound.get(&key) {
            let flag = node.kind == "checkbox"
                || node.kind == "toggle"
                || node.kind == crate::adapter::kind("ListRow");
            match value {
                Value::Bool(checked) => reconciler.set_flag(&key, *checked),
                Value::Int(number) if flag => reconciler.set_flag(&key, *number != 0),
                Value::Rich(rich) => reconciler.set_rich(&key, rich),
                other => reconciler.set_text(&key, &crate::reconcile::display(other)),
            }
        }
    }
    for child in &node.children {
        show_bound(child, prefix, bound, reconciler);
    }
}

/// The window stands for its body in the layout and in every walk of the widgets (it adds no
/// node of its own, so a walk that pairs widgets with layout nodes meets the body's children
/// under it), taking the person's input and its own changes on the way.
/// How much a view's size has to change, in rem, before the app is told: less moves no class
/// boundary and no size a view would show differently.
const SPACE_STEP_REM: f32 = 0.125;

/// The widget keyed `key` at or under `widget`.
fn keyed<'a>(widget: &'a dyn Widget, key: &str) -> Option<&'a dyn Widget> {
    if widget.test_id() == Some(key) {
        return Some(widget);
    }
    widget
        .children()
        .into_iter()
        .find_map(|child| keyed(child, key))
}

impl AppWindow {
    /// Tell the app the space each view has where layout placed it, when it changed: a view
    /// reads `space` from its own container (layout.md, "Responding to space"). The app
    /// decides the classes, with their hysteresis, and runs a view again only when what it read
    /// of it changed.
    fn follow_spaces(&mut self) {
        let keys = [
            self.root.as_ref().map(|view| view.tree.key.clone()),
            self.detail
                .as_ref()
                .map(|view| composed_key(Lane::Detail, &view.tree.key)),
        ];
        for (lane, key) in [Lane::Root, Lane::Detail].into_iter().zip(keys) {
            let Some(rect) = key
                .and_then(|key| keyed(self.body.as_ref(), &key).map(Widget::get_rect))
                .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
            else {
                continue;
            };
            let space = (
                rect.width() as f32 / self.rem,
                rect.height() as f32 / self.rem,
            );
            let told = match lane {
                Lane::Root => &mut self.root_space,
                Lane::Detail => &mut self.detail_space,
            };
            let moved = told.is_none_or(|(width, height)| {
                (width - space.0).abs() >= SPACE_STEP_REM
                    || (height - space.1).abs() >= SPACE_STEP_REM
            });
            if moved {
                *told = Some(space);
                self.out.push(ToApp::Space {
                    lane,
                    width: space.0,
                    height: space.1,
                });
            }
        }
    }
}

impl ui::widget::Decorator for AppWindow {
    type Inner = dyn Widget;

    fn build_layout(
        &mut self,
        taffy: &mut ui::taffy::TaffyTree<()>,
        theme: &ui::Theme,
        node_map: &mut HashMap<ui::taffy::NodeId, *mut dyn Widget>,
    ) -> ui::taffy::NodeId {
        self.rem = ui::widget::arrange::rem_of(theme);
        self.body.build_layout(taffy, theme, node_map)
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.body.apply_layout(taffy, node, origin);
        self.follow_spaces();
    }

    fn inner(&self) -> &(dyn Widget + 'static) {
        self.body.as_ref()
    }

    fn inner_mut(&mut self) -> &mut (dyn Widget + 'static) {
        self.body.as_mut()
    }

    fn name(&self) -> &'static str {
        "TilolangWindow"
    }

    fn handle_event(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        ctx: &mut ui::widget::EventCtx<'_>,
    ) {
        // Focus moves on a key, a press or a touch, never on a pointer that only moves.
        use ui::winit::event::WindowEvent;
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::Ime(_)
        ) {
            self.input = true;
        }
        if let WindowEvent::Ime(ime) = event {
            let composing =
                matches!(ime, ui::winit::event::Ime::Preedit(text, _) if !text.is_empty());
            if composing != self.composing {
                self.composing = composing;
                self.out.push(ToApp::Composing(composing));
            }
        }
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ui::winit::event::ElementState::Pressed,
                ..
            }
        ) || matches!(event, WindowEvent::Touch(touch) if touch.phase == ui::winit::event::TouchPhase::Started)
        {
            self.pointed_after = Some(self.fired);
        }
        if self.hold_typing(event) {
            *ctx.handled = true;
            return;
        }
        self.body.handle_event(event, ctx);
    }

    fn take_focus_request(&mut self) -> Option<String> {
        self.focus_request
            .take()
            .or_else(|| self.body.take_focus_request())
    }

    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.structure_dirty);
        self.body.take_structure_dirty() || dirty
    }

    /// The window's own changes (a shell built, a bar shown) rebuild; what its body describes
    /// (a column's content changed in place) stays described.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        let own = ui::widget::StructureChange::of(std::mem::take(&mut self.structure_dirty));
        own.join(self.body.take_structure_change())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_splice_is_where_the_texts_differ() {
        assert_eq!(
            splice_between("Q3 plan", "Q3 plans"),
            (7, 0, "s".to_owned())
        );
        assert_eq!(splice_between("Q3 plans", "Q3"), (2, 6, String::new()));
        assert_eq!(splice_between("abc", "aXc"), (1, 1, "X".to_owned()));
        assert_eq!(splice_between("", "hi"), (0, 0, "hi".to_owned()));
        assert_eq!(splice_between("aa", "aaa"), (2, 0, "a".to_owned()));
        assert_eq!(splice_between("né", "nè"), (1, 2, "è".to_owned()));
        for (before, after) in [("naïve", "naive"), ("日本", "日本語"), ("x", "")] {
            let (at, delete, insert) = splice_between(before, after);
            let mut text = before.to_owned();
            text.replace_range(at..at + delete, &insert);
            assert_eq!(text, after);
        }
    }

    fn send(app: &mut tilo_render_protocol::patch::local::End, batch: &[ToUi]) {
        app.send(wire::encode_batch(batch).expect("encodes"))
            .expect("sends");
    }

    fn sent(app: &mut tilo_render_protocol::patch::local::End) -> Vec<ToApp> {
        let mut out = Vec::new();
        while let Some(batch) = app.recv().expect("receives") {
            out.extend(wire::decode_batch::<ToApp>(&batch).expect("a batch"));
        }
        out
    }

    /// The note the page shows.
    fn doc() -> ResourceRef {
        ResourceRef {
            id: "doc".to_owned(),
            ty: "notes.Note".to_owned(),
        }
    }

    /// A page with one text field bound to a note's title.
    fn titled_page() -> Vec<ToUi> {
        let mut input = UiNode::empty("ui.TextInput", "title");
        input.binding = Some(crate::Binding {
            field: Some(("doc/notes:a".to_owned(), "title".to_owned())),
            name: "Title".to_owned(),
        });
        let mut page = UiNode::empty("stack", "page");
        page.children.push(input);
        vec![
            ToUi::Open {
                lane: Lane::Root,
                target: doc(),
                tree: page,
                front: None,
            },
            ToUi::Bound {
                lane: Lane::Root,
                key: "title".to_owned(),
                value: Value::text("Q3"),
                acked: 0,
            },
        ]
    }

    /// The spaces the window told the app, as (lane, width, height) in rem.
    fn spaces(app: &mut tilo_render_protocol::patch::local::End) -> Vec<(Lane, f32, f32)> {
        sent(app)
            .into_iter()
            .filter_map(|message| match message {
                ToApp::Space {
                    lane,
                    width,
                    height,
                } => Some((lane, width, height)),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_view_is_told_the_space_layout_gave_it_when_it_changes_by_more_than_a_step() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        send(&mut app, &titled_page());
        window.receive();
        let mut harness =
            ui::headless::FrameHarness::new(ui::State::new(), Box::new(window), (390, 844));
        let flush = |harness: &mut ui::headless::FrameHarness| {
            harness
                .root_widget_mut()
                .as_any_mut()
                .and_then(|any| any.downcast_mut::<AppWindow>())
                .expect("the window")
                .flush();
        };
        flush(&mut harness);
        let rem = f64::from(ui::widget::arrange::rem_of(harness.theme()));
        let told = spaces(&mut app);
        let [(Lane::Root, width, height)] = told[..] else {
            panic!("the root view is told its space once: {told:?}");
        };
        assert!(
            (f64::from(width) * rem - 390.0).abs() < 1.0 && f64::from(height) * rem <= 844.0,
            "the window's width, in rem: {width} × {height} at {rem} px"
        );
        // A pixel more moves nothing a view could read: nothing is said.
        harness.resize((391, 844)).expect("lays out");
        flush(&mut harness);
        assert_eq!(spaces(&mut app), []);
        // A desktop window is a new space.
        harness.resize((1280, 800)).expect("lays out");
        flush(&mut harness);
        let told = spaces(&mut app);
        assert!(
            matches!(told[..], [(Lane::Root, width, _)] if (f64::from(width) * rem - 1280.0).abs() < 1.0),
            "{told:?}"
        );
    }

    #[test]
    fn typing_is_sent_as_splices_and_what_the_app_sent_before_it_saw_them_is_not_shown() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        send(&mut app, &titled_page());
        window.receive();
        assert_eq!(window.bound_value("title"), Some(Value::text("Q3")));
        // The person types: the field shows it at once (it is the toolkit's), and the app hears
        // of it as a splice of what the field showed.
        window.act(UiEvent::Edit("title".to_owned(), "Q3 plan".to_owned()));
        window.flush();
        assert_eq!(
            sent(&mut app),
            [ToApp::Splice {
                lane: Lane::Root,
                target: doc(),
                key: "title".to_owned(),
                seq: 1,
                at: 2,
                delete: 0,
                insert: " plan".to_owned(),
            }]
        );
        // What the app sent before it took the splice does not take the typing back.
        send(
            &mut app,
            &[ToUi::Bound {
                lane: Lane::Root,
                key: "title".to_owned(),
                value: Value::text("Q3"),
                acked: 0,
            }],
        );
        window.receive();
        assert_eq!(
            window.bound_value("title"),
            Some(Value::text("Q3 plan")),
            "the field keeps what the person typed"
        );
        window.act(UiEvent::Edit("title".to_owned(), "Q3 plans".to_owned()));
        window.flush();
        assert!(
            matches!(&sent(&mut app)[..], [ToApp::Splice { seq: 2, at: 7, delete: 0, insert, .. }] if insert == "s"),
            "the next splice is against what the person typed"
        );
        // What it sent once it took them is shown.
        send(
            &mut app,
            &[ToUi::Bound {
                lane: Lane::Root,
                key: "title".to_owned(),
                value: Value::text("Q3 plans"),
                acked: 2,
            }],
        );
        window.receive();
        assert_eq!(window.bound_value("title"), Some(Value::text("Q3 plans")));
    }

    #[test]
    fn typing_after_a_press_goes_to_the_field_as_the_press_left_it_once_the_app_ran_it() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        send(&mut app, &titled_page());
        window.receive();
        // A press, and typing at once into the field focused: the typing waits for the press.
        window.act(UiEvent::Press("new".to_owned()));
        window.act(UiEvent::Edit("title".to_owned(), "Q3 x".to_owned()));
        window.flush();
        assert!(
            matches!(&sent(&mut app)[..], [ToApp::Fire { key, .. }] if key == "new"),
            "the press goes at once, the typing waits"
        );
        window.act(UiEvent::Edit("title".to_owned(), "Q3 xy".to_owned()));
        window.flush();
        assert!(sent(&mut app).is_empty(), "still waiting for the press");
        // The press opened another note into the field, and the app says it ran it: what was
        // typed goes to that note's title, at its end.
        let mut opened = titled_page();
        opened[1] = ToUi::Bound {
            lane: Lane::Root,
            key: "title".to_owned(),
            value: Value::text("Untitled"),
            acked: 0,
        };
        opened.push(ToUi::Took { fired: 1 });
        send(&mut app, &opened);
        window.receive();
        window.flush();
        assert_eq!(
            sent(&mut app),
            [ToApp::Splice {
                lane: Lane::Root,
                target: doc(),
                key: "title".to_owned(),
                seq: 1,
                at: 8,
                delete: 0,
                insert: " xy".to_owned(),
            }]
        );
        assert_eq!(
            window.bound_value("title"),
            Some(Value::text("Untitled xy"))
        );
        // From here on typing goes as it is typed.
        window.act(UiEvent::Edit("title".to_owned(), "Untitled xyz".to_owned()));
        window.flush();
        assert!(
            matches!(&sent(&mut app)[..], [ToApp::Splice { seq: 2, at: 11, insert, .. }] if insert == "z")
        );
    }

    /// The note `id`.
    fn note(id: &str) -> ResourceRef {
        ResourceRef {
            id: id.to_owned(),
            ty: "notes.Note".to_owned(),
        }
    }

    /// The note `id` opened beside the page, its title showing `title`.
    fn opened_note(id: &str, title: &str) -> Vec<ToUi> {
        let mut input = UiNode::empty("ui.TextInput", "title");
        input.binding = Some(crate::Binding {
            field: Some((format!("{id}/notes:a"), "title".to_owned())),
            name: "Title".to_owned(),
        });
        let mut page = UiNode::empty("stack", "note");
        page.children.push(input);
        vec![
            ToUi::Open {
                lane: Lane::Detail,
                target: note(id),
                tree: page,
                front: Some(wire::Front::Input),
            },
            ToUi::Bound {
                lane: Lane::Detail,
                key: "title".to_owned(),
                value: Value::text(title),
                acked: 0,
            },
        ]
    }

    #[test]
    fn typing_into_a_field_put_the_keyboard_in_while_a_press_runs_stays_with_its_note() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        send(&mut app, &titled_page());
        send(&mut app, &opened_note("a", "Alpha"));
        window.receive();
        let title = format!("{DETAIL}title");
        // The person presses New, clicks into note a's title and types there before the app
        // ran the press.
        window.act(UiEvent::Press("new".to_owned()));
        window.flush();
        assert!(
            matches!(&sent(&mut app)[..], [ToApp::Fire { key, .. }] if key == "new"),
            "the press goes at once"
        );
        window.pointed_after = Some(window.fired);
        window.act(UiEvent::Edit(title.clone(), "Alpha!".to_owned()));
        window.flush();
        assert!(sent(&mut app).is_empty(), "the typing waits for the press");
        // The press opened note b, and the app says it ran it.
        let mut opened = opened_note("b", "Untitled");
        opened.push(ToUi::Took { fired: 1 });
        send(&mut app, &opened);
        window.receive();
        window.flush();
        assert_eq!(
            sent(&mut app),
            [ToApp::Splice {
                lane: Lane::Detail,
                target: note("a"),
                key: "title".to_owned(),
                seq: 0,
                at: 5,
                delete: 0,
                insert: "!".to_owned(),
            }],
            "what was typed into note a's title goes to note a"
        );
        assert_eq!(
            window.bound_value(&title),
            Some(Value::text("Untitled")),
            "note b's title shows what it holds, none of note a's typing"
        );
    }

    #[test]
    fn typing_after_a_press_in_the_field_that_still_had_the_keyboard_follows_into_what_it_opens() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        send(&mut app, &titled_page());
        send(&mut app, &opened_note("a", "Alpha"));
        window.receive();
        let title = format!("{DETAIL}title");
        // The person presses New (in the window's bar, which leaves the keyboard in note a's
        // title) and types on at once, before the app ran the press.
        window.act(UiEvent::Press("new".to_owned()));
        window.act(UiEvent::Edit(title.clone(), "AlphaEcho".to_owned()));
        window.flush();
        assert!(
            matches!(&sent(&mut app)[..], [ToApp::Fire { key, .. }] if key == "new"),
            "the typing waits for the press"
        );
        let mut opened = opened_note("b", "Untitled");
        opened.push(ToUi::Took { fired: 1 });
        send(&mut app, &opened);
        window.receive();
        window.flush();
        assert_eq!(
            sent(&mut app),
            [ToApp::Splice {
                lane: Lane::Detail,
                target: note("b"),
                key: "title".to_owned(),
                seq: 1,
                at: 8,
                delete: 0,
                insert: "Echo".to_owned(),
            }],
            "only what was typed after the press goes to note b's title, where the press left \
             the keyboard; note a keeps what it held"
        );
        assert_eq!(
            window.bound_value(&title),
            Some(Value::text("UntitledEcho"))
        );
    }

    #[test]
    fn typing_between_what_a_press_opened_and_its_took_follows_into_the_opened_field() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        send(&mut app, &titled_page());
        send(&mut app, &opened_note("a", "Alpha"));
        window.receive();
        let title = format!("{DETAIL}title");
        window.act(UiEvent::Press("new".to_owned()));
        window.flush();
        let _ = sent(&mut app);
        // The note the press opened arrives in one batch, the app's Took in the next; the
        // person types in between, in the field that had the keyboard, which shows note b now.
        send(&mut app, &opened_note("b", ""));
        window.receive();
        window.act(UiEvent::Edit(title, "Echo".to_owned()));
        window.flush();
        assert!(sent(&mut app).is_empty(), "the typing waits for the press");
        send(&mut app, &[ToUi::Took { fired: 1 }]);
        window.receive();
        window.flush();
        assert!(
            matches!(&sent(&mut app)[..], [ToApp::Splice { target, insert, .. }] if *target == note("b") && insert == "Echo"),
            "the typing goes to the opened note's field"
        );
    }

    #[test]
    fn a_batch_that_breaks_the_protocol_shows_the_error_state_and_nothing_else() {
        let (mut app, window_end) = tilo_render_protocol::patch::local::pair();
        let mut window = AppWindow::new(Box::new(window_end), Vec::new());
        let unknown = vec![ToUi::Open {
            lane: Lane::Root,
            target: ResourceRef {
                id: "doc".to_owned(),
                ty: "app.Root".to_owned(),
            },
            tree: UiNode::empty("ui.NoSuchComponent", "root"),
            front: None,
        }];
        app.send(wire::encode_batch(&unknown).expect("encodes"))
            .expect("sends");
        assert!(window.receive());
        assert!(window.failed().is_some());
        assert!(window.composed().is_none(), "nothing of it was built");
        app.send(b"scribbles".to_vec()).expect("sends");
        assert!(window.receive(), "a scribble is refused, not a panic");
    }
}
