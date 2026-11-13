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
mod page;

use std::collections::{BTreeMap, HashMap};
use std::rc::Rc;

use page::{chosen_item, content_nodes, node_of_key};
use tilo_render_protocol::patch::validate::{PatchLimits, Validator};
use tilo_render_protocol::patch::{self as wire, Answer, Lane, ToApp, ToUi, Transport, Words};
use ui::tokens::{MotionSource, Rhythm, TextRole};
use ui::widget::arrange::{Fit, Row, Slot, Stack};
use ui::widget::toast::ToastKind;
use ui::widget::{Absent, Button, ButtonRole, Element, NavPane, NavShell, Text, Widget};

use crate::i18n::{t, t_args};
use crate::pane::Pane;
use crate::reconcile::{EventSink, Reconciler, UiEvent};
use crate::rows::{BoundValues, RemoteRows};
use crate::shell::DETAIL;
use crate::{ResourceRef, UiNode, Value};

/// Every virtual list of the window's views, by the key of its node in the window's tree.
pub type Registry = Rc<RefCell<HashMap<String, Rc<RefCell<RemoteRows>>>>>;

/// A list whose rows the window keeps: its key in the window's tree, the lane and prefix of the
/// view it stands in, the kind of component a row is, and the words of the heading over its rows
/// where one heads them.
struct Rows {
    key: String,
    lane: Lane,
    prefix: &'static str,
    list: String,
    item: String,
    heading: Option<String>,
}

impl Rows {
    /// The list `list` of the view of `lane` whose nodes carry `prefix`.
    fn new(
        prefix: &'static str,
        lane: Lane,
        list: &str,
        item: String,
        heading: Option<String>,
    ) -> Self {
        Self {
            key: format!("{prefix}{list}"),
            lane,
            prefix,
            list: list.to_owned(),
            item,
            heading,
        }
    }
}

/// One of the window's two views, as the app last sent it.
struct View {
    /// The resource it shows.
    target: ResourceRef,
    /// Its tree, without the rows of its virtual lists.
    tree: UiNode,
}

/// What a bound control holds, as the person's typing left it: the text it showed when they
/// began and now, or the rich text. Every bound control that takes typing reports in one of
/// these, so what the person types while a press runs waits for it whichever kind of control
/// the keyboard is in.
#[derive(Clone, Debug)]
enum Held {
    /// A text control (or a number, a date): what it showed when they began, and now.
    Text { base: String, now: String },
    /// A rich-text control: what it held when they began, and now.
    Rich {
        base: wire::rich::RichText,
        now: wire::rich::RichText,
    },
}

impl Held {
    /// The text it reads as: a rich text's paragraphs, a line each.
    fn texts(&self) -> (String, String) {
        match self {
            Self::Text { base, now } => (base.clone(), now.clone()),
            Self::Rich { base, now } => (rich_text_of(base), rich_text_of(now)),
        }
    }
}

/// `rich` as the text a person reads, a line per paragraph.
fn rich_text_of(rich: &wire::rich::RichText) -> String {
    rich.blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `rich` with `delete` bytes at byte `at` of its text replaced by `insert` (paragraphs a line
/// each, as [`rich_text_of`] reads them): the change lands in the paragraph the caret is in,
/// and every mark around it moves with the text it marks. Typing is characters, never a
/// paragraph break: Enter's key text is a control character, which is never held.
fn rich_spliced_at(
    rich: &wire::rich::RichText,
    at: usize,
    delete: usize,
    insert: &str,
) -> wire::rich::RichText {
    let mut rich = rich.clone();
    if rich.blocks.is_empty() {
        rich.blocks.push(wire::rich::Block {
            kind: "paragraph".to_owned(),
            ..wire::rich::Block::default()
        });
    }
    // The paragraph the caret is in, and the byte in it.
    let mut start = 0usize;
    let mut index = rich.blocks.len().saturating_sub(1);
    for (i, block) in rich.blocks.iter().enumerate() {
        if at <= start.saturating_add(block.text.len()) {
            index = i;
            break;
        }
        start = start.saturating_add(block.text.len()).saturating_add(1);
    }
    let Some(block) = rich.blocks.get_mut(index) else {
        return rich;
    };
    let mut local = at.saturating_sub(start).min(block.text.len());
    while !block.text.is_char_boundary(local) {
        local = local.saturating_sub(1);
    }
    let mut end = local.saturating_add(delete).min(block.text.len());
    while !block.text.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    // The marks count characters: the range they replace, and what it gives way to.
    let first = block
        .text
        .get(..local)
        .map_or(0, |head| head.chars().count());
    let removed = block
        .text
        .get(local..end)
        .map_or(0, |gone| gone.chars().count());
    let added = insert.chars().count();
    let last = first.saturating_add(removed);
    for mark in &mut block.marks {
        let moved = |chars: usize| -> usize {
            if chars <= first {
                chars
            } else if chars >= last {
                chars.saturating_sub(removed).saturating_add(added)
            } else {
                first.saturating_add(added)
            }
        };
        mark.start = moved(mark.start);
        mark.end = moved(mark.end);
    }
    block.marks.retain(|mark| mark.start < mark.end);
    block.text.replace_range(local..end, insert);
    rich
}

/// `text` with `(at, end)` replaced by `insert`.
fn splice_text(text: &str, (at, end): (usize, usize), insert: &str) -> String {
    let mut out = text.to_owned();
    out.replace_range(at..end, insert);
    out
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
    /// It is the field a press opened's, typed while the window's request for the keyboard to
    /// go there was still on its way ([`AppWindow::hold_for_landing`]): it goes once the app
    /// took that request.
    waits_focus: bool,
    /// What the control holds as they typed.
    held: Held,
    /// What the app sent for the field since (it showed something else meanwhile, such as the
    /// note the press opened).
    sent: Option<Value>,
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

/// What the window is to whoever shows it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    /// An app's window: its views composed into the one shell.
    Window,
    /// A part of a shell ([`AppWindow::part`]): the root view as it is.
    Part,
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
    /// The task runs a change mark asked to show ([`ToUi::ShowRun`]), until the window's
    /// host takes them ([`Self::take_shown_runs`]).
    shown_runs: Vec<String>,
    /// How many presses the window sent the app (`ToApp::Fire`, `ToApp::Invoke`), and how many
    /// of them the app said it ran (`ToUi::Took`).
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
    /// The field a press opened, the window asked the keyboard for and has not heard back of:
    /// what the person types meanwhile is that field's, not the one the keyboard is leaving
    /// ([`Self::hold_for_landing`]). In the composed tree.
    landing: Option<String>,
    /// The item the window's own view shows in place (a tree beside what it shows): what the
    /// fields of its content belong to.
    shown_in_place: Option<ResourceRef>,
    /// What the row of the item the window's own view shows as its page names it: what the
    /// window is named by while it shows one ([`Self::title`]).
    page_title: Option<String>,
    /// The field of the page the window draws for the item a press showed, until the keyboard
    /// is in it: the page is built again when the item changes, and the keyboard goes with it.
    page_focus: Option<String>,
    /// How many times the window asked for it, so a field that never takes the keyboard does
    /// not have the page built again on every frame.
    page_focus_tries: u8,
    /// What the app last said each bound control holds, by key: what the window shows again
    /// when the person's typing goes elsewhere.
    app_bound: HashMap<String, Value>,
    /// The item each press showed the window's own view (by the press's count): what the
    /// person typed after that press belongs to it, even once a later press showed another.
    shows_by_press: BTreeMap<u64, ResourceRef>,
    /// How many presses the window had sent when the app last opened a view for the person's
    /// input, until the app says it ran them (`ToUi::Took`).
    opened: Option<u64>,
    /// One `rem` in logical pixels under the theme the window was last laid out in.
    rem: f32,
    /// The space last told the app each lane's view has, in rem.
    root_space: Option<(f32, f32)>,
    detail_space: Option<(f32, f32)>,
    /// The column the window keeps in front of a narrow window: what a person's tap or the
    /// app's open brought forward, held while the shell is built anew. A shell built from the
    /// views starts on the screen a new window opens on; the window itself remembers where the
    /// person is, so a patch that changes the views' keys does not send them back.
    front: Option<NavPane>,
    /// A part of a shell rather than a window ([`Self::part`]): the root view shown as it is,
    /// with no window shell around it.
    shape: Shape,
    /// What the app said a part has no window to show, until its embedder takes it.
    notices: Vec<(wire::NoticeKind, String)>,
    /// The actions the app offers the person at this window, as their command palette lists
    /// them ([`ToUi::Actions`]).
    actions: Vec<tilo_actions::ActionDescriptor>,
    /// How many rows the frame's list showed before the search typed anything: what the
    /// search's counter counts them of.
    search_total: Option<usize>,
}

impl AppWindow {
    /// A part of a shell (its top bar, a dock) receiving its views over `transport`: the root
    /// view's tree shown as it is, over the whole area the part is given, with no window
    /// shell, header or ground of its own. The view fills the area, so what it places at the
    /// area's edges (a dock at the foot) stands there however little it shows between them;
    /// what it does not draw stays clear, so what lies under the part shows through, and what
    /// the view opens (a menu) has room.
    #[must_use]
    pub fn part(transport: Box<dyn Transport>) -> Self {
        let mut window = Self::new(transport, Vec::new());
        window.shape = Shape::Part;
        window.body = Box::new(
            Stack::stack()
                .fill_width()
                .fill_height()
                .push_slot(slot(
                    Box::new(Pane::new(Box::new(Absent::new()))),
                    Fit::fill(1.0),
                ))
                .push_slot(slot(Box::new(Absent::new()), Fit::hug()))
                .push_slot(slot(Box::new(Absent::new()), Fit::hug())),
        );
        window
    }

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
            shown_runs: Vec::new(),
            fired: 0,
            took: 0,
            typed: std::collections::VecDeque::new(),
            pointed_after: None,
            focus_request: None,
            landing: None,
            shown_in_place: None,
            page_title: None,
            page_focus: None,
            page_focus_tries: 0,
            app_bound: HashMap::new(),
            shows_by_press: BTreeMap::new(),
            opened: None,
            rem: 16.0,
            root_space: None,
            detail_space: None,
            front: None,
            shape: Shape::Window,
            notices: Vec::new(),
            actions: Vec::new(),
            search_total: None,
        }
    }

    /// What the app said (a refusal, a done action) since this was last asked, where the
    /// window is a part with no shell to toast in.
    pub fn take_notices(&mut self) -> Vec<(wire::NoticeKind, String)> {
        std::mem::take(&mut self.notices)
    }

    /// The actions the app offers the person at this window, as its command palette lists
    /// them ([`ToUi::Actions`]): labels, keys, icons and what they act on, for the window to
    /// offer them the way it offers its own.
    #[must_use]
    pub fn actions(&self) -> &[tilo_actions::ActionDescriptor] {
        &self.actions
    }

    /// The person chose `address` from the window's command palette with `argument`, or pressed
    /// a control the app published the action for: the app runs it as their own step. `argument`
    /// is the palette's collected text or canonical JSON arguments, as a prompt or typed form
    /// encodes them.
    ///
    /// An action whose key a text field of the view declares (a list's entry) is pressed where
    /// that field stands: the press puts the keyboard in it, as the key does, rather than
    /// making an empty item the person never typed.
    pub fn invoke_action(
        &mut self,
        address: tilo_actions::ActionAddress,
        argument: Option<String>,
    ) {
        if argument.is_none()
            && let Some(key) = self
                .actions()
                .iter()
                .find(|action| action.address == address)
                .and_then(|action| action.state.shortcut.as_deref())
            && let Some(field) = self
                .built
                .as_ref()
                .and_then(|built| focus_field(built, &ui::platform::Shortcut::from(key)))
        {
            self.focus_request = Some(field);
            self.flush();
            return;
        }
        // A press like any other: what the person types while the app runs it goes where it
        // leaves the keyboard, and what it makes opens as their input.
        self.fired = self.fired.saturating_add(1);
        self.tell(ToApp::Invoke { address, argument });
    }

    /// Whether the app asked for the window to come forward since this was last asked
    /// ([`ToUi::Activate`]).
    pub fn take_activation(&mut self) -> bool {
        std::mem::take(&mut self.activations) > 0
    }

    /// The first task run a change mark in the window asked to show and the window's host has
    /// not taken yet ([`ToUi::ShowRun`]): the host shows it where the system keeps its log.
    pub fn take_shown_run(&mut self) -> Option<String> {
        (!self.shown_runs.is_empty()).then(|| self.shown_runs.remove(0))
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
        if any {
            self.page_title = self.shown_item_title();
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
        // Rows the app sent for a list: its selection then names rows it did not hold yet.
        let mut arrived = false;
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
                ToUi::ShowRun { run } => self.shown_runs.push(run),
                ToUi::Speak(language) => ui::i18n::I18nManager::set_locale(&language),
                ToUi::Open {
                    lane,
                    target,
                    tree,
                    front: brought,
                } => {
                    opened_for_input |= self.opened(lane, target, tree, brought);
                    content = true;
                    front = brought.or(front);
                }
                ToUi::Close => {
                    if self.detail.take().is_some() {
                        self.forget_lane(Lane::Detail);
                        content = true;
                    }
                }
                ToUi::Front {
                    front: brought,
                    shows,
                } => {
                    front = Some(brought);
                    opened_for_input |= self.fronted(brought, shows);
                }
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
                rows @ (ToUi::Rows { .. } | ToUi::RowsChanged { .. }) => {
                    self.take_rows(rows);
                    // A row that arrived can hold a list of its own (the document of a row
                    // standing open), whose rows the app sends next.
                    self.track_rows();
                    arrived = true;
                }
                ToUi::Notice { words, kind, undo } => self.notice(&words, kind, undo),
                ToUi::Ask(ask) => self.rebuild_ask(ask.as_ref()),
                ToUi::Form(form) => self.show_form(form.as_deref()),
                ToUi::Actions(catalogue) => self.take_actions(catalogue)?,
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
        if arrived {
            // The list can name the row of the view shown only now that it holds the rows.
            self.select_shown();
        }
        self.send_typed_into(opened_for_input);
        self.keep_page_focus();
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

    /// The lane `lane` opened on `target` with the tree `tree`, coming to the front of a narrow
    /// window when `brought` names the press that opened it. Whether that press takes the
    /// keyboard into the page.
    fn opened(
        &mut self,
        lane: Lane,
        target: ResourceRef,
        tree: UiNode,
        brought: Option<wire::Front>,
    ) -> bool {
        if lane == Lane::Root {
            self.failed = None;
        }
        self.forget_lane(lane);
        let view = View { target, tree };
        match lane {
            Lane::Root => {
                self.root = Some(view);
                // What the window's own view showed in place is no part of a view that opened
                // again.
                self.shown_in_place = None;
                self.page_focus = None;
                self.page_focus_tries = 0;
            }
            Lane::Detail => self.detail = Some(view),
        }
        self.track_rows();
        if lane == Lane::Detail && brought == Some(wire::Front::Input) {
            // Its Took may come in a later batch: what is typed until then follows the press
            // into it.
            self.opened = Some(self.fired);
            return true;
        }
        false
    }

    /// The page the person's press showed is built again with the item: the keyboard goes into
    /// it once it stands, and once more if the page was built after the first ask.
    fn keep_page_focus(&mut self) {
        let Some(field) = self.page_focus.clone() else {
            return;
        };
        if self.focused_field().as_deref() == Some(field.as_str()) || self.page_focus_tries >= 2 {
            self.page_focus = None;
        } else {
            self.page_focus_tries = self.page_focus_tries.saturating_add(1);
            self.focus_request = Some(field);
            self.structure_dirty = true;
        }
    }

    /// What the app offers the person at the window now: every action, or the actions of the
    /// resources whose labels moved, which replace theirs where they sit.
    fn take_actions(&mut self, catalogue: wire::ActionCatalogue) -> Result<(), String> {
        match catalogue {
            wire::ActionCatalogue::All(actions) => self.actions = crate::catalogue::read(&actions)?,
            wire::ActionCatalogue::Relabeled(actions) => {
                for action in crate::catalogue::read(&actions)? {
                    if let Some(held) = self
                        .actions
                        .iter_mut()
                        .find(|held| held.address == action.address)
                    {
                        *held = action;
                    } else {
                        // An action the window does not hold yet: it takes its place after
                        // those it knows of its resource.
                        let at = self
                            .actions
                            .iter()
                            .rposition(|held| held.address.resource == action.address.resource)
                            .map_or(self.actions.len(), |last| last.saturating_add(1));
                        self.actions.insert(at, action);
                    }
                }
            }
        }
        // The window publishes each action as the resource its controls stand on.
        let published = self.catalogue();
        self.bind_published(&published);
        Ok(())
    }

    /// Bind the window's controls to the actions the app publishes (`published`): a control
    /// whose event runs one of them runs it through the door, the one path anyone else running
    /// the action takes, and is where an agent running it is seen. An app whose instance runs
    /// in the window's own process publishes its actions itself and binds them here; an app in
    /// a host sends them ([`ToUi::Actions`]).
    pub fn bind_published(&mut self, published: &[tilo_actions::ResourceSpec]) {
        // A control is bound to the action its event runs when its widget is built: controls
        // whose events would now take the other path are built again, so a press and anyone
        // else running the action take the one path. The app publishing the same actions again
        // binds nothing anew.
        let rebind = {
            let reconciler = self.reconciler.borrow();
            let adapter = reconciler.adapter();
            adapter.published(published.to_vec()).is_some_and(|shown| {
                self.built
                    .as_ref()
                    .is_some_and(|built| rebinding(&shown, adapter, built))
            })
        };
        if rebind {
            self.built = None;
            self.compose();
        }
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
        let loading = ui::widget::EmptyState::new(ui::i18n::widget_text("arriving-loading")).busy();
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
        // The field a press opened for this lane is gone with it: what was typed for it goes
        // to the resource it was typed into, wherever the lane shows now.
        if self.landing.as_ref().is_some_and(ours) {
            self.landing = None;
        }
    }

    /// Keep rows for every virtual list of the views and of the rows they hold, and drop the
    /// rows of lists gone.
    fn track_rows(&self) {
        // Each list with the words of the heading over it, where one heads its rows.
        let mut wanted: Vec<Rows> = Vec::new();
        for (view, lane, prefix) in [
            (self.root.as_ref(), Lane::Root, ""),
            (self.detail.as_ref(), Lane::Detail, DETAIL),
        ] {
            let Some(view) = view else { continue };
            for (list, item, heading) in crate::rows::lists(&view.tree) {
                wanted.push(Rows::new(prefix, lane, &list, item, heading));
            }
        }
        // A row a list holds can hold a list of its own — the document of a row standing open
        // where it stands — and so can that list's rows.
        let mut at = 0;
        while let Some(rows) = wanted.get(at) {
            let (lane, prefix, heading) = (rows.lane, rows.prefix, rows.heading.clone());
            let inner: Vec<(String, String, Option<String>)> = self
                .rows
                .borrow()
                .get(&rows.key)
                .map(|rows| {
                    let rows = rows.borrow();
                    rows.rendered()
                        .iter()
                        .filter_map(|row| rows.row(row))
                        .flat_map(|row| crate::rows::lists_under(row, heading.as_deref()))
                        .collect()
                })
                .unwrap_or_default();
            for (list, item, heading) in inner {
                wanted.push(Rows::new(prefix, lane, &list, item, heading));
            }
            at = at.saturating_add(1);
        }
        let mut registry = self.rows.borrow_mut();
        registry.retain(|key, rows| {
            wanted
                .iter()
                .any(|want| &want.key == key && rows.borrow().reads(want.lane, &want.list))
        });
        for rows in wanted {
            let kept = registry.entry(rows.key.clone()).or_insert_with(|| {
                let reader = Rc::new(RefCell::new(Reconciler::sharing(&self.reconciler.borrow())));
                Rc::new(RefCell::new(RemoteRows::new(
                    rows.lane,
                    &rows.list,
                    &rows.item,
                    rows.prefix,
                    reader,
                    Rc::clone(&self.bound),
                )))
            });
            kept.borrow_mut().under(rows.heading.clone());
        }
    }

    /// What the bound control `key` of `lane` shows now, as the app sends it: taken unless the
    /// person typed into it since what the app took (`acked`).
    fn take_bound(&mut self, lane: Lane, key: &str, value: Value, acked: u64) {
        let key = composed_key(lane, key);
        self.app_bound.insert(key.clone(), value.clone());
        // The field keeps what the person typed into it while a press was running, while it
        // still shows the resource they typed into.
        let shows = self.target(lane);
        if let Some(typed) = self.typed.iter_mut().find(|typed| {
            typed.key.as_deref() == Some(key.as_str())
                && typed.target.is_some()
                && typed.target == shows
        }) {
            // What the app sent for the field: the typing goes against it when the app ran the
            // press. The field keeps showing what the person typed into it, which the app has
            // not taken: a value it held before that is no reason to take their words away.
            typed.sent = Some(value);
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
        if self.shape == Shape::Part {
            // A part has no window to toast in: its embedder says it where the shell says
            // things ([`Self::take_notices`]).
            self.notices.push((kind, text));
            return;
        }
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
        // The shell reports the columns its own front moves, and owes the frame that slides
        // them; a window whose content already stood in front — a press on what the person was
        // reading — changed nothing its layout is built for.
        let already_in_front = self.front == Some(NavPane::Content);
        if let Some(shell) = self.shell() {
            shell.show_from(NavPane::Content, source);
        }
        self.front = Some(NavPane::Content);
        if !already_in_front {
            self.structure_dirty = true;
        }
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
        if self.shape == Shape::Part && self.root.is_none() {
            // A part whose whole view is another app's state has no root of its own: what it
            // shows is the detail lane's view, keyed as a window's detail view is so a press
            // reaches the view that drew it.
            let Some(detail) = self.detail.as_ref() else {
                return;
            };
            let tree = crate::reconcile::normalize(&detail.tree);
            let composed = crate::shell::prefixed(&crate::shell::without_rows(&tree), DETAIL);
            self.reconcile_into_pane(&composed);
            self.built = Some(composed);
            self.refresh_bound();
            return;
        }
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let root_tree = crate::reconcile::normalize(&root.tree);
        if self.shape == Shape::Part {
            // A shell's part is its view, with no window shell composed around it.
            let composed = crate::shell::without_rows(&root_tree);
            self.reconcile_into_pane(&composed);
            self.built = Some(composed);
            self.refresh_bound();
            return;
        }
        let root_shell = crate::shell::as_shell(&crate::shell::without_rows(&root_tree));
        let detail_shell = self.detail.as_ref().map(|detail| {
            let tree = crate::reconcile::normalize(&detail.tree);
            crate::shell::prefixed(
                &crate::shell::as_shell(&crate::shell::without_rows(&tree)),
                DETAIL,
            )
        });
        let composed = crate::shell::compose(&root_shell, detail_shell.as_ref());
        // A page while it shows something other than the window itself: a view that shows the
        // resource the window already is (`show(r)` of the app's own root, a scenario that
        // stages it) stands where the window opens, with nothing to go back to.
        let composed = match (&self.root, &self.detail) {
            (Some(root), Some(detail))
                if root.target == detail.target
                    // A page the window's own view already shows (a tree beside what it shows)
                    // is no page after it: nothing stands behind to go back to.
                    || {
                        let mut chosen = None;
                        chosen_item(&root.tree, &mut chosen);
                        chosen == Some(&detail.target)
                    } =>
            {
                let mut shell = composed;
                shell.events.remove(crate::shell::BACK);
                shell
                    .props
                    .insert("can_go_back".to_owned(), Value::Bool(false));
                shell
            }
            _ => composed,
        };
        let composed = crate::shell::header_rules(composed);
        // A window that had nothing at all to list and now lists something shows its columns
        // again, and opens on the screen it would have opened on: the front of a column that was
        // gone is no screen to keep (a notebook staged after its window opened keeps its tree in
        // front on a phone, rather than the first note).
        if !composed.props.contains_key(crate::shell::NOTHING_LISTED)
            && self
                .built
                .as_ref()
                .is_some_and(|built| built.props.contains_key(crate::shell::NOTHING_LISTED))
        {
            self.front = None;
        }
        let composed = match self.front {
            // A shell built anew opens on the screen a new window opens on: the window hands it
            // the one it shows, so the person stays where they are.
            Some(front) => with_front(composed, front),
            None => composed,
        };
        self.reconcile_into_pane(&composed);
        self.built = Some(composed);
        self.select_shown();
        self.refresh_bound();
    }

    /// Bring the pane's widgets up to date with `composed`, reconciled into what they were
    /// built from, or built anew.
    fn reconcile_into_pane(&mut self, composed: &UiNode) {
        let old = self.built.take();
        let reconciler = Rc::clone(&self.reconciler);
        // What an update builds into a window that already composed is arriving (a place opened
        // onto the desk comes in); the window's first tree shows at rest, as its first frame does.
        let _window = ui::motion::laying_out_window(old.is_some());
        if let Some(pane) = self.shell_pane() {
            let mut reconciler = reconciler.borrow_mut();
            match &old {
                Some(old) if old.key == composed.key && old.kind == composed.kind => {
                    if let Some(replaced) = reconciler.update_in(pane.content_mut(), old, composed)
                    {
                        pane.set(replaced);
                    }
                }
                _ => pane.set(reconciler.build(composed)),
            }
        }
    }

    /// Select, in each list of the overview, the row of the view shown after it: the app keeps
    /// no selection beside it. The sidebar's place follows what the content shows the same way:
    /// a place names the resource it shows by the id alone, so the window chooses the one that
    /// names the shown resource, and leaves the app's own choice when none does.
    fn select_shown(&mut self) {
        let shown = self.detail.as_ref().map(|detail| detail.target.id_string());
        if let Some(shell) = self.shell() {
            if let Some(shown) = &shown
                && let Some(sidebar) = shell.sidebar_mut::<ui::widget::NavList>()
            {
                sidebar.select_naming(shown);
            }
            if let Some(list) = shell.overview_list_mut() {
                list.select_presenting(shown.as_deref());
            }
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
            rows.borrow_mut().refresh_bound();
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

    /// Whether the window holds a message for the app it has not sent: a space a layout gave a
    /// view, what the person did, a row asked for. An embedder that only sends when it has
    /// something to say asks this, so a window nothing touches still hands the app what its
    /// frame gave its views.
    #[must_use]
    pub fn owes(&self) -> bool {
        !self.out.is_empty()
            || self
                .rows
                .borrow()
                .values()
                .any(|rows| rows.borrow().wants())
    }

    /// Send the app what the person did since the last frame: one batch.
    pub fn flush(&mut self) {
        // What was typed while the keyboard was on its way to the field a press opened goes
        // there once it arrived.
        self.release_landing();
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
        let wanted: Vec<(Lane, String, crate::rows::Asked)> = self
            .rows
            .borrow()
            .values()
            .filter_map(|rows| rows.borrow_mut().take_wanted())
            .collect();
        for (lane, list, asked) in wanted {
            self.out.push(ToApp::Rows {
                lane,
                list,
                start: u32::try_from(asked.range.start).unwrap_or(u32::MAX),
                end: u32::try_from(asked.range.end).unwrap_or(u32::MAX),
                open: asked.open,
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
        // A control pressed where a field declares its same key leads the person to the field,
        // as the key does: the window's Primary brings them to a list's entry to type the item,
        // rather than making one nobody typed.
        if let UiEvent::Press(key) | UiEvent::Fire(key, _, None) = &event
            && let Some(field) = self.press_leads_a_field(key)
        {
            self.focus_request = Some(field);
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
            UiEvent::EditRich(composed, rich) => match self.edited_rich(&composed, rich) {
                Some(message) => message,
                None => return,
            },
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
        let held = Held::Text {
            base: self.texts.get(composed).cloned().unwrap_or_default(),
            now: text,
        };
        self.bound_text(composed, held)
    }

    /// The bound rich-text control `composed` now holds `rich`.
    fn edited_rich(&mut self, composed: &str, rich: wire::rich::RichText) -> Option<ToApp> {
        let base = match self.bound.borrow().get(composed) {
            Some(Value::Rich(before)) => before.clone(),
            _ => wire::rich::RichText::default(),
        };
        self.bound_text(composed, Held::Rich { base, now: rich })
    }

    /// The bound control `composed` now holds `held`: shown as the person left it, and sent,
    /// unless a press it was typed after has not run yet — then it waits, like every control
    /// that takes typing ([`Self::hold`]).
    fn bound_text(&mut self, composed: &str, held: Held) -> Option<ToApp> {
        let (lane, key) = lane_of(composed);
        // A control that edits no text (a number, a date) sends the value it now holds: how its
        // edits merge, as its component's description says. A control no component built holds
        // text when what it shows is text.
        let edits_text = matches!(held, Held::Rich { .. })
            || match self.reconciler.borrow().adapter().merge_of(composed) {
                Some(merge) => merge == ui::component::Merge::Splice,
                None => match self.bound.borrow().get(composed) {
                    Some(Value::Text(_)) | None => true,
                    Some(Value::Opt(held)) => {
                        matches!(held.as_deref(), Some(Value::Text(_)) | None)
                    }
                    Some(_) => false,
                },
            };
        if !edits_text {
            let Held::Text { now, .. } = held else {
                return None;
            };
            let target = self.target(lane)?;
            return Some(ToApp::Edit {
                lane,
                target,
                key,
                text: now,
            });
        }
        let shown = match &held {
            Held::Text { now, .. } => Value::Text(now.clone()),
            Held::Rich { now, .. } => Value::Rich(now.clone()),
        };
        // Typing after a press the app has not run yet waits for it: the press may open
        // something else into the field the person types into.
        if self.fired > self.took {
            self.hold(composed, lane, held, shown);
            return None;
        }
        match held {
            Held::Text { base, now } => self.splice(composed, &base, now),
            Held::Rich { now, .. } => {
                let target = self.target(lane)?;
                self.bound
                    .borrow_mut()
                    .insert(composed.to_owned(), Value::Rich(now.clone()));
                Some(ToApp::EditRich {
                    lane,
                    target,
                    key,
                    rich: now,
                })
            }
        }
    }

    /// Hold what the person typed in the bound control `composed` of `lane` until the presses
    /// the app has not run yet did, showing `shown` meanwhile: what they typed goes where the
    /// keyboard would have been had the press run at once.
    fn hold(&mut self, composed: &str, lane: Lane, held: Held, shown: Value) {
        let fired = self.fired;
        let target = self.target(lane);
        let same_field =
            |typed: &Typed| typed.key.as_deref() == Some(composed) && typed.target == target;
        // The person moved on to another field meanwhile: what they typed before stays where
        // they typed it.
        if self
            .typed
            .back()
            .is_some_and(|typed| typed.after == fired && typed.key.is_some() && !same_field(typed))
            && let Some(typed) = self.typed.pop_back()
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
                waits_focus: false,
                held,
                sent: None,
            });
        } else if let Some(typed) = self.typed.back_mut() {
            // Every key after the first keeps what the control held before the person began:
            // what they typed is read from there.
            match (&mut typed.held, held) {
                (Held::Text { now, .. }, Held::Text { now: newer, .. }) => now.clone_from(&newer),
                (Held::Rich { now, .. }, Held::Rich { now: newer, .. }) => *now = newer,
                (slot, newer) => *slot = newer,
            }
        }
        // What the field shows is what the person typed, until the app takes it.
        if let Some(Held::Text { now, .. }) = self.typed.back().map(|typed| &typed.held) {
            self.texts.insert(composed.to_owned(), now.clone());
        }
        self.bound.borrow_mut().insert(composed.to_owned(), shown);
    }

    /// The view a person's press opened takes the keyboard in its first text field, and what
    /// they typed after the press, before it ran, goes there: as it would have, had the press
    /// run at once. What they typed before the press, or into a field they put the keyboard in
    /// since, stays that field's, of the resource it showed.
    fn focus_opened(&mut self) {
        fn first_field(node: &UiNode) -> Option<&UiNode> {
            if node.binding.is_some()
                && (node.kind == crate::adapter::kind("TextInput")
                    || node.kind == crate::adapter::kind("RichInput"))
            {
                return Some(node);
            }
            node.children.iter().find_map(first_field)
        }
        // The page a press showed: the item opened after the window's own, or — for a window
        // whose page is its own, a tree beside what it shows — the first field of its content.
        let page = self
            .detail
            .as_ref()
            .and_then(|detail| first_field(&detail.tree))
            .map(|field| (composed_key(Lane::Detail, &field.key), Lane::Detail));
        let (field, lane) = if let Some(page) = page {
            page
        } else {
            let Some(root) = self.root.as_ref() else {
                return;
            };
            let Some(field) = content_nodes(&root.tree).into_iter().find_map(first_field) else {
                return;
            };
            (field.key.clone(), Lane::Root)
        };
        // What was typed with no field focused after the press the app just ran, not after a
        // later one.
        let took = self.took;
        let target = self.target(lane);
        // What the field the keyboard goes to holds: a body's rich text typed into a title is
        // words, and the title takes words.
        let rich = self
            .built
            .as_ref()
            .and_then(|built| node_of_key(built, lane_of(&field).1.as_str()))
            .is_some_and(|node| node.kind == crate::adapter::kind("RichInput"));
        // The fields the typing leaves show what the app has of them again, not the words that
        // went elsewhere.
        let mut left = Vec::new();
        for typed in &mut self.typed {
            if typed.after <= took && typed.follows && !typed.moved {
                if typed.key.is_some() {
                    // Only what the person typed moves, never what the field held before it,
                    // and a line break a body's text ends with does not move into a title.
                    let insert = match &typed.held {
                        Held::Text { base, now } => splice_between(base, now).2,
                        Held::Rich { base, now } => {
                            splice_between(&rich_text_of(base), &rich_text_of(now)).2
                        }
                    };
                    let kept = insert.trim_end_matches('\n').to_owned();
                    typed.held = match &mut typed.held {
                        Held::Rich { now, .. } if rich => Held::Rich {
                            base: wire::rich::RichText::default(),
                            now: rich_spliced_at(now, 0, rich_text_of(now).len(), &kept),
                        },
                        _ => Held::Text {
                            base: String::new(),
                            now: kept,
                        },
                    };
                }
                // Its old value is no part of the field it goes to now: what it holds there is
                // read from that field.
                typed.sent = None;
                typed.moved = true;
                if let Some(old) = typed.key.replace(field.clone())
                    && old != field
                {
                    left.push(old);
                }
                typed.target.clone_from(&target);
            }
        }
        for key in left {
            self.show_bound_again(&key);
        }
        if lane == Lane::Root && self.detail.is_none() {
            self.page_focus = Some(field.clone());
            self.page_focus_tries = 0;
        }
        self.ask_focus(field);
    }

    /// Ask the app to put the keyboard in `field` (its key in the composed tree), and keep
    /// what the person types until it has it that field's ([`Self::hold_for_landing`]).
    fn ask_focus(&mut self, field: String) {
        self.landing = Some(field.clone());
        self.focus_request = Some(field);
    }

    /// What the person typed while the keyboard was on its way to the field a press opened goes
    /// there once the app took the window's request for it.
    fn release_landing(&mut self) {
        if self.landing.is_some() || !self.typed.iter().any(|typed| typed.waits_focus) {
            return;
        }
        for typed in &mut self.typed {
            typed.waits_focus = false;
        }
        self.send_typed();
    }

    /// Hold the text the person typed while the keyboard was on its way to `field`, the field a
    /// press opened, showing it there as they type: it goes when the keyboard arrives.
    fn hold_for_landing(&mut self, field: &str, text: &str) {
        let after = self.fired;
        let (lane, _) = lane_of(field);
        let target = self.target(lane);
        if let Some(typed) = self
            .typed
            .back_mut()
            .filter(|typed| typed.waits_focus && typed.key.as_deref() == Some(field))
        {
            match &mut typed.held {
                Held::Text { now, .. } => now.push_str(text),
                Held::Rich { now, .. } => {
                    *now = rich_spliced_at(now, rich_text_of(now).len(), 0, text);
                }
            }
        } else {
            // What the app has for the field: the text the person's typing goes after, and what
            // its echo is measured against.
            let before = self.bound.borrow().get(field).cloned();
            let (held, shown) = match before {
                Some(Value::Rich(rich)) => (
                    Held::Rich {
                        now: rich_spliced_at(&rich, rich_text_of(&rich).len(), 0, text),
                        base: rich.clone(),
                    },
                    Value::Rich(rich),
                ),
                Some(Value::Text(base)) => (
                    Held::Text {
                        base: base.clone(),
                        now: format!("{base}{text}"),
                    },
                    Value::Text(base),
                ),
                _ => (
                    Held::Text {
                        base: String::new(),
                        now: text.to_owned(),
                    },
                    Value::Text(String::new()),
                ),
            };
            self.typed.push_back(Typed {
                after,
                key: Some(field.to_owned()),
                target,
                follows: true,
                moved: true,
                waits_focus: true,
                held,
                sent: Some(shown),
            });
        }
        // The field shows what the person typed, as it would had the keyboard arrived first.
        match self.typed.back().map(|typed| &typed.held) {
            Some(Held::Text { now, .. }) => {
                self.texts.insert(field.to_owned(), now.clone());
                self.bound
                    .borrow_mut()
                    .insert(field.to_owned(), Value::Text(now.clone()));
            }
            Some(Held::Rich { now, .. }) => {
                self.bound
                    .borrow_mut()
                    .insert(field.to_owned(), Value::Rich(now.clone()));
            }
            None => {}
        }
        self.refresh_bound();
    }

    /// Hold `event` when it types text the window cannot place yet: while the keyboard is on its
    /// way to the field a press opened, into that field; while a press runs and no field has the
    /// keyboard (the press took it), into the field it leaves the keyboard in. Whether it was
    /// held.
    fn hold_typing(&mut self, event: &ui::winit::event::WindowEvent) -> bool {
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
        if key.state != ui::winit::event::ElementState::Pressed {
            return false;
        }
        // The keyboard is on its way to the field a press opened: what the person types
        // meanwhile is that field's, never the one the keyboard is leaving.
        if let Some(field) = self.landing.clone()
            && self.focused_control().as_deref() != Some(field.as_str())
        {
            self.hold_for_landing(&field, text);
            return true;
        }
        if self.fired <= self.took || takes_typing(self.body.as_ref()) {
            return false;
        }
        let fired = self.fired;
        match self
            .typed
            .back_mut()
            .filter(|typed| typed.after == fired && typed.key.is_none())
        {
            Some(typed) => match &mut typed.held {
                Held::Text { now, .. } => now.push_str(text),
                Held::Rich { now, .. } => {
                    *now = rich_spliced_at(now, rich_text_of(now).len(), 0, text);
                }
            },
            None => self.typed.push_back(Typed {
                after: fired,
                key: None,
                target: None,
                follows: true,
                moved: false,
                waits_focus: false,
                held: Held::Text {
                    base: String::new(),
                    now: text.to_owned(),
                },
                sent: None,
            }),
        }
        true
    }

    /// Enter in a field of one line that submits nothing moves the keyboard on to the next field
    /// of its view, as native Notes goes from a note's title to its body. A field with `submit`
    /// keeps Enter: it submits. Whether it moved.
    fn enter_moves_on(
        &mut self,
        event: &ui::winit::event::WindowEvent,
        modifiers: ui::winit::keyboard::ModifiersState,
    ) -> bool {
        use ui::winit::keyboard::{Key, NamedKey};
        let ui::winit::event::WindowEvent::KeyboardInput { event: key, .. } = event else {
            return false;
        };
        if key.state != ui::winit::event::ElementState::Pressed
            || key.logical_key != Key::Named(NamedKey::Enter)
            || self.composing
            || modifiers.shift_key()
            || modifiers.control_key()
            || modifiers.alt_key()
            || modifiers.super_key()
        {
            return false;
        }
        let Some(next) = self
            .focused_field()
            .zip(self.built.as_ref())
            .and_then(|(field, built)| field_after_enter(built, &field))
        else {
            return false;
        };
        // The window's own request, taken once its tree is laid out again.
        self.ask_focus(next);
        self.structure_dirty = true;
        true
    }

    /// The press the app declared for `event` (`key: "Ctrl+Enter"`, `key: "e"`) in this window,
    /// when no text field the keyboard is in uses it: see [`Press`]. A paste, or a key an input
    /// method is composing, is no press of an app's.
    fn declared_shortcut(
        &self,
        event: &ui::winit::event::WindowEvent,
        modifiers: ui::winit::keyboard::ModifiersState,
    ) -> Option<Press> {
        if self.composing {
            return None;
        }
        let ui::winit::event::WindowEvent::KeyboardInput { event: key, .. } = event else {
            return None;
        };
        if key.state != ui::winit::event::ElementState::Pressed || key.repeat {
            return None;
        }
        if key
            .text
            .as_ref()
            .is_some_and(|text| text.chars().count() > 1)
        {
            return None;
        }
        let pressed = ui::platform::Shortcut::pressed(
            &key.logical_key,
            modifiers,
            ui::platform::PlatformConventions::default(),
        )?;
        self.press_where(&pressed)
    }

    /// Where a press of `pressed` leads in the window: the field it brings the person to, the
    /// field that carries it out (the keyboard is already there), or the control that runs it —
    /// on the item the keyboard is on first, then what that stands in, then the window.
    fn press_where(&self, pressed: &ui::platform::Shortcut) -> Option<Press> {
        let built = self.built.as_ref()?;
        let focused = self.focused_control();
        // A key a text field declares is the field's: where the keyboard is already in it, it
        // carries the key out there, on the item the person is on, rather than sending them to
        // the first field of the window that names it.
        if let Some(field) = focused.as_deref().filter(|field| {
            node_at(built, field).is_some_and(|field| text_field(field) && declares(field, pressed))
        }) {
            return Some(if field_takes_a_press(built, field) {
                Press::Commit
            } else {
                Press::Lead(field.to_owned())
            });
        }
        if let Some(field) = focus_field(built, pressed) {
            return Some(Press::Lead(field));
        }
        shortcut_action_where(built, focused.as_deref(), pressed)
            .map(|(key, event)| Press::Run(key, event))
    }

    /// The press the field the keyboard is in takes as its own: Enter, which a field of one line
    /// that submits commits what it holds with.
    fn enter_press(&mut self, ctx: &mut ui::widget::EventCtx<'_>) {
        let enter = ui::synthetic_input::named_key(ui::winit::keyboard::NamedKey::Enter);
        self.body.handle_event(&enter, ctx);
    }

    /// The field a press on the control `key` leads the person to: one whose declared key is
    /// the shortcut that control names, when there is one. See [`focus_field`].
    fn press_leads_a_field(&self, key: &str) -> Option<String> {
        let built = self.built.as_ref()?;
        let declared = shortcut_declared(built, key)?;
        focus_field(built, &declared)
    }

    /// Run the action the app declared the shortcut `key` for, as pressing it does: the event
    /// it runs reaches the app at once, or the keyboard goes to the field the key leads to.
    /// Whether the window offers it. A test's way in.
    pub fn run_shortcut(&mut self, key: &str) -> bool {
        let Some(press) = self.press_where(&ui::platform::Shortcut::from(key)) else {
            return false;
        };
        match press {
            // The window's own request, taken once its tree is laid out again: what the field
            // commits, it commits as the person leaves it.
            Press::Lead(field) => {
                self.focus_request = Some(field);
                self.take_events();
                self.flush();
                true
            }
            // The keyboard is already in the field that declares the key, and the field carries
            // it out through its own press: only the event path does that.
            Press::Commit => true,
            Press::Run(node_key, event) => {
                self.act(UiEvent::Fire(node_key, event, None));
                self.take_events();
                self.flush();
                true
            }
        }
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
        while self.typed.front().is_some_and(|typed| {
            typed.after <= self.took && !(typed.waits_focus && self.landing.is_some())
        }) {
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
            // The field the press showed takes it: the page beside a list, or the page after
            // the window's own, named while the keyboard goes there.
            let Some(field) = self.landing.clone().or_else(|| self.focused_field()) else {
                let typed_text = typed.held.texts().1;
                if !typed_text.is_empty() {
                    let words = Words::Plain(t_args(
                        "tilolang-typing-unplaced",
                        &[("typed", typed_text.as_str().into())],
                    ));
                    self.notice(&words, wire::NoticeKind::Info, false);
                }
                return;
            };
            typed.sent = self.bound.borrow().get(&field).cloned();
            typed.target = self.field_target_for(&field, Some(typed.after));
            typed.key = Some(field);
        }
        let Some(key) = typed.key else {
            return;
        };
        // What the field shows now, when the window did not hold what the person typed: their
        // own typing is no part of what the app has of it.
        if typed.moved && typed.sent.is_none() {
            typed.sent = self.bound.borrow().get(&key).cloned();
        }
        let (lane, field) = lane_of(&key);
        // The resource the field belongs to, when it is the one the person typed into: the item
        // the press before their typing showed, where the window draws the page itself.
        let target = self.field_target_for(&key, Some(typed.after));
        // A field of the page belongs to the item its press showed: it goes there even once the
        // page shows another, and what it showed when the person typed is what they typed into.
        let page = self.page_field(&key);
        let still_shown = typed.target.is_none() || typed.target == target || page;
        // What the field shows now: what the app sent for it since, or what it showed when the
        // person began typing (the window showed their typing meanwhile, which is no part of
        // what it shows now).
        let shown = match typed.sent.take() {
            Some(sent) if !page => sent,
            _ => match &typed.held {
                Held::Text { base, .. } => Value::Text(base.clone()),
                Held::Rich { base, .. } => Value::Rich(base.clone()),
            },
        };
        if still_shown && let Some(target) = target {
            self.send_typed_into_field(&key, lane, field, target, &typed.held, &shown);
            return;
        }
        // The lane moved on to another resource: what they typed goes to the one it was typed
        // into, and the field the lane shows now keeps what it shows.
        let Some(target) = typed.target else {
            return;
        };
        let message = self.typed_elsewhere(&key, lane, field, target, &typed.held, &shown);
        self.out.push(message);
    }

    /// Send what the person typed in `key` of `lane` into the field the lane shows now
    /// (`target`), against what it shows (`shown`).
    fn send_typed_into_field(
        &mut self,
        key: &str,
        lane: Lane,
        field: String,
        target: ResourceRef,
        held: &Held,
        shown: &Value,
    ) {
        let (show_at, delete, insert) = Self::place_of(held, shown);
        match (shown, held) {
            (Value::Rich(before), _) => {
                let rich = rich_spliced_at(before, show_at, delete, &insert);
                self.bound
                    .borrow_mut()
                    .insert(key.to_owned(), Value::Rich(rich.clone()));
                self.out.push(ToApp::EditRich {
                    lane,
                    target,
                    key: field,
                    rich,
                });
            }
            (Value::Text(before), _) => {
                let text = splice_text(before, (show_at, show_at.saturating_add(delete)), &insert);
                if let Some(message) = self.splice(key, before, text) {
                    self.out.push(message);
                }
            }
            (_, Held::Rich { now, .. }) => {
                self.bound
                    .borrow_mut()
                    .insert(key.to_owned(), Value::Rich(now.clone()));
                self.out.push(ToApp::EditRich {
                    lane,
                    target,
                    key: field,
                    rich: now.clone(),
                });
            }
            (_, Held::Text { now, .. }) => self.out.push(ToApp::Edit {
                lane,
                target,
                key: field,
                text: now.clone(),
            }),
        }
    }

    /// What the person typed, where it goes in what the field shows now: a splice of the
    /// shown text and the place the typing keeps from its end.
    fn place_of(held: &Held, shown: &Value) -> (usize, usize, String) {
        let (base_text, now_text) = held.texts();
        let (at, delete, insert) = splice_between(&base_text, &now_text);
        let shown = match shown {
            Value::Text(text) => text.clone(),
            Value::Rich(rich) => rich_text_of(rich),
            _ => base_text.clone(),
        };
        let after = base_text.len().saturating_sub(at.saturating_add(delete));
        let mut start = shown.len().saturating_sub(after.saturating_add(delete));
        while !shown.is_char_boundary(start) {
            start = start.saturating_sub(1);
        }
        (start, delete, insert)
    }

    /// What the person typed into the field of a resource the lane left: an edit of the
    /// resource they typed into.
    fn typed_elsewhere(
        &self,
        key: &str,
        lane: Lane,
        field: String,
        target: ResourceRef,
        held: &Held,
        shown: &Value,
    ) -> ToApp {
        let (show_at, delete, insert) = Self::place_of(held, shown);
        match (held, shown) {
            (Held::Rich { base, .. }, _) => ToApp::EditRich {
                lane,
                target,
                key: field,
                rich: rich_spliced_at(base, show_at, delete, &insert),
            },
            (Held::Text { .. }, Value::Rich(before)) => ToApp::EditRich {
                lane,
                target,
                key: field,
                rich: rich_spliced_at(before, show_at, delete, &insert),
            },
            (Held::Text { .. }, _) => {
                let before = match shown {
                    Value::Text(text) => text.clone(),
                    _ => String::new(),
                };
                let typed_text =
                    splice_text(&before, (show_at, show_at.saturating_add(delete)), &insert);
                let (at, delete, insert) = splice_between(&before, &typed_text);
                ToApp::Splice {
                    lane,
                    target,
                    key: field,
                    seq: self.seqs.get(key).copied().unwrap_or_default(),
                    at: u32::try_from(at).unwrap_or(u32::MAX),
                    delete: u32::try_from(delete).unwrap_or(u32::MAX),
                    insert,
                }
            }
        }
    }

    /// The control the keyboard is in now, by key in the composed tree.
    fn focused_control(&self) -> Option<String> {
        fn focused(widget: &dyn Widget) -> Option<String> {
            if widget.is_focused()
                && let Some(id) = widget.test_id()
            {
                return Some(id.to_owned());
            }
            widget.children().into_iter().find_map(focused)
        }
        focused(self.body.as_ref())
    }

    /// The bound text control the keyboard is in now, by key in the composed tree.
    fn focused_field(&self) -> Option<String> {
        let field = self.focused_control()?;
        matches!(
            self.bound.borrow().get(&field),
            Some(Value::Text(_) | Value::Rich(_))
        )
        .then_some(field)
    }

    /// Whether the keyboard is in a field that takes typing: a text field's keys are its own
    /// until it says it does not use one.
    fn typing(&self) -> bool {
        takes_typing(self.body.as_ref())
    }

    /// The splice of the bound text control `composed` from `before` to `text`, which it now
    /// shows.
    fn splice(&mut self, composed: &str, before: &str, text: String) -> Option<ToApp> {
        let (lane, key) = lane_of(composed);
        // Of the resource the field belongs to: the app applies it there, whatever the lane
        // shows by the time it takes it.
        let target = self.field_target(composed)?;
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

    /// The resource the root view shows.
    #[must_use]
    pub fn root_target(&self) -> Option<&ResourceRef> {
        self.root.as_ref().map(|view| &view.target)
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

    /// Each component of the window's view that did not build or take its props, and why.
    #[must_use]
    pub fn broken_components(&self) -> Vec<String> {
        self.reconciler.borrow().broken()
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
    /// not, such as the names of the workspace's participants.
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

/// The composed shell `shell` showing `pane` in front of a narrow window: the column it opens
/// on, given as the shell's own state, which a shell built anew has none of.
fn with_front(mut shell: UiNode, pane: NavPane) -> UiNode {
    let case = match pane {
        NavPane::Sidebar => "sidebar",
        NavPane::Overview => "overview",
        NavPane::Content => "content",
    };
    shell.props.insert("pane".to_owned(), Value::text(case));
    shell
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

/// The field Enter in the field `key` of the composed tree `tree` moves the keyboard on to: the
/// next field of its view, when `key` is a text field of one line that submits nothing.
fn field_after_enter(tree: &UiNode, key: &str) -> Option<String> {
    fn in_order<'a>(node: &'a UiNode, out: &mut Vec<&'a UiNode>) {
        out.push(node);
        for child in &node.children {
            in_order(child, out);
        }
    }
    // A bound control whose binding is text takes typing, whatever component it is: the
    // document's body is a block editor, and a field after the title moves to it all the same.
    let takes_typing = |node: &UiNode| {
        node.binding.is_some()
            && crate::adapter::description_of(node).is_some_and(|description| {
                description.bindings.iter().any(|binding| {
                    matches!(
                        binding.ty,
                        ui::component::Type::Text | ui::component::Type::RichText
                    )
                })
            })
    };
    let mut nodes = Vec::new();
    in_order(tree, &mut nodes);
    let at = nodes.iter().position(|node| node.key == key)?;
    let field = nodes.get(at)?;
    // Given `lines`, the field wraps and Enter breaks its line (a document's title still ends
    // at Enter, and wraps only to fit).
    if !one_line(field) || field.events.contains_key("submit") {
        return None;
    }
    let lane = lane_of(key).0;
    nodes
        .get(at.saturating_add(1)..)?
        .iter()
        .find(|node| takes_typing(node) && lane_of(&node.key).0 == lane)
        .map(|node| node.key.clone())
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
/// composed tree (each of `node`'s keys carrying `prefix` there), reporting whether any control
/// was given a value other than the one it showed.
pub fn show_bound<S: std::hash::BuildHasher>(
    node: &UiNode,
    prefix: &str,
    bound: &HashMap<String, Value, S>,
    reconciler: &Reconciler,
) -> bool {
    let mut changed = false;
    if node.binding.is_some() {
        let key = format!("{prefix}{}", node.key);
        if let Some(value) = bound.get(&key) {
            let flag = node.kind == "checkbox"
                || node.kind == "toggle"
                || node.kind == crate::adapter::kind("ListRow");
            changed |= match value {
                Value::Bool(checked) => reconciler.set_flag(&key, *checked),
                Value::Int(number) if flag => reconciler.set_flag(&key, *number != 0),
                Value::Int(number) => reconciler.set_count(&key, *number),
                Value::Float(number) => reconciler.set_number(&key, *number),
                Value::Rich(rich) => reconciler.set_rich(&key, rich),
                other => reconciler.set_text(&key, &crate::reconcile::display(other)),
            };
        }
    }
    for child in &node.children {
        changed |= show_bound(child, prefix, bound, reconciler);
    }
    changed
}

/// Where a press of a key an app declared leads in a window.
enum Press {
    /// The keyboard goes to the text field the key brings the person to: the entry an app wrote
    /// for its primary action, rather than an item nobody typed.
    Lead(String),
    /// The key is the one the field the keyboard is already in declares: the field carries it
    /// out itself, as its own press does.
    Commit,
    /// The control that declares the action runs it, on the event it takes.
    Run(String, String),
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

/// The node key and event to fire for the control that answers a press, matched the way the
/// toolkit matches every shortcut ([`ui::platform::Shortcut::same_press`]): one key, in one
/// spelling, with the modifiers it names, and a command modifier that is the platform's own. So
/// an app's `key: "Ctrl+Enter"` is Command+Enter on a Mac and `key: "e"` is the unshifted E on
/// every layout (a `Button`'s `press`, a `MenuItem`'s `choose`, as the action's `key:` reaches
/// them through `std.header`). `None` when no control declares it.
///
/// The control `focused` — the one the keyboard is in — is where the answer starts: the actions
/// of the item the person is on lead, then those of what it stands in, out to the window's. With
/// no control to start from, the content after the overview's list leads (the resource the
/// person selected), then the whole shell.
fn shortcut_action_where(
    node: &UiNode,
    focused: Option<&str>,
    pressed: &ui::platform::Shortcut,
) -> Option<(String, String)> {
    if let Some(path) = focused.and_then(|key| path_to(node, key)) {
        // Out from the control the keyboard is in: what it declares itself, then what else
        // stands beside the way on at each level (the item's own controls, the header of the page
        // it is on), then what the item stands in.
        for (at, stands) in path.iter().enumerate().rev() {
            if let Some(found) = declared_action(stands, pressed) {
                return Some(found);
            }
            let on_the_way = path.get(at.saturating_add(1)).map(|next| next.key.as_str());
            for child in &stands.children {
                if Some(child.key.as_str()) == on_the_way {
                    continue;
                }
                if let Some(found) = declared_within(child, pressed) {
                    return Some(found);
                }
            }
        }
    }
    for child in crate::shell::slot_children(node, "content") {
        if let Some(found) = declared_within(child, pressed) {
            return Some(found);
        }
    }
    declared_within(node, pressed)
}

/// The control `node` itself is for the shortcut `pressed`: its node key and the event that
/// runs its action, by its kind (a button's `press`, a menu item's `choose`).
fn declared_action(node: &UiNode, pressed: &ui::platform::Shortcut) -> Option<(String, String)> {
    if !declares(node, pressed) {
        return None;
    }
    ["press", "choose"]
        .into_iter()
        .find(|event| node.events.contains_key(*event))
        .map(|event| (node.key.clone(), event.to_owned()))
}

/// Whether some control of the tree `node` an app built while the actions `before` were
/// published would take the other path now that the window offers the actions `after`: an event
/// that runs an action the app publishes is bound to the action itself, and one that runs an
/// action it does not publish is the view's own.
fn rebinding(
    before: &crate::adapter::Published,
    after: &crate::adapter::Adapter,
    node: &UiNode,
) -> bool {
    node.events.values().any(|event| {
        let Some((resource, action)) = event.invokes.as_ref() else {
            return false;
        };
        before.holds(resource, action) != after.holds(resource, action)
    }) || node
        .children
        .iter()
        .any(|child| rebinding(before, after, child))
}

/// The first control in `node`'s tree, itself included, that answers the shortcut `pressed`.
fn declared_within(node: &UiNode, pressed: &ui::platform::Shortcut) -> Option<(String, String)> {
    declared_action(node, pressed).or_else(|| {
        node.children
            .iter()
            .find_map(|child| declared_within(child, pressed))
    })
}

/// Whether `node` declares `pressed` as its key, in its `key` prop (`key: "Ctrl+Enter"`).
fn declares(node: &UiNode, pressed: &ui::platform::Shortcut) -> bool {
    matches!(
        node.props.get("key"),
        Some(Value::Text(key))
            if ui::platform::Shortcut::from(key.as_str()).same_press(pressed)
    )
}

/// The nodes from `node` down to the one keyed `key`, the window's root first.
fn path_to<'a>(node: &'a UiNode, key: &str) -> Option<Vec<&'a UiNode>> {
    if node.key == key {
        return Some(vec![node]);
    }
    node.children.iter().find_map(|child| {
        let mut path = path_to(child, key)?;
        path.insert(0, node);
        Some(path)
    })
}

/// Whether `node` is a text field: what a person types in.
fn text_field(node: &UiNode) -> bool {
    node.kind == crate::adapter::kind("TextInput") || node.kind == crate::adapter::kind("RichInput")
}

/// Whether `node` is a text field of one line: Enter ends its text rather than breaking a line.
fn one_line(node: &UiNode) -> bool {
    node.kind == crate::adapter::kind("TextInput")
        && matches!(
            node.props.get("lines"),
            None | Some(Value::Unit | Value::Opt(None))
        )
}

/// Whether the text field `key` of the tree `node` takes a press of its own: a field of one line
/// that submits what it holds, so the key it declares commits the item typed there.
fn field_takes_a_press(node: &UiNode, key: &str) -> bool {
    node_at(node, key).is_some_and(|field| one_line(field) && field.events.contains_key("submit"))
}

/// The node of the tree `node` keyed `key`.
fn node_at<'a>(node: &'a UiNode, key: &str) -> Option<&'a UiNode> {
    if node.key == key {
        return Some(node);
    }
    node.children.iter().find_map(|child| node_at(child, key))
}

/// Whether `widget`, or one of the children it holds, has the keyboard and takes typing.
fn takes_typing(widget: &dyn Widget) -> bool {
    (widget.is_focused() && widget.ime_content().is_some())
        || widget.children().into_iter().any(takes_typing)
}

/// The node key of the text field in `node`'s tree that declares the press `pressed`: the entry
/// at the end of a list, where the app's key puts the person rather than making an empty item.
/// It answers before any control: a header or bar button that names the same key leads the
/// person to the entry the app wrote for it.
fn focus_field(node: &UiNode, pressed: &ui::platform::Shortcut) -> Option<String> {
    if text_field(node) && declares(node, pressed) {
        return Some(node.key.clone());
    }
    node.children
        .iter()
        .find_map(|child| focus_field(child, pressed))
}

/// The shortcut the control `key` declares in its `key` prop, as pressing it names one: a
/// button's press or a menu row's choose.
fn shortcut_declared(node: &UiNode, key: &str) -> Option<ui::platform::Shortcut> {
    let control = node.kind == crate::adapter::kind("Button")
        || node.kind == crate::adapter::kind("MenuItem");
    if control
        && node.key == key
        && let Some(Value::Text(declared)) = node.props.get("key")
    {
        return Some(ui::platform::Shortcut::from(declared.as_str()));
    }
    node.children
        .iter()
        .find_map(|child| shortcut_declared(child, key))
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

/// The event a view's window frame declares where its search writes: what is typed in the
/// window's bar narrows what the frame lists, as a place chosen in its sidebar does.
const SEARCH: &str = "search";

impl AppWindow {
    /// Whether the frame the views drew says where the window's search goes.
    fn searches(&self) -> bool {
        self.composed()
            .is_some_and(|shell| shell.events.contains_key(SEARCH))
    }

    /// How many rows the window's list shows now: the rows of the first list its views show,
    /// a tree's rows counting as a list's do.
    fn shown_rows(&self) -> Option<usize> {
        fn first_list(node: &UiNode) -> Option<&str> {
            if tilo_render_protocol::patch::is_virtual_list(&node.kind) {
                return Some(&node.key);
            }
            node.children.iter().find_map(first_list)
        }
        let key = first_list(self.composed()?)?;
        let rows = self.rows.borrow().get(key).map(Rc::clone)?;
        let len = ui::component::RowSource::len(&*rows.borrow());
        Some(len)
    }

    /// Whether the window's search narrows what the frame lists: the frame says where it goes.
    /// A document standing in the window is found in as well, so the one search does both.
    fn offers_search(&self) -> bool {
        self.searches()
    }

    /// Type `text` into the window's search: the frame narrows what it lists, as a place
    /// chosen in the sidebar narrows it. Closing the search widens it again.
    fn narrow_to(&mut self, text: &str) {
        let Some(shell) = self.composed() else {
            return;
        };
        if !shell.events.contains_key(SEARCH) {
            return;
        }
        let (lane, key) = lane_of(&shell.key);
        if text.is_empty() {
            self.search_total = None;
        } else if self.search_total.is_none() {
            // The rows the list had before anything was typed: what the search counts of.
            self.search_total = self.shown_rows();
        }
        self.out.push(ToApp::Fire {
            lane,
            key,
            event: SEARCH.to_owned(),
            value: Some(Value::text(text)),
        });
    }
}

/// The window's search narrows what the views list where their frame declares it: the app
/// hears the text as the frame's `search` event and narrows its own query, so the list
/// beside the person narrows in place, found by the same search the store answers every
/// text field from.
impl ui::widget::Queryable for AppWindow {
    fn set_query_text(&mut self, text: &str) {
        self.narrow_to(text);
    }

    fn query_closed(&mut self) {
        self.narrow_to("");
    }

    /// The rows the list shows now, of the rows it held before anything was typed. The app
    /// answers a search a frame after it hears it, so the window reads the count off the
    /// frame the app sent, again each time the search asks.
    fn query_count(&self) -> Option<ui::widget::FilterCount> {
        let shown = self.shown_rows()?;
        Some(ui::widget::FilterCount::new(
            shown,
            self.search_total.unwrap_or(shown).max(shown),
        ))
    }
}

impl ui::widget::Decorator for AppWindow {
    type Inner = dyn Widget;

    // The window is the publisher of the app's actions and the one that answers choosing one
    // ([`crate::catalogue`]), and it answers for the views' search where their frame says
    // where it goes; everything else is the body's and forwarded to it.
    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::widget::Queryable>() && self.offers_search() {
            request.provide::<dyn ui::widget::Queryable>(self);
            return;
        }
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::host_operation::ResourceOperations>() {
            request.provide::<dyn ui::host_operation::ResourceOperations>(self);
            return;
        }
        self.body.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::widget::Queryable>() && self.offers_search() {
            request.provide::<dyn ui::widget::Queryable>(self);
            return;
        }
        if request.wants::<dyn ui::resources::PublishesResources>() {
            request.provide::<dyn ui::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::host_operation::ResourceOperations>() {
            request.provide::<dyn ui::host_operation::ResourceOperations>(self);
            return;
        }
        self.body.provide_mut(request);
    }

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
        // Where the person is stays the window's across the shell being built anew: the column
        // the narrow stack has in front, as they left it (Back, a place, a row).
        if let Some(shell) = self.shell() {
            self.front = Some(shell.front_pane());
        }
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
        use ui::winit::event::WindowEvent;
        // A press in the window is the person putting the keyboard where they chose: a keyboard
        // still on its way to the field a press opened stays where it was.
        if matches!(
            event,
            WindowEvent::MouseInput {
                state: ui::winit::event::ElementState::Pressed,
                ..
            }
        ) || matches!(event, WindowEvent::Touch(touch) if touch.phase == ui::winit::event::TouchPhase::Started)
        {
            self.landing = None;
            self.focus_request = None;
        }
        // What the person typed while the keyboard was on its way to the field a press opened
        // goes there first, before the input this event brings.
        self.release_landing();
        // Focus moves on a key, a press or a touch, never on a pointer that only moves.
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
        if self.hold_typing(event) || self.enter_moves_on(event, ctx.modifiers.state()) {
            *ctx.handled = true;
            return;
        }
        // A field the keyboard is in takes the keys it uses, and says which they are by taking
        // them: the rest are the app's.
        let typing = self.typing();
        if typing {
            self.body.handle_event(event, ctx);
            if *ctx.handled {
                return;
            }
        }
        let Some(press) = self.declared_shortcut(event, ctx.modifiers.state()) else {
            if !typing {
                self.body.handle_event(event, ctx);
            }
            return;
        };
        match press {
            // The field the keyboard is in carries the key out as its own press does: Enter,
            // which submits what it holds there.
            Press::Commit => {
                self.enter_press(ctx);
            }
            // The keyboard goes to the field the key leads to, which takes it from there.
            Press::Lead(field) => {
                self.focus_request = Some(field);
                self.take_events();
            }
            // The control that declares the action runs it.
            Press::Run(node_key, event) => {
                self.act(UiEvent::Fire(node_key, event, None));
                self.take_events();
            }
        }
        *ctx.handled = true;
    }

    fn take_focus_request(&mut self) -> Option<String> {
        let request = self
            .focus_request
            .take()
            .or_else(|| self.body.take_focus_request());
        // The app places the keyboard now: what the person typed while it was on its way is
        // the field's it is arriving in.
        if request.is_some() {
            self.landing = None;
        }
        request
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
#[path = "window_tests.rs"]
mod tests;
