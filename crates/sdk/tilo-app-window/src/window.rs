//! One app window as the app window holds it. The window keeps a copy of each of its app's two views
//! (the root, and the view of the resource `open(r)` or a row went to), as the app sends them over
//! the patch protocol, and shows them as the columns of one shell ([`crate::shell`]): the root's
//! sidebar and overview beside the content and header of the view after it. On a narrow screen the
//! columns become push navigation with Back, by the shell's rules. Everything in the shell is the
//! views' own, built through the one adapter: the window adds no frame, header or list. What holds
//! for every window is the platform's: the overview's selection is the view shown after it, a
//! notice of a destructive action offers Undo, and the Ask bar is trusted chrome the app cannot
//! draw. What the person does is sent back as it happens, at most one batch a frame: a component's
//! event by its key, an edit through a binding as a splice of what the control showed (the
//! toolkit's field shows the typing at once), and the rows a list shows now.

use std::cell::RefCell;
mod keys;
mod page;
use keys::{
    Press, declares, field_after_enter, field_takes_a_press, focus_field, level_of, node_at,
    rect_of, shortcut_action_where, shortcut_declared, takes_typing, text_field,
};
mod moved;
pub(crate) mod rebind;

use std::collections::{BTreeMap, HashMap, HashSet};
use std::rc::Rc;

use page::{chosen_item, content_nodes, node_of_key};
use tilo_render_protocol::patch::validate::{PatchLimits, Validator};
use tilo_render_protocol::patch::{self as wire, Answer, Level, ToApp, ToUi, Transport, Words};
use ui::floating_surface::{
    FloatingRole, FloatingSource, FloatingSurfaceHostMessage, FloatingSurfacePresentation,
    FloatingSurfaceRequest, protocol_rect,
};
use ui::tokens::{MotionSource, Rhythm};
use ui::widget::arrange::{Fit, Part, Stack};
use ui::widget::toast::ToastKind;
use ui::widget::{
    Absent, Element, NavPane, NavShell, PermissionPrompt, PictureInPictureAsk, PromptChoice,
    SidebarPlacement, Widget,
};

use crate::i18n::{t, t_args};
use crate::pane::Pane;
use crate::reconcile::{EventSink, Reconciler, UiEvent};
use crate::rows::{BoundValues, RemoteRows};
use crate::shell::{DETAIL, NOTHING_LISTED};
use ui::call_pictures::CallPictures;
use ui::widget::{CallSource, call_slot};

use crate::{ResourceRef, UiNode, Value};

/// The tile's own name for a picture's source, as the protocol carries it.
fn tile_source(source: wire::CallPictureSource) -> CallSource {
    match source {
        wire::CallPictureSource::Camera => CallSource::Camera,
        wire::CallPictureSource::Screen => CallSource::Screen,
    }
}

/// Every virtual list of the window's views, by the key of its node in the window's tree.
pub type Registry = Rc<RefCell<HashMap<String, Rc<RefCell<RemoteRows>>>>>;

/// A list whose rows the window keeps: its key in the window's tree, the level and prefix of the
/// view it is in, the kind of component a row is, and the words of the heading over its rows where
/// one heads them.
struct Rows {
    key: String,
    level: Level,
    prefix: &'static str,
    list: String,
    item: String,
    heading: Option<String>,
}

impl Rows {
    fn new(
        prefix: &'static str,
        level: Level,
        list: &str,
        item: String,
        heading: Option<String>,
    ) -> Self {
        Self {
            key: format!("{prefix}{list}"),
            level,
            prefix,
            list: list.to_owned(),
            item,
            heading,
        }
    }
}

/// One of the window's two views, as the app last sent it.
struct View {
    target: ResourceRef,
    /// Its tree, without the rows of its virtual lists.
    tree: UiNode,
    /// The view declares `protect capture`: a capture or a stream leaves it out.
    capture_protected: bool,
}

/// What a bound control holds, as the person's typing left it: the text it showed when they began
/// and now, or the rich text. Every bound control that takes typing reports in one of these, so
/// what the person types while a press runs waits for it whichever kind of control the keyboard is
/// in.
#[derive(Clone, Debug)]
enum Held {
    Text {
        base: String,
        now: String,
    },
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

fn rich_text_of(rich: &wire::rich::RichText) -> String {
    rich.blocks
        .iter()
        .map(|block| block.text.as_str())
        .collect::<Vec<_>>()
        .join("\n")
}

/// `rich` with `delete` bytes at byte `at` of its text replaced by `insert` (paragraphs a line
/// each, as [`rich_text_of`] reads them): the change lands in the paragraph the caret is in, and
/// every mark around it moves with the text it marks. Typing is characters, never a paragraph
/// break: Enter's key text is a control character, which is never held.
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

fn splice_text(text: &str, (at, end): (usize, usize), insert: &str) -> String {
    let mut out = text.to_owned();
    out.replace_range(at..end, insert);
    out
}

/// What the person typed after a press the app has not run yet: held, not sent, until it did. It
/// goes where the keyboard would have been had the press run at once: typed where the keyboard was
/// when they pressed, or with no field focused, it follows the press into the field of what it
/// opened; typed into a field they put the keyboard in since, it is that field's, of the resource
/// it showed.
struct Typed {
    /// How many presses the window had sent when the person typed it: it waits until the app ran
    /// them.
    after: u64,
    /// The field, by key in the composed tree: none while no field had the keyboard (the press took
    /// it).
    key: Option<String>,
    /// The resource the field's view showed when the person typed into it: none while no field had
    /// the keyboard.
    target: Option<ResourceRef>,
    /// The field had the keyboard before the press: the person typed after the press where the
    /// keyboard was, not where they put it since.
    follows: bool,
    /// It followed the press into the field the press opened: it goes at that field's end, after
    /// whatever went there before it.
    moved: bool,
    /// It is typed into the field a press opened while the window's request for the keyboard to go
    /// there was still on its way ([`AppWindow::hold_for_landing`]): it goes once the app took that
    /// request.
    waits_focus: bool,
    held: Held,
    /// What the app sent for the field since (it showed something else meanwhile, such as the note
    /// the press opened).
    sent: Option<Value>,
}

fn part(element: Element, fit: Fit) -> Part {
    let mut part = Part::new(element);
    part.fit = fit;
    part
}

/// What brings a column to the front: a click or a tap slides it in; a key, an agent's action or
/// nothing the person just did moves it without.
fn by_input() -> MotionSource {
    ui::motion::recent_input(ui::time::now()).unwrap_or(MotionSource::Action)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shape {
    Window,
    /// A part of a shell ([`AppWindow::part`]): the root view as it is.
    Part,
}

/// The floating window the shell placed for the picture a window floats: its window and the
/// content inside it, on the shell.
#[derive(Clone, Copy, Debug)]
struct PipWindow {
    rect: ui::kurbo::Rect,
    content_rect: ui::kurbo::Rect,
}

/// Where the person's keys stand in a window, between its rebuilds.
#[derive(Clone, Copy, Debug, Default)]
struct Keys {
    /// A key, a press or a touch arrived since the window last looked where the keyboard focus is.
    input: bool,
    /// The keyboard is asked to leave the field it is in, at the next rebuild.
    blur_request: bool,
    /// Whether an input method composes in the focused control, as last sent.
    composing: bool,
}

pub struct AppWindow {
    transport: Box<dyn Transport>,
    limits: PatchLimits,
    root: Option<View>,
    detail: Option<View>,
    /// Builds the composed shell and reconciles it, reporting what its controls do.
    reconciler: Rc<RefCell<Reconciler>>,
    events: EventSink,
    built: Option<UiNode>,
    rows: Registry,
    /// What each bound control shows, as the app last sent it, by key in the composed tree.
    bound: BoundValues,
    /// The text of each bound text control as the app has it, splices sent included: what the next
    /// splice is against.
    texts: HashMap<String, String>,
    /// The last splice of each bound control sent, by key in the composed tree.
    seqs: HashMap<String, u64>,
    body: Element,
    answers: Rc<RefCell<Vec<Answer>>>,
    ask: Option<wire::Ask>,
    /// The form shown, if one is: an action waiting for its arguments.
    form: Option<crate::form::Shown>,
    out: Vec<ToApp>,
    structure_dirty: bool,
    /// Why the window shows the platform's error state: a batch it refused.
    failed: Option<String>,
    /// Where the person's keys stand: input since the last look, a blur asked, a composition.
    keys: Keys,
    /// The bound control the keyboard focus was in, as last sent, by key in the composed tree.
    focus: Option<String>,
    /// The record and field the control holding the keyboard edits, by the control's key in the
    /// composed tree, as last seen: when the same editor edits another record (a deck's next
    /// slide), the person has left the field they were typing in.
    focus_binding: Option<(String, (String, String))>,
    /// The modifier keys held now, as the last window event said.
    held: wire::Held,
    /// The modifier keys held, as last sent with a press.
    held_sent: wire::Held,
    /// How often the app asked for the window to come forward since the window last did.
    activations: u32,
    /// The page this window shows, the same one its views read their events from: the app's
    /// page calls are asked here and answered on the next frame.
    page: crate::page_runner::Page,
    /// The app's page request each page action waits for the answer of, by the action.
    page_asks: BTreeMap<u32, u32>,
    /// The person's picker, which answers a page's file input: the host's chooser is the one
    /// place a person chooses a file, and a window the host gave none answers no chooser.
    page_picker: Option<std::sync::Arc<dyn PagePicker>>,
    /// The task runs a change mark asked to show ([`ToUi::ShowRun`]), until the window takes
    /// them ([`Self::take_shown_runs`]).
    shown_runs: Vec<String>,
    /// How many presses the window sent the app (`ToApp::Fire`, `ToApp::Invoke`), and how many of
    /// them the app said it ran (`ToUi::Took`).
    fired: u64,
    took: u64,
    typed: std::collections::VecDeque<Typed>,
    /// How many presses the window had sent when the person last pointed (a click, a touch): what
    /// they type after a press they pointed after goes where they put the keyboard, not where it
    /// was at the press.
    pointed_after: Option<u64>,
    /// The control to take the keyboard once the window's tree is laid out, by key in the composed
    /// tree ([`Widget::take_focus_request`]).
    focus_request: Option<String>,
    /// The field a press opened, the window asked the keyboard for and has not heard back of: what
    /// the person types meanwhile is that field's, not the one the keyboard is leaving
    /// ([`Self::hold_for_landing`]). In the composed tree.
    landing: Option<String>,
    /// The item the window's own view shows in place (a tree beside what it shows): what the fields
    /// of its content belong to.
    shown_in_place: Option<ResourceRef>,
    /// What the row of the item the window's own view shows as its page names it: what the window
    /// is named by while it shows one ([`Self::title`]).
    page_title: Option<String>,
    /// The field of the page the window draws for the item a press showed, until the keyboard is in
    /// it: the page is built again when the item changes, and the keyboard goes with it.
    page_focus: Option<String>,
    /// How many times the window asked for it, so a field that never takes the keyboard does not
    /// have the page built again on every frame.
    page_focus_tries: u8,
    /// What the app last said each bound control holds, by key: what the window shows again when
    /// the person's typing goes elsewhere.
    app_bound: HashMap<String, Value>,
    /// The item each press showed the window's own view (by the press's count): what the person
    /// typed after that press belongs to it, even once a later press showed another.
    shows_by_press: BTreeMap<u64, ResourceRef>,
    /// How many presses the window had sent when the app last opened a view for the person's input,
    /// until the app says it ran them (`ToUi::Took`).
    opened: Option<u64>,
    /// One `rem` in logical pixels under the theme the window was last laid out in.
    rem: f32,
    /// The space last told the app each level's view has, in rem. The level's own root is the key
    /// `""`; the view calls that read their space are the keys the app declared (`ToUi::Spaces`).
    root_space: HashMap<String, (f32, f32, f32)>,
    detail_space: HashMap<String, (f32, f32, f32)>,
    /// The layout node of each keyed widget of the body, as its last build or remount made them,
    /// and the body's own node: what the room each view call is offered is read from
    /// ([`ui::widget::offered_room`]).
    laid_nodes: Option<(ui::taffy::NodeId, HashMap<String, ui::taffy::NodeId>)>,
    /// The view calls of each level that read the space layout gives them, by key in that level's
    /// tree, as the app declared them: what the window measures after every layout.
    root_readers: Vec<String>,
    detail_readers: Vec<String>,
    /// The column a narrow window keeps in front: what a person's tap or the app's open brought
    /// forward, held while the shell is built anew. A shell built from the views starts on the
    /// screen a new window opens on; the window remembers where the person is, so a patch that
    /// changes the views' keys does not send them back.
    front: Option<NavPane>,
    /// The column a one-column window owes the front while its body holds nothing: the list the
    /// person chooses from (the overview beside the body, else the sidebar). It is taken once a
    /// layout says the window shows one column at a time, so a person who looks at the empty body
    /// anyway keeps it.
    front_chooser: Option<NavPane>,
    /// Where the shell stood its sidebar at its last layout: a screen of its own when the window
    /// shows one column at a time, and no bar control of the body's there.
    standing: SidebarPlacement,
    /// The window owes its body built again for the `SidebarPlacement` it now stands at: what an
    /// empty state offers follows the width ([`Self::standing`]).
    compose_at: Option<SidebarPlacement>,
    /// The floating window the shell stood for the picture this window floats, where it stands:
    /// what the picture is drawn into while it floats, and what the surface is clipped to.
    pip_window: Option<PipWindow>,
    /// The source region the shell was last told the floating picture stands at: it moves one
    /// message at a time, and a picture that did not move is not told again.
    pip_region_told: Option<ui::kurbo::Rect>,
    /// A part of a shell rather than a window ([`Self::part`]): the root view as it is, with no
    /// window shell around it.
    shape: Shape,
    /// What the app said a part has no window to show, until its embedder takes it.
    notices: Vec<(wire::NoticeKind, String)>,
    /// The actions the app offers the person at this window, as their command palette lists them
    /// ([`ToUi::Actions`]), with the surface its resources publish.
    pub(crate) offered: crate::catalogue::Offered,
    /// The app whose window this is, as its package names it (`so.tilo.video`): what a floating
    /// surface it asks for names as the app it mirrors.
    app_id: String,
    /// The app's commands the person moved to other keys, as the window answers their presses.
    moved: moved::MovedCommands,
    /// The moved commands' keys as the window's controls were last drawn with them.
    drawn: moved::Relabels,
    /// The places a playback plays, as the app sent them ([`ToUi::Outputs`]): the window process
    /// hands them to the host that plays them.
    outputs: Option<Vec<u8>>,
    /// How many rows the frame's list showed before the search typed anything: what the search's
    /// counter counts them of.
    search_total: Option<usize>,
    /// Whether the person's changes are saved, as the window's bar says it ([`ToUi::Kept`]).
    save: ui::SaveStatusSurface,
    /// What the app's connection heard of the people at the app ([`ToUi::Heard`]), until the
    /// window's toolkit takes it ([`Self::take_heard`]).
    heard: Vec<ui::component_lifecycle::Heard>,
    /// Whether the window shows the history of what it shows, as the app last said
    /// ([`ToUi::History`]): the window bar's History control stands open while it does.
    history: ui::widget::HistoryControl,
    /// The system's call pictures this window shows, decoded where it draws
    /// ([`ToUi::CallPictures`]): each kept at the address its tile reads
    /// ([`ui::live_pictures`]).
    call_pictures: CallPictures,
    /// The call tiles this window shows now, by live slot: a slot that leaves is forgotten, so
    /// a tile whose person stopped showing draws their face again.
    call_shown: std::collections::BTreeSet<String>,
    /// Counts the frames published for the call's slots: a frame newer than the one a tile kept
    /// is decoded afresh.
    call_generation: u64,
}

impl AppWindow {
    /// A part of a shell (its top bar, a dock) receiving its views over `transport`: the root
    /// view's tree as it is, over the whole area the part is given, with no window shell, header or
    /// background. The view fills the area, so what it places at the area's edges (a dock at the
    /// foot) is there however little it shows between them; what it does not draw stays clear, so
    /// what lies under the part shows through, and what the view opens (a menu) has room.
    #[must_use]
    pub fn part(transport: Box<dyn Transport>) -> Self {
        let mut window = Self::new(transport, Vec::new());
        window.shape = Shape::Part;
        window.body = Box::new(
            Stack::stack()
                .fill_width()
                .fill_height()
                .push_part(part(
                    Box::new(Pane::new(Box::new(Absent::new()))),
                    Fit::fill(1.0),
                ))
                .push_part(part(Box::new(Absent::new()), Fit::hug()))
                .push_part(part(Box::new(Absent::new()), Fit::hug())),
        );
        window
    }

    /// A window receiving its app's views over `transport`, showing `beside` after its shell and
    /// Ask bar (an embedder's endpoints, which take no room).
    #[must_use]
    pub fn new(transport: Box<dyn Transport>, beside: Vec<Element>) -> Self {
        let mut body = Stack::stack()
            .fill_width()
            .fill_height()
            .gap(Rhythm::Tight)
            .push_part(part(
                Box::new(Pane::new(Box::new(NavShell::new()))),
                Fit::fill(1.0),
            ))
            .push_part(part(Box::new(Absent::new()), Fit::hug()))
            // The form of an action waiting for its arguments: out of flow, over the window.
            .push_part(part(Box::new(Absent::new()), Fit::hug()));
        for element in beside {
            body = body.push_part(part(element, Fit::hug()));
        }
        let events: EventSink = Rc::default();
        let rows: Registry = Rc::default();
        let reconciler = {
            // Weakly: the rows' own readers share this reconciler, and a strong hold would keep the
            // rows alive after the window closes.
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
        let reconciler = Rc::new(RefCell::new(reconciler));
        let page = reconciler.borrow().page_handle();
        Self {
            transport,
            limits: PatchLimits::default(),
            root: None,
            detail: None,
            reconciler,
            page,
            pip_window: None,
            pip_region_told: None,
            page_asks: BTreeMap::new(),
            page_picker: None,
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
            keys: Keys::default(),
            focus: None,
            focus_binding: None,
            held: wire::Held::default(),
            held_sent: wire::Held::default(),
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
            root_space: HashMap::new(),
            detail_space: HashMap::new(),
            laid_nodes: None,
            root_readers: Vec::new(),
            detail_readers: Vec::new(),
            front: None,
            front_chooser: None,
            standing: SidebarPlacement::None,
            compose_at: None,
            shape: Shape::Window,
            notices: Vec::new(),
            offered: crate::catalogue::Offered::default(),
            app_id: String::new(),
            moved: moved::MovedCommands::default(),
            drawn: Vec::new(),
            outputs: None,
            search_total: None,
            save: ui::SaveStatusSurface::new(),
            heard: Vec::new(),
            history: ui::widget::HistoryControl::Closed,
            call_pictures: CallPictures::detached(),
            call_shown: std::collections::BTreeSet::new(),
            call_generation: 0,
        }
    }

    pub fn take_notices(&mut self) -> Vec<(wire::NoticeKind, String)> {
        std::mem::take(&mut self.notices)
    }

    /// The actions the app offers the person at this window, as its command palette lists them
    /// ([`ToUi::Actions`]): labels, keys, icons and what they act on, for the window to offer them
    /// the way it offers its own.
    #[must_use]
    pub fn actions(&self) -> &[tilo_actions::ActionDescriptor] {
        self.offered.actions()
    }

    /// The app whose window this is, as its package names it: a picture-in-picture surface the
    /// window asks for mirrors that app's player.
    pub fn set_app_id(&mut self, app_id: impl Into<String>) {
        self.app_id = app_id.into();
    }

    /// Answer the app's commands as moved to `moved` (each action's id, and the keys it is on
    /// now or none), instead of as the person's table moves them: a host that reads the table
    /// itself, and a test.
    pub fn set_moved_commands(&mut self, moved: Vec<(String, Option<String>)>) {
        self.moved.give(moved);
        if self.moves_undrawn() {
            self.compose();
        }
    }

    /// Whether the person moved a command since the window's controls were drawn.
    fn moves_undrawn(&self) -> bool {
        self.built.is_some()
            && self
                .moved
                .relabels(&self.app_id, |action| self.declared_keys(action))
                != self.drawn
    }

    /// The keys the window answers the command `action` on: the ones the person moved it to,
    /// or the ones the app declared.
    #[must_use]
    pub fn command_keys(&self, action: &str) -> Option<String> {
        let declared = self.declared_keys(action);
        self.moved.keys(&self.app_id, action, declared.as_deref())
    }

    /// Give the window the person's picker: a page's file input is answered with the paths they
    /// choose there, and a window given none answers a chooser with nothing.
    pub fn set_page_picker(&mut self, picker: std::sync::Arc<dyn PagePicker>) {
        self.page_picker = Some(picker);
    }

    /// The system's call pictures reached this window: decode them where it draws, and keep each
    /// newest frame at the address its tile reads ([`ui::widget::call_slot`]). A picture whose
    /// person or source stopped showing is forgotten, so its tile shows their face again. Whether
    /// the window changed.
    fn call_pictures_arrived(
        &mut self,
        shown: &[wire::CallShown],
        pictures: Vec<wire::CallPicture>,
    ) -> bool {
        let kept: std::collections::BTreeSet<String> = shown
            .iter()
            .map(|shown| call_slot(&shown.person, tile_source(shown.source)))
            .collect();
        let mut changed = false;
        for slot in self.call_shown.difference(&kept) {
            ui::live_pictures::forget(slot);
            changed = true;
        }
        self.call_shown = kept;
        let coded: Vec<ui::native_media::Picture> = pictures
            .into_iter()
            .map(|picture| ui::native_media::Picture {
                person: picture.person,
                stream: picture.stream,
                source: tile_source(picture.source).of_call(),
                object: picture.object,
            })
            .collect();
        if !self.call_pictures.decode_now(coded) {
            return changed;
        }
        for (person, source, image) in self.call_pictures.pictures() {
            let slot = call_slot(&person, CallSource::of_picture(source));
            if !self.call_shown.contains(&slot) {
                continue;
            }
            self.call_generation = self.call_generation.saturating_add(1);
            let pixels = std::sync::Arc::new(image.data.data().to_vec());
            ui::live_pictures::publish(
                &slot,
                self.call_generation,
                image.width,
                image.height,
                pixels,
            );
            changed = true;
        }
        changed
    }

    /// The places a playback plays, as the app last sent them ([`ToUi::Outputs`]), until the host
    /// takes them.
    pub fn take_outputs(&mut self) -> Option<Vec<u8>> {
        self.outputs.take()
    }

    /// The sound the window's player decoded since this was last asked, as stereo 48 kHz
    /// interleaved S16: the one stream the host plays in every place the session's outputs name,
    /// so several speakers and a cast target carry the same sound.
    pub fn take_audio(&self) -> Vec<i16> {
        let mut samples = Vec::new();
        audio_of(self.body.as_ref(), &mut samples);
        samples
    }

    /// Say `text` in the app's window as a notice of `kind`: a place the session plays that
    /// refused or stopped, said where the person looks instead of only in the log.
    pub fn say_notice(&mut self, text: &str, kind: wire::NoticeKind) {
        self.notice(&Words::Plain(text.to_owned()), kind, false);
    }

    /// The action the window answers by address, for a press no control of its tree declares: an
    /// action whose chord is `pressed`, on the resource the window shows. A row's action is answered
    /// by the row it is on, never by the window.
    fn listed_action(
        &self,
        pressed: &ui::platform::Shortcut,
    ) -> Option<tilo_actions::ActionAddress> {
        let shown: Vec<String> = [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
            .map(|view| view.target.id_string())
            .collect();
        self.actions()
            .iter()
            .find(|action| {
                shown.contains(&action.address.resource)
                    && action
                        .state
                        .shortcut
                        .as_deref()
                        .is_some_and(|chord| same_press(chord, pressed))
            })
            .map(|action| action.address.clone())
    }

    /// The person chose `address` from the window's command palette with `argument`, or pressed a
    /// control the app published the action for: the app runs it as their own step. `argument` is
    /// the palette's collected text or canonical JSON arguments, as a prompt or typed form encodes
    /// them. An action whose key a text field of the view declares (a list's entry) is pressed
    /// where that field is: the press puts the keyboard in it, as the key does, instead of making
    /// an empty item nobody typed.
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
        // What the person typed since the last batch goes out first: an action that reads the field
        // it was asked for (a save, a send) reads every character typed before it, never the text
        // as it stood before the last key of a burst.
        self.take_events();
        // A press like any other: what the person types while the app runs it goes where it leaves
        // the keyboard, and what it makes opens as their input.
        self.fired = self.fired.saturating_add(1);
        self.tell(ToApp::Invoke { address, argument });
    }

    /// Whether the app asked for the window to come forward since this was last asked
    /// ([`ToUi::Activate`]).
    pub fn take_activation(&mut self) -> bool {
        std::mem::take(&mut self.activations) > 0
    }

    /// The first task run a change mark in the window asked to show and the window has not
    /// taken yet ([`ToUi::ShowRun`]): the window shows it where the system keeps its log.
    pub fn take_shown_run(&mut self) -> Option<String> {
        (!self.shown_runs.is_empty()).then(|| self.shown_runs.remove(0))
    }

    /// What the app's connection heard of the people at the app since this was last asked
    /// ([`ToUi::Heard`]): the window's toolkit shows it as it shows what a connection of its own
    /// hears.
    pub fn take_heard(&mut self) -> Vec<ui::component_lifecycle::Heard> {
        std::mem::take(&mut self.heard)
    }

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

    /// Refuse what the app sent: the window shows the platform's error state, and takes nothing
    /// more until the app opens its root anew.
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

    /// Do what the app asked of the window's page, and answer it: an operation the page answers
    /// at once is done here; one the page thinks about waits with the window's next frames.
    fn page_asked(&mut self, request_id: u32, request: Vec<u8>) {
        let Ok(request) = String::from_utf8(request) else {
            self.refuse_page(request_id, "a page request is no text");
            return;
        };
        match crate::page_runner::ask(&self.page, &request) {
            Ok(crate::page_runner::Asked::Answered(answer)) => {
                self.out.push(ToApp::PageAnswer {
                    request_id,
                    answer: answer.into_bytes(),
                });
            }
            Ok(crate::page_runner::Asked::Waiting(action)) => {
                self.page_asks.insert(action, request_id);
            }
            Err(why) => self.refuse_page(request_id, &why),
        }
    }

    /// Answer a page request the window cannot run, so the action waiting on it is told why and
    /// never waits for good.
    fn refuse_page(&mut self, request_id: u32, why: &str) {
        self.out.push(ToApp::PageAnswer {
            request_id,
            answer: tilo_page_engine::codec::refused(why).into_bytes(),
        });
    }

    /// Whether the window, showing the error state, leaves `message` aside: it takes nothing
    /// until the app opens its root anew, though a page request in it still answers, so an
    /// action waiting on the page is never left waiting.
    fn refuses(&mut self, message: &ToUi) -> bool {
        if self.failed.is_none()
            || matches!(
                message,
                ToUi::Open {
                    level: Level::Root,
                    ..
                }
            )
        {
            return false;
        }
        if let ToUi::Page { request_id, .. } = message {
            self.refuse_page(*request_id, "this window shows no page");
        }
        true
    }

    #[allow(
        clippy::too_many_lines,
        reason = "one arm per message of one closed protocol"
    )]
    fn apply(&mut self, messages: Vec<ToUi>) -> Result<(), String> {
        // What the keyboard's editor holds before this batch: the record a change in it is
        // measured against.
        self.leave_editor_whose_record_changed();
        let mut content = false;
        // Rows the app sent for a list: its selection then names rows it did not hold yet.
        let mut arrived = false;
        let mut front = None;
        // A view a person's press opened: its first field takes the keyboard.
        let mut opened_for_input = false;
        for message in messages {
            if self.refuses(&message) {
                continue;
            }
            match message {
                ToUi::Loading => self.show_loading(),
                ToUi::Activate => self.activations = self.activations.saturating_add(1),
                ToUi::ShowRun { run } => self.shown_runs.push(run),
                ToUi::Speak(language) => ui::i18n::I18nManager::set_locale(&language),
                ToUi::Kept(kept) => self.kept(kept),
                // The window's page does what the app asked, and its answer goes back as the
                // engine gave it. An operation the page answers at once is done here; one the
                // page thinks about waits with the window's next frames.
                ToUi::Page {
                    request_id,
                    request,
                } => self.page_asked(request_id, request),
                // Its page goes back to what the window shows ([`Self::compose`]).
                ToUi::History(shown) => content |= self.shows_history(shown),
                // What the window shows of people never stops it showing the app.
                ToUi::Heard(heard) => match serde_json::from_slice(&heard) {
                    Ok(heard) => self.heard.push(heard),
                    Err(error) => tracing::warn!(%error, "what the app heard does not read"),
                },
                // The call's newest pictures, decoded where this window draws them.
                ToUi::CallPictures { shown, pictures } => {
                    content |= self.call_pictures_arrived(&shown, pictures);
                }
                ToUi::Open {
                    level,
                    target,
                    tree,
                    front: brought,
                    capture_protected,
                } => {
                    opened_for_input |=
                        self.opened(level, target, tree, brought, capture_protected);
                    content = true;
                    front = brought.or(front);
                }
                ToUi::Close => {
                    if self.detail.take().is_some() {
                        self.forget_level(Level::Detail);
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
                ToUi::Patch { level, patches } => {
                    let view = match level {
                        Level::Root => self.root.as_mut(),
                        Level::Detail => self.detail.as_mut(),
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
                    level,
                    key,
                    value,
                    acked,
                } => self.take_bound(level, &key, value, acked),
                rows @ (ToUi::Rows { .. } | ToUi::RowsChanged { .. }) => {
                    self.take_rows(rows);
                    // A row that arrived can hold a list of its own (the document of a row open), whose rows the app sends next.
                    self.track_rows();
                    arrived = true;
                }
                ToUi::Spaces { level, keys } => self.take_spaces(level, keys),
                ToUi::Notice { words, kind, undo } => self.notice(&words, kind, undo),
                ToUi::Ask(ask) => self.rebuild_ask(ask.as_ref()),
                ToUi::Form(form) => self.show_form(form.as_deref()),
                ToUi::Actions(catalogue) => self.take_actions(&catalogue)?,
                ToUi::Outputs(outputs) => self.outputs = Some(outputs),
                ToUi::Took { fired } => {
                    self.took = self.took.max(fired);
                    // What was typed after the presses it ran goes to what they opened, before what
                    // a later press opened (further on in this batch) takes its place.
                    opened_for_input = false;
                    let opened = self.opened.take().is_some();
                    self.send_typed_into(opened);
                }
                // The app asks where the keyboard goes: a control by its key in the composed tree,
                // or no control at all (the `blur()` a view applies), which the tree reads as an
                // empty request.
                ToUi::Focus { key } => {
                    self.focus_request = Some(key.unwrap_or_default());
                    self.structure_dirty = true;
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
        self.leave_editor_whose_record_changed();
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

    /// The level `level` opened on `target` with the tree `tree`, coming to the front of a narrow
    /// window when `brought` names the press that opened it. Whether that press takes the keyboard
    /// into the page.
    fn opened(
        &mut self,
        level: Level,
        target: ResourceRef,
        tree: UiNode,
        brought: Option<wire::Front>,
        capture_protected: bool,
    ) -> bool {
        if level == Level::Root {
            self.failed = None;
        }
        self.forget_level(level);
        let view = View {
            target,
            tree,
            capture_protected,
        };
        match level {
            Level::Root => {
                self.root = Some(view);
                // What the window's own view showed in place is no part of a view that opened
                // again.
                self.shown_in_place = None;
                self.page_focus = None;
                self.page_focus_tries = 0;
            }
            Level::Detail => self.detail = Some(view),
        }
        self.track_rows();
        if level == Level::Detail && brought == Some(wire::Front::Input) {
            // Its Took may come in a later batch: what is typed until then follows the press into
            // it.
            self.opened = Some(self.fired);
            return true;
        }
        false
    }

    /// The page the person's press showed is built again with the item: the keyboard goes into it
    /// once it is there, and once more if the page was built after the first ask.
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

    /// What the app offers the person at the window now ([`crate::catalogue::Offered::take`]).
    fn take_actions(&mut self, catalogue: &[u8]) -> Result<(), String> {
        self.offered.take(catalogue)?;
        // The window publishes each action as the resource its controls are on.
        let published = self.catalogue().to_vec();
        self.bind_published(&published);
        Ok(())
    }

    /// Bind the window's controls to the actions the app publishes (`published`): a control whose
    /// event runs one of them runs it through the action API, the one path anyone else running the action
    /// takes, and is where an agent running it is seen. An app whose instance runs in the window's
    /// own process publishes its actions itself and binds them here; an app in a window sends them
    /// ([`ToUi::Actions`]).
    pub fn bind_published(&mut self, published: &[tilo_actions::ResourceSpec]) {
        // A control is bound to the action its event runs when its widget is built: controls whose
        // events would now take the other path are built again, and they alone, so a press and
        // anyone else running the action take the one path while the rest of the window stays as
        // it stands. The app publishing the same actions again binds nothing anew.
        let (rebound, rows_changed) = {
            let reconciler = self.reconciler.borrow();
            let adapter = reconciler.adapter();
            match adapter.published(published.to_vec()) {
                None => (None, false),
                Some(shown) => {
                    // A list builds its rows from the nodes the app rendered them as, which are
                    // not in the window's tree: a row whose control would take the other path is
                    // named changed, so its list builds it again.
                    let mut rows_changed = false;
                    for rows in self.rows.borrow().values() {
                        rows_changed |= rows.borrow_mut().rebind(&shown, adapter);
                    }
                    let rebound = self
                        .built
                        .as_ref()
                        .and_then(|built| rebind::rebinding(&shown, adapter, built));
                    (rebound, rows_changed)
                }
            }
        };
        if rebound.is_some() || rows_changed {
            // Reconciled against the window as built with those controls named as another
            // kind: each is built again in its place, and a list rebuilds the rows named changed.
            if let Some(rebound) = rebound {
                self.built = Some(rebound);
            }
            self.compose();
        }
    }

    /// Rows of a virtual list, or what changed of them, into the list that shows them.
    fn take_rows(&self, message: ToUi) {
        match message {
            ToUi::Rows {
                level,
                list,
                len,
                start,
                keys,
                rows,
            } => {
                if let Some(held) = self.rows.borrow().get(&composed_key(level, &list)) {
                    held.borrow_mut()
                        .rows(len as usize, start as usize, keys, rows);
                }
            }
            ToUi::RowsChanged {
                level,
                list,
                len,
                changes,
            } => {
                if let Some(held) = self.rows.borrow().get(&composed_key(level, &list)) {
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

    fn forget_level(&mut self, level: Level) {
        let prefix = match level {
            Level::Root => None,
            Level::Detail => Some(DETAIL),
        };
        let ours = |key: &String| match prefix {
            Some(prefix) => key.starts_with(prefix),
            None => !key.starts_with(DETAIL),
        };
        self.rows.borrow_mut().retain(|key, _| !ours(key));
        self.bound.borrow_mut().retain(|key, _| !ours(key));
        self.texts.retain(|key, _| !ours(key));
        self.seqs.retain(|key, _| !ours(key));
        // The field a press opened for this level is gone with it: what was typed for it goes to the
        // resource it was typed into, wherever the level shows now.
        if self.landing.as_ref().is_some_and(ours) {
            self.landing = None;
        }
    }

    fn track_rows(&self) {
        let mut wanted: Vec<Rows> = Vec::new();
        for (view, level, prefix) in [
            (self.root.as_ref(), Level::Root, ""),
            (self.detail.as_ref(), Level::Detail, DETAIL),
        ] {
            let Some(view) = view else { continue };
            for (list, item, heading) in crate::rows::lists(&view.tree) {
                wanted.push(Rows::new(prefix, level, &list, item, heading));
            }
        }
        // A row a list holds can hold a list of its own — the document of a row held open where
        // it stands — and so can that list's rows.
        let mut at = 0;
        while let Some(rows) = wanted.get(at) {
            let (level, prefix, heading) = (rows.level, rows.prefix, rows.heading.clone());
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
                wanted.push(Rows::new(prefix, level, &list, item, heading));
            }
            at = at.saturating_add(1);
        }
        let mut registry = self.rows.borrow_mut();
        registry.retain(|key, rows| {
            wanted
                .iter()
                .any(|want| &want.key == key && rows.borrow().reads(want.level, &want.list))
        });
        for rows in wanted {
            let kept = registry.entry(rows.key.clone()).or_insert_with(|| {
                let reader = Rc::new(RefCell::new(Reconciler::sharing(&self.reconciler.borrow())));
                Rc::new(RefCell::new(RemoteRows::new(
                    rows.level,
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

    /// What the bound control `key` of `level` shows now, as the app sends it: taken unless the
    /// person typed into it since what the app took (`acked`).
    fn take_bound(&mut self, level: Level, key: &str, value: Value, acked: u64) {
        let key = composed_key(level, key);
        self.app_bound.insert(key.clone(), value.clone());
        // The field keeps what the person typed into it while a press was running, while it still
        // shows the resource they typed into.
        let shows = self.target(level);
        // The app sent it before it took every splice the window sent of the field: it is the
        // field as it was before some of the person's typing, which the app still applies after it.
        let before_our_splices = self.seqs.get(&key).is_some_and(|sent| *sent > acked);
        if let Some(typed) = self.typed.iter_mut().find(|typed| {
            typed.key.as_deref() == Some(key.as_str())
                && typed.target.is_some()
                && typed.target == shows
        }) {
            // What the app sent for the field: the typing goes against it when the app ran the
            // press. The field keeps showing what the person typed into it, which the app has not
            // taken: a value it held before is no reason to take their words away. A value sent
            // before the app took the splices typed ahead of the held typing is no place to put
            // it: placed against that, the held typing would land before letters typed earlier
            // ("Q3 summari" sent, "se" held: "Q3se summari"). The held typing then goes against what the
            // field showed when it was typed, which is what the app holds once it took them.
            if !before_our_splices {
                typed.sent = Some(value);
            }
            return;
        }
        if before_our_splices {
            return;
        }
        if let Value::Text(text) = &value {
            self.texts.insert(key.clone(), text.clone());
        }
        self.bound.borrow_mut().insert(key, value);
        self.refresh_bound();
    }

    /// Whether the window shows the history of what it shows ([`ToUi::History`]). Its shell is
    /// built again: the history's page goes back to what the window shows.
    fn shows_history(&mut self, shown: bool) -> bool {
        self.history = ui::widget::HistoryControl::of(shown);
        true
    }

    /// Say in the window's bar whether the person's changes are saved: changes the workspace has
    /// not taken yet are marked unsaved, with "Try again", until it has; then the mark goes.
    fn kept(&mut self, kept: bool) {
        let state = if kept {
            ui::SaveState::Failed(t("tilolang-change-kept"))
        } else {
            ui::SaveState::Idle
        };
        self.save.set(state, ui::time::now());
        self.save.publish();
        ui::window_toolbar::show_edited(self.save.shows_unsaved());
    }

    /// A file dropped on the window where nothing of the app's own took it: kept in the device's
    /// content store, and handed to the app's view as the `File` it reads — its name, its media
    /// type, its size and where its bytes are — so an app whose process reads content addresses,
    /// never paths, can take it. Whether the view takes files dropped on the window at all.
    fn drop_on_window(&mut self, paths: &[std::path::PathBuf]) -> bool {
        let Some(shell) = self.built.as_ref() else {
            return false;
        };
        // Only a view that declared the window's `drop` event takes files dropped anywhere on it.
        if !shell.events.contains_key("drop") {
            return false;
        }
        let key = shell.key.clone();
        let mut files = Vec::new();
        for path in paths {
            match ui::files::dropped_file(path) {
                Ok(file) => files.push(ui::component::Value::File(file)),
                Err(refusal) => {
                    let words = Words::Plain(refusal.words());
                    self.notice(&words, wire::NoticeKind::Error, false);
                }
            }
        }
        if files.is_empty() {
            return false;
        }
        self.reconciler.borrow().adapter().fire(
            &key,
            "drop",
            Some(ui::component::Value::List(files)),
        );
        true
    }

    fn notice(&mut self, words: &Words, kind: wire::NoticeKind, undo: bool) {
        let text = say(words);
        // What the app says nothing of takes the one up down: the app's own way to take back a
        // notice it no longer means. A part has none of its own, and says nothing.
        if text.is_empty() {
            if let Some(shell) = self.shell() {
                shell.dismiss_toast();
            }
            return;
        }
        if self.shape == Shape::Part {
            // A part has no window to toast in: its embedder says it where the shell says things
            // ([`Self::take_notices`]).
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
            (wire::NoticeKind::Warning, false) => shell.toast(text, ToastKind::Warning),
            (wire::NoticeKind::Success, false) => shell.toast(text, ToastKind::Success),
            (wire::NoticeKind::Info, false) => shell.toast(text, ToastKind::Info),
        }
    }

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

    /// A one-column window whose body holds only an empty state opens on the list that chooses
    /// for it (the overview beside the body, else the sidebar): "Nothing open" with the way to
    /// choose a press away is nothing to do. The window knows its width and the app does not, so
    /// the choice is the window's. It is made once for each turn the body has nothing in it, so a
    /// person who looks at the empty body anyway keeps it.
    fn front_the_chooser(&mut self) {
        let Some(chooser) = self.front_chooser else {
            return;
        };
        // Only a window that shows one column at a time chooses: where both stand, the body is
        // beside the list and there is nothing to put in front of what.
        let one_column = self
            .shell()
            .is_some_and(|shell| shell.geometry().sidebar == SidebarPlacement::Screen);
        if !one_column {
            return;
        }
        self.front_chooser = None;
        if self
            .shell()
            .is_some_and(|shell| shell.front_pane() != chooser)
        {
            self.front = Some(chooser);
            if let Some(shell) = self.shell() {
                shell.show_from(chooser, MotionSource::Action);
            }
            self.structure_dirty = true;
        }
    }

    fn show_content(&mut self, source: MotionSource) {
        // The shell reports the columns its own front moves, and owes the frame that slides them; a
        // window whose content already stood in front (a press on what the person was reading)
        // changed nothing its layout is built for.
        let already_in_front = self.front == Some(NavPane::Content);
        if let Some(shell) = self.shell() {
            shell.show_from(NavPane::Content, source);
        }
        self.front = Some(NavPane::Content);
        if !already_in_front {
            self.structure_dirty = true;
        }
    }

    fn set_body_part(&mut self, index: usize, element: Element, fit: Fit) {
        if let Some(arranged) = self.body.as_any_mut().and_then(|any| {
            any.downcast_mut::<ui::widget::arrange::Arranged<ui::widget::arrange::FlexLayout>>()
        }) {
            let mut parts = arranged.take_parts();
            if let Some(target) = parts.get_mut(index) {
                *target = part(element, fit);
            }
            arranged.set_parts(parts);
        }
        self.structure_dirty = true;
    }

    /// Bring the shell up to date with the views: the root's shell with the view after it as its
    /// content, reconciled into the widgets it was built into.
    fn compose(&mut self) {
        if self.shape == Shape::Part && self.root.is_none() {
            // A part whose whole view is another app's state has no root of its own: what it shows
            // is the detail level's view, keyed as a window's detail view is so a press reaches the
            // view that drew it.
            let Some(detail) = self.detail.as_ref() else {
                return;
            };
            let tree = crate::reconcile::normalize_shown(&detail.tree);
            let composed = crate::shell::prefixed(&tree, DETAIL);
            self.reconcile_into_pane(&composed);
            self.built = Some(composed);
            self.refresh_bound();
            return;
        }
        let Some(root) = self.root.as_ref() else {
            return;
        };
        let root_tree = crate::reconcile::normalize_shown(&root.tree);
        if self.shape == Shape::Part {
            // A shell's part is its view, with no window shell composed around it.
            self.reconcile_into_pane(&root_tree);
            self.built = Some(root_tree);
            self.refresh_bound();
            return;
        }
        let root_shell = crate::shell::as_shell(&root_tree);
        let detail_shell = self.detail.as_ref().map(|detail| {
            let tree = crate::reconcile::normalize_shown(&detail.tree);
            crate::shell::prefixed(&crate::shell::as_shell(&tree), DETAIL)
        });
        let composed = crate::shell::compose(&root_shell, detail_shell.as_ref());
        // A page while it shows something other than the window itself: a view that shows the
        // resource the window already is (`show(r)` of the app's own root, a scenario that stages
        // it) is where the window opens, with nothing to go back to. The history of what the
        // window shows goes back to it.
        let composed = match (&self.root, &self.detail) {
            (Some(root), Some(detail))
                if !self.history.is_open()
                    && (root.target == detail.target
                        // A page the window's own view already shows (a tree beside what it
                        // shows) is no page after it: nothing behind it to go back to.
                        || {
                            let mut chosen = None;
                            chosen_item(&root.tree, &mut chosen);
                            chosen == Some(&detail.target)
                        }) =>
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
        let mut composed = crate::shell::header_rules(composed);
        // An empty state says nothing is there yet, so it is also where the first one is made:
        // the body's offers the window's primary action. At a width that keeps the sidebar a
        // screen of its own the body's bar control is not drawn, and the list's state is where
        // that action stands instead. A window folded onto one state keeps its bar, which holds
        // that action already: the state repeats no control standing over it.
        let folded = composed.props.contains_key(NOTHING_LISTED);
        if !folded {
            crate::shell::offer_primary(&mut composed, &["content"], None);
        }
        if self.standing == SidebarPlacement::Screen {
            // The control is the state's own here, so the line under it names what it starts;
            // where the columns stand beside each other the bar's control speaks for itself, and
            // the state keeps its words alone.
            let message = crate::i18n::t("tilolang-empty-start-here");
            crate::shell::offer_primary(
                &mut composed,
                &["sidebar", "overview"],
                Some(message.as_str()),
            );
        }
        // Whether the body has anything in it: a body that holds only an empty state says nothing
        // is chosen or open, and a one-column window has nothing to show there. The column that
        // chooses owes the front when the body turned empty this way; a layout decides it once it
        // knows the window shows one column (`Self::front_the_chooser`).
        let nothing = crate::shell::shows_nothing(&composed);
        let was_nothing = self.built.as_ref().is_some_and(crate::shell::shows_nothing);
        if nothing && !was_nothing {
            // What chooses stands nearest the body: the overview list beside it, else the sidebar.
            self.front_chooser = Some(if crate::shell::has_overview(&composed) {
                NavPane::Overview
            } else {
                NavPane::Sidebar
            });
        } else if !nothing {
            self.front_chooser = None;
        }
        // Where the person is now, not where the last frame laid them out: a place chosen in this
        // very input put its page in front, and the shell its choice builds anew keeps it there.
        // A shell never laid out has no screen in front yet.
        if self.front.is_some()
            && let Some(shell) = self.shell()
        {
            self.front = Some(shell.front_pane());
        }
        // A window that had nothing at all to list and now lists something shows its columns again,
        // and opens on the screen it would have opened on: the front of a column that was gone is
        // no screen to keep (a notebook staged after its window opened keeps its tree in front on a
        // phone, not the first note).
        if !composed.props.contains_key(crate::shell::NOTHING_LISTED)
            && self
                .built
                .as_ref()
                .is_some_and(|built| built.props.contains_key(crate::shell::NOTHING_LISTED))
        {
            self.front = None;
        }
        let composed = match self.front {
            // A shell built anew opens on the screen a new window opens on: the window hands it the
            // one it shows, so the person stays where they are.
            Some(front) => with_front(composed, front),
            None => composed,
        };
        self.reconcile_into_pane(&composed);
        self.built = Some(composed);
        self.select_shown();
        self.refresh_bound();
    }

    fn reconcile_into_pane(&mut self, composed: &UiNode) {
        // A shell built anew in place of this one keeps the notice it had up: an action that
        // changes what the app publishes rebinds the window's controls, and the person's offer to
        // undo what they just did outlives the shell it stood in.
        let held = self.shell().and_then(|shell| shell.held_notice());
        let relabels = self
            .moved
            .relabels(&self.app_id, |action| self.declared_keys(action));
        let drawn = std::mem::replace(&mut self.drawn, relabels.clone());
        let old = self.built.take();
        let old = old.as_ref().map(|old| moved::relabeled(old, &drawn));
        let composed = &*moved::relabeled(composed, &relabels);
        let reconciler = Rc::clone(&self.reconciler);
        // What an update builds into a composed window arrives; its first tree shows at rest.
        let _window = ui::motion::laying_out_window(old.is_some());
        let mut rebuilt = false;
        if let Some(pane) = self.shell_pane() {
            let mut reconciler = reconciler.borrow_mut();
            match &old {
                Some(old) if old.key == composed.key && old.kind == composed.kind => {
                    if let Some(replaced) = reconciler.update_in(pane.content_mut(), old, composed)
                    {
                        pane.set(replaced);
                    }
                }
                _ => {
                    pane.set(reconciler.build(composed));
                    rebuilt = true;
                }
            }
        }
        if rebuilt
            && let Some(held) = held
            && let Some(shell) = self.shell()
        {
            shell.restore_notice(held);
        }
    }

    /// Select, in each list of the overview, the row of the view shown after it: the app keeps no
    /// selection beside it. The sidebar's place follows what the content shows the same way: a
    /// place names the resource it shows by the id alone, so the window chooses the one that names
    /// the shown resource, and leaves the app's choice when none does.
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
    /// shell's, and those of the rows its lists show.
    fn refresh_bound(&self) {
        if let Some(built) = &self.built {
            let reconciler = self.reconciler.borrow();
            show_bound(built, "", &self.bound.borrow(), &reconciler);
        }
        for rows in self.rows.borrow().values() {
            rows.borrow_mut().refresh_bound();
        }
    }

    /// The Ask bar: trusted chrome naming who asks for what, with the person's choices. It is the
    /// system's permission prompt, which no app can draw, so an app cannot forge it.
    fn ask_bar(ask: &wire::Ask) -> Element {
        let question = t_args(
            "tilolang-ask-action",
            &[("action", ask.action.as_str().into())],
        );
        Box::new(
            // Who asks stands over the question, as an app asking for a capability does.
            PermissionPrompt::for_owner(ask.invoker.clone(), question)
                .with_icon(ui::icons::Icon::Shield)
                .with_choices(vec![
                    (PromptChoice::Allow, t("tilolang-ask-confirm")),
                    (PromptChoice::Always, t("tilolang-ask-always")),
                    (PromptChoice::Deny, t("tilolang-ask-decline")),
                ]),
        )
    }

    /// The person's answer on the Ask bar since the last look, taken from the prompt.
    fn take_ask_answer(&mut self) -> Option<Answer> {
        self.ask.as_ref()?;
        let prompt = self
            .body
            .children_mut()
            .into_iter()
            .nth(Self::ASK_PART)?
            .as_any_mut()?
            .downcast_mut::<PermissionPrompt>()?;
        Some(match prompt.pending_choice.take()? {
            PromptChoice::Allow | PromptChoice::Session => Answer::Allow,
            PromptChoice::Always => Answer::Always,
            PromptChoice::Deny => Answer::Decline,
        })
    }

    const ASK_PART: usize = 1;

    fn rebuild_ask(&mut self, ask: Option<&wire::Ask>) {
        let bar: Element = match ask {
            Some(ask) => Self::ask_bar(ask),
            None => Box::new(Absent::new()),
        };
        self.ask = ask.cloned();
        self.set_body_part(Self::ASK_PART, bar, Fit::hug());
    }

    const FORM_PART: usize = 2;

    /// Show the form `form` of an action waiting for its arguments, or none. The form the window
    /// shows already, sent again as it reads now, is reconciled in place.
    fn show_form(&mut self, form: Option<&wire::Form>) {
        let Some(form) = form else {
            if self.form.take().is_some() {
                self.set_body_part(Self::FORM_PART, Box::new(Absent::new()), Fit::hug());
            }
            return;
        };
        let reconciler = Rc::clone(&self.reconciler);
        if let Some(shown) = self.form.as_mut().filter(|shown| shown.id == form.id) {
            let sheet = self
                .body
                .children_mut()
                .into_iter()
                .nth(Self::FORM_PART)
                .and_then(crate::form::sheet_of);
            if let Some(sheet) = sheet {
                shown.update(form, sheet, &mut reconciler.borrow_mut());
                self.structure_dirty = true;
                return;
            }
        }
        let (shown, mut sheet) = crate::form::Shown::build(form, &mut reconciler.borrow_mut());
        // A form of a field or two is a popover beside the control that ran it.
        if crate::form::fits_a_popover(form)
            && let Some((level, key)) = &form.from
        {
            sheet.set_anchor(rect_of(self.body.as_ref(), &composed_key(*level, key)));
        }
        self.form = Some(shown);
        self.set_body_part(Self::FORM_PART, Box::new(sheet), Fit::hug());
    }

    /// What the person answered in the form's own controls since the last frame. Cancel closes it
    /// at once; what they typed stays the app's.
    fn take_form_answer(&mut self) {
        let Some(answer) = self.form.as_ref().and_then(crate::form::Shown::take_answer) else {
            return;
        };
        if matches!(answer, ToApp::FormCancel { .. }) {
            self.show_form(None);
        }
        self.out.push(answer);
    }

    /// Whether the window holds a message for the app it has not sent: a space a layout gave a
    /// view, what the person did, a row asked for. An embedder that only sends when it has
    /// something to say asks this, so a window nothing touches hands the app what its frame gave
    /// its views.
    #[must_use]
    pub fn owes(&self) -> bool {
        !self.out.is_empty()
            || !self.page_asks.is_empty()
            || crate::page_runner::prompts_waiting(&self.page)
            || crate::page_runner::choosers_waiting(&self.page)
            || crate::page_runner::notes_waiting(&self.page)
            || !self.events.is_empty()
            || self
                .rows
                .borrow()
                .values()
                .any(|rows| rows.borrow().wants())
    }

    /// Whether the app has not yet said it ran every press the window sent it (`ToUi::Took`).
    #[must_use]
    pub fn presses_unanswered(&self) -> bool {
        self.fired > self.took
    }

    /// Send the app what the window holds for it, and take up what the person did and what the
    /// page answered. Whether there was any of it: a flush that found nothing changed nothing,
    /// so a host that flushes every turn owes no frame for it.
    pub fn flush(&mut self) -> bool {
        // What the person did in the window since the last flush: the keys it took, the events
        // its controls raised. What they lead to changes the window, as what goes out does.
        let input = std::mem::take(&mut self.keys.input);
        let mut took = input || !self.events.is_empty();
        // A file input the person clicked in the page waits on their choice: the host's picker
        // asks, in its own time, and answers the page with what they picked.
        for chooser in crate::page_runner::choosers(&self.page) {
            took = true;
            self.ask_page_picker(chooser);
        }
        // A page the app asked does its work in its own time: what it answered since the last
        // frame goes back to whoever asked.
        for (action, answer) in crate::page_runner::answers(&self.page) {
            if let Some(request_id) = self.page_asks.remove(&action) {
                self.out.push(ToApp::PageAnswer {
                    request_id,
                    answer: answer.into_bytes(),
                });
            }
        }
        // What the page said of itself that the app's record keeps (a download moving, the pages
        // it can go to) goes to the app, which writes it into the page's record through its own
        // action: the record moves while the person looks.
        for note in crate::page_runner::notes(&self.page) {
            self.out.push(ToApp::PagePrompt {
                prompt: note.into_bytes(),
            });
        }
        // What the page waits on the person's yes for goes to the app, which raises each as its
        // call of its own action, and withdraws it once the page no longer waits.
        for prompt in crate::page_runner::prompts(&self.page) {
            self.out.push(ToApp::PagePrompt {
                prompt: prompt.into_bytes(),
            });
        }
        // What was typed while the keyboard was on its way to the field a press opened goes there
        // once it arrived.
        took |= self.release_landing();
        if let Some(answer) = self.take_ask_answer() {
            self.answers.borrow_mut().push(answer);
        }
        for answer in std::mem::take(&mut *self.answers.borrow_mut()) {
            self.out.push(ToApp::Answer(answer));
        }
        self.take_events();
        self.take_form_answer();
        if self.shell().and_then(NavShell::take_toast_action).is_some() {
            self.out.push(ToApp::Undo);
        }
        if self.save.take_bar_action().is_some() {
            self.out.push(ToApp::SaveAgain);
        }
        if input {
            self.follow_focus();
        }
        let wanted: Vec<(Level, String, crate::rows::Asked)> = self
            .rows
            .borrow()
            .values()
            .filter_map(|rows| rows.borrow_mut().take_wanted())
            .collect();
        for (level, list, asked) in wanted {
            self.out.push(ToApp::Rows {
                level,
                list,
                start: u32::try_from(asked.range.start).unwrap_or(u32::MAX),
                end: u32::try_from(asked.range.end).unwrap_or(u32::MAX),
                open: asked.open,
                menu: asked.menu,
            });
        }
        if self.out.is_empty() {
            return took;
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
        true
    }

    /// Ask the person for the files the page's input takes, in the host's picker, and answer the
    /// page with what they chose. The picker answers in its own time, so it runs off the frame
    /// and the answer goes to the engine, which wakes the window when it lands. A window the
    /// host gave no picker answers the page with nothing, so no page waits on a chooser no one
    /// can answer.
    fn ask_page_picker(&self, chooser: tilo_webview::FileChooser) {
        #[cfg(target_arch = "wasm32")]
        {
            let _ = chooser;
            return;
        }
        #[cfg(not(target_arch = "wasm32"))]
        {
            let Some(picker) = self.page_picker.clone() else {
                crate::page_runner::answer_chooser(&self.page, chooser.id, Vec::new());
                return;
            };
            let engine = crate::page_runner::engine_of(&self.page);
            let asked = std::thread::Builder::new()
                .name("tilo-page-file-chooser".to_owned())
                .spawn(move || {
                    let files = PageFiles {
                        title: chooser.title.clone(),
                        several: chooser.mode == tilo_webview::FileChooserMode::Several,
                        folder: chooser.mode == tilo_webview::FileChooserMode::Folder,
                        accept: chooser.accept.clone(),
                    };
                    let paths = picker.pick(&files);
                    engine.send(tilo_webview::WebViewCommand::AnswerFileChooser {
                        id: chooser.id,
                        paths,
                    });
                });
            if asked.is_err() {
                crate::page_runner::answer_chooser(&self.page, chooser.id, Vec::new());
            }
        }
    }

    fn take_events(&mut self) {
        // One queue, in the order the person did things.
        for event in self.events.take() {
            self.event(event);
        }
    }

    /// Tell the app which bound control the keyboard focus is in now, when that changed: what its
    /// presence shows of the person (where they type).
    fn follow_focus(&mut self) {
        let now = self.focused_field();
        if now == self.focus {
            return;
        }
        self.out.push(ToApp::Focus {
            at: now.as_deref().map(level_of),
        });
        self.focus = now;
    }

    /// The record and field the bound control `field` edits, by its key in the composed tree.
    fn field_binding(&self, field: &str) -> Option<(String, String)> {
        let (level, key) = level_of(field);
        let tree = match level {
            Level::Root => self.root.as_ref(),
            Level::Detail => self.detail.as_ref(),
        }?;
        let node = node_at(&tree.tree, &key)?;
        let (target, field) = node.binding.as_ref()?.field()?;
        Some((target.to_owned(), field.to_owned()))
    }

    /// The editor holding the keyboard now edits another record than it did: an editor the view
    /// rebuilt for another one (a deck's next slide) gives the keyboard up, as closing it did, so
    /// what the person types next never lands in a record they did not choose.
    fn leave_editor_whose_record_changed(&mut self) {
        let Some(now) = self.focused_field() else {
            self.focus_binding = None;
            return;
        };
        let binding = self.field_binding(&now);
        let left = matches!(
            (&self.focus_binding, &binding),
            (Some((key, before)), Some(after)) if key == &now && before != after
        );
        self.focus_binding = binding.map(|binding| (now, binding));
        if left {
            self.keys.blur_request = true;
            self.structure_dirty = true;
            self.focus = None;
            self.out.push(ToApp::Focus { at: None });
        }
    }

    fn event(&mut self, event: UiEvent) {
        if crate::form::in_form(&event) {
            if let Some(edit) = self.form.as_ref().and_then(|form| form.edit(&event)) {
                self.out.push(edit);
            }
            return;
        }
        if let UiEvent::Fire(key, name, _, _) = &event
            && *name == crate::shell::BACK
            && self.built.as_ref().is_some_and(|built| built.key == *key)
        {
            self.out.push(ToApp::Back);
            return;
        }
        // A control pressed where a field declares its same key leads the person to the field, as
        // the key does: the window's Primary brings them to a list's entry to type the item,
        // instead of making one nobody typed.
        if let UiEvent::Press(key) | UiEvent::Fire(key, _, None, _) = &event
            && let Some(field) = self.press_leads_a_field(key)
        {
            self.focus_request = Some(field);
            return;
        }
        if matches!(event, UiEvent::Press(_) | UiEvent::Fire(..)) {
            self.fired = self.fired.saturating_add(1);
            // What the person held with the press goes before it, when it changed: the app reads
            // it as the press's `host.Press`.
            if self.held != self.held_sent {
                self.held_sent = self.held;
                self.out.push(ToApp::Held(self.held));
            }
        }
        let message = match event {
            UiEvent::Press(key) => {
                let (level, key) = level_of(&key);
                ToApp::Fire {
                    level,
                    key,
                    event: "press".to_owned(),
                    value: None,
                    gesture: None,
                }
            }
            UiEvent::Fire(key, event, value, gesture) => {
                let (level, key) = level_of(&key);
                ToApp::Fire {
                    level,
                    key,
                    event,
                    value,
                    gesture,
                }
            }
            UiEvent::Edit(composed, text) => match self.edit(&composed, text) {
                Some(message) => message,
                None => return,
            },
            UiEvent::EditRich(composed, rich, gesture) => {
                match self.edited_rich(&composed, rich, gesture) {
                    Some(message) => message,
                    None => return,
                }
            }
            UiEvent::Toggle(key, checked) => {
                let (level, key) = level_of(&key);
                let Some(target) = self.target(level) else {
                    return;
                };
                ToApp::Toggle {
                    level,
                    target,
                    key,
                    checked,
                }
            }
            UiEvent::Resolve(key, approve) => {
                let (level, key) = level_of(&key);
                ToApp::Resolve {
                    level,
                    key,
                    approve,
                }
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
        self.bound_text(composed, held, None)
    }

    fn edited_rich(
        &mut self,
        composed: &str,
        rich: wire::rich::RichText,
        gesture: Option<String>,
    ) -> Option<ToApp> {
        let base = match self.bound.borrow().get(composed) {
            Some(Value::Rich(before)) => before.clone(),
            _ => wire::rich::RichText::default(),
        };
        self.bound_text(composed, Held::Rich { base, now: rich }, gesture)
    }

    /// The bound control `composed` now holds `held`: shown as the person left it, and sent, unless
    /// a press it was typed after has not run yet; then it waits, like every control that takes
    /// typing ([`Self::hold`]).
    fn bound_text(&mut self, composed: &str, held: Held, gesture: Option<String>) -> Option<ToApp> {
        let (level, key) = level_of(composed);
        // A control that edits no text (a number, a date) sends the value it now holds: how its
        // edits merge, as its component's description says. A control no component built holds text
        // when what it shows is text.
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
            let target = self.target(level)?;
            return Some(ToApp::Edit {
                level,
                target,
                key,
                text: now,
            });
        }
        let shown = match &held {
            Held::Text { now, .. } => Value::Text(now.clone()),
            Held::Rich { now, .. } => Value::Rich(now.clone()),
        };
        // Typing after a press the app has not run yet waits for it: the press may open something
        // else into the field the person types into. An edit that is one part of a gesture with a
        // press of its own (a picture pasted in, whose name the app records) does not wait: the
        // two changes are one step of the person's whether they land one before the other.
        if self.fired > self.took && gesture.is_none() {
            self.hold(composed, level, held, shown);
            return None;
        }
        match held {
            Held::Text { base, now } => self.splice(composed, &base, now),
            Held::Rich { now, .. } => {
                let target = self.target(level)?;
                self.bound
                    .borrow_mut()
                    .insert(composed.to_owned(), Value::Rich(now.clone()));
                Some(ToApp::EditRich {
                    level,
                    target,
                    key,
                    rich: now,
                    gesture,
                })
            }
        }
    }

    /// Hold what the person typed in the bound control `composed` of `level` until the presses the
    /// app has not run yet did, showing `shown` meanwhile: what they typed goes where the keyboard
    /// would have been had the press run at once.
    fn hold(&mut self, composed: &str, level: Level, held: Held, shown: Value) {
        let fired = self.fired;
        let target = self.target(level);
        let same_field =
            |typed: &Typed| typed.key.as_deref() == Some(composed) && typed.target == target;
        // The person moved on to another field meanwhile: what they typed before stays where they
        // typed it.
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
            // Every key after the first keeps what the control held before the person began: what
            // they typed is read from there.
            match (&mut typed.held, held) {
                (Held::Text { now, .. }, Held::Text { now: newer, .. }) => now.clone_from(&newer),
                (Held::Rich { now, .. }, Held::Rich { now: newer, .. }) => *now = newer,
                (part, newer) => *part = newer,
            }
        }
        // What the field shows is what the person typed, until the app takes it.
        if let Some(Held::Text { now, .. }) = self.typed.back().map(|typed| &typed.held) {
            self.texts.insert(composed.to_owned(), now.clone());
        }
        self.bound.borrow_mut().insert(composed.to_owned(), shown);
    }

    /// The view a person's press opened takes the keyboard in its first text field, and what they
    /// typed after the press, before it ran, goes there, as it would have had the press run at
    /// once. What they typed before the press, or into a field they put the keyboard in since, is
    /// that field's, of the resource it showed.
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
        // The page a press showed: the item opened after the window's own, or, for a window whose
        // page is its own (a tree beside what it shows), the first field of its content.
        let page = self
            .detail
            .as_ref()
            .and_then(|detail| first_field(&detail.tree))
            .map(|field| (composed_key(Level::Detail, &field.key), Level::Detail));
        let (field, level) = if let Some(page) = page {
            page
        } else {
            let Some(root) = self.root.as_ref() else {
                return;
            };
            let Some(field) = content_nodes(&root.tree).into_iter().find_map(first_field) else {
                return;
            };
            (field.key.clone(), Level::Root)
        };
        // What was typed with no field focused after the press the app just ran, not after a later
        // one.
        let took = self.took;
        let target = self.target(level);
        // What the field the keyboard goes to holds: a body's rich text typed into a title is
        // words, and the title takes words.
        let rich = self
            .built
            .as_ref()
            .and_then(|built| node_of_key(built, level_of(&field).1.as_str()))
            .is_some_and(|node| node.kind == crate::adapter::kind("RichInput"));
        // The fields the typing leaves show what the app has of them again, not the words that went
        // elsewhere.
        let mut left = Vec::new();
        for typed in &mut self.typed {
            if typed.after <= took && typed.follows && !typed.moved {
                if typed.key.is_some() {
                    // Only what the person typed moves, never what the field held before it, and a
                    // line break a body's text ends with does not move into a title.
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
                // Its old value is no part of the field it goes to now: what it holds there is read
                // from that field.
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
        if level == Level::Root && self.detail.is_none() {
            self.page_focus = Some(field.clone());
            self.page_focus_tries = 0;
        }
        self.ask_focus(field);
    }

    /// Ask the app to put the keyboard in `field` (its key in the composed tree), and keep what the
    /// person types until it has it that field's ([`Self::hold_for_landing`]).
    fn ask_focus(&mut self, field: String) {
        self.landing = Some(field.clone());
        self.focus_request = Some(field);
    }

    /// What the person typed while the keyboard was on its way to the field a press opened goes
    /// there once the app took the window's request for it.
    fn release_landing(&mut self) -> bool {
        if self.landing.is_some() || !self.typed.iter().any(|typed| typed.waits_focus) {
            return false;
        }
        for typed in &mut self.typed {
            typed.waits_focus = false;
        }
        self.send_typed();
        true
    }

    /// Hold the text the person typed while the keyboard was on its way to `field`, the field a
    /// press opened, showing it there as they type: it goes when the keyboard arrives.
    fn hold_for_landing(&mut self, field: &str, text: &str) {
        let after = self.fired;
        let (level, _) = level_of(field);
        let target = self.target(level);
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
            // What the app has for the field: the text the person's typing goes after, and what its
            // echo is measured against.
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
    /// keyboard (the press took it), into the field it leaves the keyboard in. Whether it was held.
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
        // A press the window's controls reported since the last event counts: the key may follow it
        // in the same frame.
        self.take_events();
        if key.state != ui::winit::event::ElementState::Pressed {
            return false;
        }
        // The keyboard is on its way to the field a press opened: what the person types meanwhile
        // is that field's, never the one the keyboard is leaving.
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

    /// Enter in a field of one line that submits nothing moves the keyboard on to the next field of
    /// its view, as native Notes goes from a note's title to its body. A field with `submit` keeps
    /// Enter: it submits. Whether it moved.
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
            || self.keys.composing
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
        if self.keys.composing {
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

    /// Where a press of `pressed` leads in the window: the field it brings the person to, the field
    /// that carries it out (the keyboard is already there), or the control that runs it — on the
    /// item the keyboard is on first, then what that is in, then the window. A key is a binding to
    /// an action: the control that declares it runs it with the arguments the view bound, and a
    /// chord no control declares runs the action the window offers on the resource it shows, by its
    /// address, as the palette runs it. The person's own chords are the session's: it claims them
    /// before a window sees the press.
    ///
    /// A command the person moved to other keys (`settings.shortcuts` `rebind-command`) is
    /// pressed on those, as the app declared it, and the keys the app gave it are no longer its.
    /// The keys the app declared its action `action` on.
    fn declared_keys(&self, action: &str) -> Option<String> {
        self.actions()
            .iter()
            .find(|offered| offered.address.action == action)
            .and_then(|offered| offered.state.shortcut.clone())
    }

    /// The app's commands the person moved, as the window's palette shows them.
    pub(crate) fn moved_now(&self) -> Vec<(String, Option<String>)> {
        self.moved.now(&self.app_id)
    }

    fn press_where(&self, pressed: &ui::platform::Shortcut) -> Option<Press> {
        match self
            .moved
            .press(&self.app_id, pressed, |action| self.declared_keys(action))
        {
            moved::MovedPress::Unmoved => self.press_as_declared(pressed),
            moved::MovedPress::To(declared) => self.press_as_declared(&declared),
            moved::MovedPress::Off => None,
        }
    }

    /// Where a press of `pressed` leads, as the app declared its keys.
    fn press_as_declared(&self, pressed: &ui::platform::Shortcut) -> Option<Press> {
        let built = self.built.as_ref()?;
        let focused = self.focused_control();
        // A key a text field declares is the field's: where the keyboard is already in it, it
        // carries the key out there, on the item the person is on, instead of sending them to the
        // first field of the window that names it.
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
        if let Some(found) = shortcut_action_where(built, focused.as_deref(), pressed) {
            return Some(Press::Run(found.0, found.1));
        }
        self.listed_action(pressed).map(Press::RunAction)
    }

    /// The press the field the keyboard is in takes as its own: Enter, which a field of one line
    /// that submits commits what it holds with.
    fn enter_press(&mut self, ctx: &mut ui::widget::EventCtx<'_>) {
        let enter = ui::synthetic_input::named_key(ui::winit::keyboard::NamedKey::Enter);
        self.body.handle_event(&enter, ctx);
    }

    /// The field a press on the control `key` leads the person to: one whose declared key is the
    /// shortcut that control names, when there is one. See [`focus_field`].
    fn press_leads_a_field(&self, key: &str) -> Option<String> {
        let built = self.built.as_ref()?;
        let declared = shortcut_declared(built, key)?;
        focus_field(built, &declared)
    }

    /// Send what the person typed after each press the app ran since, into the view a press opened
    /// for them when `opened` (its first field takes the keyboard).
    fn send_typed_into(&mut self, opened: bool) {
        if opened {
            self.focus_opened();
        }
        self.send_typed();
    }

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

    /// Send what the person typed while a press was running into the field it went into, against
    /// what the field shows now: what they typed keeps its place from the field's end. Typed while
    /// no field had the keyboard, and the press opened no field: it goes to the field the keyboard
    /// is in now, at its end; with none, the window says where it went.
    fn send_typed_now(&mut self, mut typed: Typed) {
        if typed.key.is_none() {
            // The field the press showed takes it: the page beside a list, or the page after the
            // window's own, named while the keyboard goes there.
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
        // typing is no part of what the app has of it.
        if typed.moved && typed.sent.is_none() {
            typed.sent = self.bound.borrow().get(&key).cloned();
        }
        let (level, field) = level_of(&key);
        // The resource the field belongs to, when it is the one the person typed into: the item the
        // press before their typing showed, where the window draws the page itself.
        let target = self.field_target_for(&key, Some(typed.after));
        // A field of the page belongs to the item its press showed: it goes there even once the
        // page shows another, and what it showed when the person typed is what they typed into.
        let page = self.page_field(&key);
        let still_shown = typed.target.is_none() || typed.target == target || page;
        // What the field shows now: what the app sent for it since, or what it showed when the
        // person began typing (the window showed their typing meanwhile, which is no part of what
        // it shows now).
        let shown = match typed.sent.take() {
            Some(sent) if !page => sent,
            _ => match &typed.held {
                Held::Text { base, .. } => Value::Text(base.clone()),
                Held::Rich { base, .. } => Value::Rich(base.clone()),
            },
        };
        if still_shown && let Some(target) = target {
            self.send_typed_into_field(&key, level, field, target, &typed.held, &shown);
            return;
        }
        // The level moved on to another resource: what they typed goes to the one it was typed into,
        // and the field the level shows now keeps what it shows.
        let Some(target) = typed.target else {
            return;
        };
        let message = self.typed_elsewhere(&key, level, field, target, &typed.held, &shown);
        self.out.push(message);
    }

    /// Send what the person typed in `key` of `level` into the field the level shows now (`target`),
    /// against what it shows (`shown`).
    fn send_typed_into_field(
        &mut self,
        key: &str,
        level: Level,
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
                    level,
                    target,
                    key: field,
                    rich,
                    gesture: None,
                });
            }
            (Value::Text(before), _) => {
                let text = splice_text(before, (show_at, show_at.saturating_add(delete)), &insert);
                // Nothing of what they typed is left to make: the field shows it already.
                if text == *before {
                    return;
                }
                if let Some(message) = self.splice(key, before, text) {
                    self.out.push(message);
                }
            }
            (_, Held::Rich { now, .. }) => {
                self.bound
                    .borrow_mut()
                    .insert(key.to_owned(), Value::Rich(now.clone()));
                self.out.push(ToApp::EditRich {
                    level,
                    target,
                    key: field,
                    rich: now.clone(),
                    gesture: None,
                });
            }
            (_, Held::Text { now, .. }) => self.out.push(ToApp::Edit {
                level,
                target,
                key: field,
                text: now.clone(),
            }),
        }
    }

    /// What the person typed, where it goes in what the field shows now: their edit of what the
    /// field showed when they began (`held`'s base), rebased over the app's edit of that same
    /// text to what it sent since (`shown`), as a splice of the shown text.
    fn place_of(held: &Held, shown: &Value) -> (usize, usize, String) {
        let (base_text, now_text) = held.texts();
        let shown = match shown {
            Value::Text(text) => text.clone(),
            Value::Rich(rich) => rich_text_of(rich),
            _ => base_text.clone(),
        };
        rebase_splice(
            splice_between(&base_text, &now_text),
            splice_between(&base_text, &shown),
        )
    }

    /// What the person typed into the field of a resource the level left: an edit of the resource
    /// they typed into.
    fn typed_elsewhere(
        &self,
        key: &str,
        level: Level,
        field: String,
        target: ResourceRef,
        held: &Held,
        shown: &Value,
    ) -> ToApp {
        let (show_at, delete, insert) = Self::place_of(held, shown);
        match (held, shown) {
            (Held::Rich { base, .. }, _) => ToApp::EditRich {
                level,
                target,
                key: field,
                rich: rich_spliced_at(base, show_at, delete, &insert),
                gesture: None,
            },
            (Held::Text { .. }, Value::Rich(before)) => ToApp::EditRich {
                level,
                target,
                key: field,
                rich: rich_spliced_at(before, show_at, delete, &insert),
                gesture: None,
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
                    level,
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

    /// The key press this window handles answered the control `key`: its place in the window is
    /// where a question the call it makes raises stands. A control not laid out says nothing.
    fn note_key_control(&self, key: Option<&str>) {
        if let Some(rect) = key.and_then(|key| rect_of(self.body.as_ref(), key)) {
            ui::press::note_control(rect);
        }
    }

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

    fn focused_field(&self) -> Option<String> {
        let field = self.focused_control()?;
        matches!(
            self.bound.borrow().get(&field),
            Some(Value::Text(_) | Value::Rich(_))
        )
        .then_some(field)
    }

    /// Whether the keyboard is in a field that takes typing: a text field's keys are its own until
    /// it says it does not use one.
    fn typing(&self) -> bool {
        takes_typing(self.body.as_ref())
    }

    /// The splice of the bound text control `composed` from `before` to `text`, which it now shows.
    fn splice(&mut self, composed: &str, before: &str, text: String) -> Option<ToApp> {
        let (level, key) = level_of(composed);
        // Of the resource the field belongs to: the app applies it there, whatever the level shows
        // by the time it takes it.
        let target = self.field_target(composed)?;
        let (at, delete, insert) = splice_between(before, &text);
        let seq = self.seqs.entry(composed.to_owned()).or_default();
        *seq = seq.saturating_add(1);
        let seq = *seq;
        // What the field shows is what the person typed, whatever the window shows it again from
        // before the app takes it.
        self.bound
            .borrow_mut()
            .insert(composed.to_owned(), Value::Text(text.clone()));
        self.texts.insert(composed.to_owned(), text);
        Some(ToApp::Splice {
            level,
            target,
            key,
            seq,
            at: u32::try_from(at).unwrap_or(u32::MAX),
            delete: u32::try_from(delete).unwrap_or(u32::MAX),
            insert,
        })
    }

    fn target(&self, level: Level) -> Option<ResourceRef> {
        match level {
            Level::Root => self.root.as_ref(),
            Level::Detail => self.detail.as_ref(),
        }
        .map(|view| view.target.clone())
    }

    /// Whether what the window shows now declares `protect capture` — in its root or its detail
    /// view, since the window draws both: a capture or a stream leaves the whole window out.
    #[must_use]
    pub fn capture_protected(&self) -> bool {
        [self.root.as_ref(), self.detail.as_ref()]
            .into_iter()
            .flatten()
            .any(|view| view.capture_protected)
    }

    #[must_use]
    pub fn composed(&self) -> Option<&UiNode> {
        self.built.as_ref()
    }

    #[must_use]
    pub fn root_tree(&self) -> Option<UiNode> {
        self.root
            .as_ref()
            .map(|view| crate::reconcile::normalize(&view.tree))
    }

    #[must_use]
    pub fn detail_tree(&self) -> Option<UiNode> {
        self.detail
            .as_ref()
            .map(|view| crate::reconcile::normalize(&view.tree))
    }

    #[must_use]
    pub fn detail_target(&self) -> Option<&ResourceRef> {
        self.detail.as_ref().map(|view| &view.target)
    }

    #[must_use]
    pub fn root_target(&self) -> Option<&ResourceRef> {
        self.root.as_ref().map(|view| &view.target)
    }

    #[must_use]
    pub fn lists(&self) -> Vec<String> {
        let mut keys: Vec<String> = self.rows.borrow().keys().cloned().collect();
        keys.sort();
        keys
    }

    #[must_use]
    pub fn rows_of(&self, key: &str) -> Option<Rc<RefCell<RemoteRows>>> {
        self.rows.borrow().get(key).cloned()
    }

    #[must_use]
    pub fn bound_value(&self, key: &str) -> Option<Value> {
        self.bound.borrow().get(key).cloned()
    }

    #[must_use]
    pub fn failed(&self) -> Option<&str> {
        self.failed.as_deref()
    }

    #[must_use]
    pub fn broken_components(&self) -> Vec<String> {
        self.reconciler.borrow().broken()
    }

    #[must_use]
    pub fn asks(&self) -> Option<&wire::Ask> {
        self.ask.as_ref()
    }

    pub fn answer(&self, answer: Answer) {
        self.answers.borrow_mut().push(answer);
    }

    /// Tell the app `message` with the next batch: what the embedder knows that the app does not,
    /// such as the names of the workspace's members.
    pub fn tell(&mut self, message: ToApp) {
        self.out.push(message);
    }

    /// Report what a person did: an event of a control of the composed shell, by its key there.
    pub fn act(&self, event: UiEvent) {
        self.events.push(event);
    }

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

    pub fn shell_mut(&mut self) -> Option<&mut NavShell> {
        self.shell()
    }
}

/// Whether `event` is a key going down: a press the person made with the keyboard.
fn is_key_press(event: &ui::winit::event::WindowEvent) -> bool {
    matches!(
        event,
        ui::winit::event::WindowEvent::KeyboardInput { event: key, .. }
            if key.state == ui::winit::event::ElementState::Pressed && !key.repeat
    )
}

fn composed_key(level: Level, key: &str) -> String {
    match level {
        Level::Root => key.to_owned(),
        Level::Detail => format!("{DETAIL}{key}"),
    }
}

fn with_front(mut shell: UiNode, pane: NavPane) -> UiNode {
    let case = match pane {
        NavPane::Sidebar => "sidebar",
        NavPane::Overview => "overview",
        NavPane::Content => "content",
    };
    shell.props.insert("pane".to_owned(), Value::text(case));
    shell
}

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

/// The splice `ours` of a text, made against the same text as `theirs`, as a splice of the text
/// `theirs` made: what both inserted stays, ours after theirs where both insert at one place,
/// and what ours deletes that theirs deleted already is gone. Offsets are bytes.
#[must_use]
pub fn rebase_splice(
    (at, delete, insert): (usize, usize, String),
    (their_at, their_delete, their_insert): (usize, usize, String),
) -> (usize, usize, String) {
    let end = at.saturating_add(delete);
    let their_end = their_at.saturating_add(their_delete);
    let added = their_insert.len();
    // Wholly before what they changed: in place. An insert where theirs is goes after it.
    if end < their_at || (end == their_at && delete > 0) {
        return (at, delete, insert);
    }
    // Wholly after: moved by what theirs grew or shrank the text by.
    if at >= their_end {
        let moved = at.saturating_sub(their_delete).saturating_add(added);
        return (moved, delete, insert);
    }
    // Ours starts in what theirs replaced: it goes after their insert, deleting only what theirs
    // left of its range.
    if at >= their_at {
        let after = their_at.saturating_add(added);
        return (after, end.saturating_sub(their_end), insert);
    }
    // Ours starts before theirs and reaches into it: ours deletes from its start to theirs, and on
    // past theirs as far as it reached; their insert stays, after ours.
    let past = end.saturating_sub(their_end);
    let reach = their_at
        .saturating_sub(at)
        .saturating_add(added)
        .saturating_add(past);
    (at, reach, insert + &their_insert)
}

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

/// The text a bound control holds `value` as: an actor as [`ui::component::actor_text`] spells
/// it, so the edit it reports reads back as the same actor; anything else as it shows.
fn bound_text(value: &Value) -> String {
    match value {
        Value::Actor(who) => ui::component::actor_text(&who.id),
        Value::Opt(Some(inner)) => bound_text(inner),
        other => crate::reconcile::display(other),
    }
}

/// Whether a chord as the app published it, or the person moved it, is the press `pressed`, matched
/// as the toolkit matches every shortcut: one key in one spelling, read through the keyboard in use.
fn same_press(chord: &str, pressed: &ui::platform::Shortcut) -> bool {
    ui::platform::Shortcut::from(chord).same_press(pressed)
}

/// Show what each bound control under `node` is bound to, as `bound` holds it by key in the
/// composed tree (each of `node`'s keys carrying `prefix` there), reporting whether any control was
/// given a value other than the one it showed.
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
            // What the control binds, as its description declares it: a number control shows a
            // field of whole numbers, and a value a field holds as optional is its value.
            let bound_ty = crate::adapter::description_of(node)
                .and_then(|description| description.bindings.first())
                .map(|binding| binding.ty.clone());
            let number = matches!(bound_ty, Some(tilo_render_protocol::patch::PropType::Float));
            let flag = matches!(bound_ty, Some(tilo_render_protocol::patch::PropType::Bool))
                || node.kind == "checkbox"
                || node.kind == "toggle"
                || node.kind == crate::adapter::kind("ListRow");
            let mut value = value;
            while let Value::Opt(Some(inner)) = value {
                value = &**inner;
            }
            changed |= match value {
                Value::Bool(checked) => reconciler.set_flag(&key, *checked),
                Value::Int(number_of) if flag => reconciler.set_flag(&key, *number_of != 0),
                Value::Int(number_of) if number =>
                {
                    #[expect(
                        clippy::cast_precision_loss,
                        reason = "a setting's whole number is far inside f64"
                    )]
                    reconciler.set_number(&key, *number_of as f64)
                }
                Value::Int(number_of) => reconciler.set_count(&key, *number_of),
                Value::Float(number_of) => reconciler.set_number(&key, *number_of),
                Value::Rich(rich) => reconciler.set_rich(&key, rich),
                // A field that holds none, in a control that can show none: a switch is off and a
                // text is empty; a number keeps what it showed.
                Value::Opt(None) if flag => reconciler.set_flag(&key, false),
                Value::Opt(None) if number => false,
                Value::Opt(None) => reconciler.set_text(&key, ""),
                other => reconciler.set_text(&key, &bound_text(other)),
            };
        }
    }
    for child in &node.children {
        changed |= show_bound(child, prefix, bound, reconciler);
    }
    changed
}

/// The window stands for its body in the layout and in every walk of the widgets (it adds no node
/// of its own, so a walk that pairs widgets with layout nodes meets the body's children under it),
/// taking the person's input and its own changes on the way. How much a view's size has to change,
/// in rem, before the app is told: less moves no class boundary and no size a view would show
/// differently.
const SPACE_STEP_REM: f32 = 0.125;

impl AppWindow {
    /// Tell the app the space each view is offered where layout placed it, when it changed: a view
    /// reads `space` from its own container (layout.md, "Responding to space"). The app decides
    /// the classes, with their hysteresis, and runs a view again only when what it read of it
    /// changed. A level's own view is measured every layout; each view call the app declared as
    /// reading its space ([`ToUi::Spaces`]) is measured too.
    ///
    /// The space is the room layout offers the call ([`ui::widget::offered_room`]), never the size
    /// of what it drew: a call packed by its content (a part in a bar) reads the room its parent
    /// has, so drawing less never makes it read less and draw less again.
    fn follow_spaces(&mut self, taffy: &ui::taffy::TaffyTree<()>, body: ui::taffy::NodeId) {
        if self.rem <= 0.0 {
            return;
        }
        let mut wanted: [Vec<(String, String)>; 2] = [Vec::new(), Vec::new()];
        for ((level, slots), readers) in [Level::Root, Level::Detail]
            .into_iter()
            .zip(&mut wanted)
            .zip([&self.root_readers, &self.detail_readers])
        {
            let root = match level {
                Level::Root => self.root.as_ref().map(|view| view.tree.key.clone()),
                Level::Detail => self
                    .detail
                    .as_ref()
                    .map(|view| composed_key(Level::Detail, &view.tree.key)),
            };
            if let Some(key) = root {
                slots.push((String::new(), key));
            }
            for key in readers {
                slots.push((key.clone(), composed_key(level, key)));
            }
        }
        if wanted.iter().all(Vec::is_empty) {
            return;
        }
        let rects = widget_rects(self.body.as_ref());
        let covered = self.call_nodes(&wanted);
        // The nodes of the build this layout solved: another build's are not this tree's.
        let nodes = self
            .laid_nodes
            .as_ref()
            .filter(|(built, _)| *built == body)
            .map(|(_, nodes)| nodes);
        let offered = |key: &str| {
            let node = *nodes?.get(key)?;
            // Only a widget this tree placed is offered its room here: a list places its rows
            // itself, and the size their nodes were solved to is not where they stand.
            let solved = taffy.layout(node).ok()?.size;
            let placed = rects
                .get(key)
                .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)?;
            if (f64::from(solved.width) - placed.width()).abs() > 1.0
                || (f64::from(solved.height) - placed.height()).abs() > 1.0
            {
                return None;
            }
            let room = ui::widget::offered_room(taffy, node);
            Some((f64::from(room.width), f64::from(room.height)))
        };
        let mut told: Vec<(Level, String, (f32, f32, f32))> = Vec::new();
        for (level, slots) in [Level::Root, Level::Detail].into_iter().zip(&wanted) {
            let spaces = match level {
                Level::Root => &mut self.root_space,
                Level::Detail => &mut self.detail_space,
            };
            for (key, in_tree) in slots {
                // The box layout gave the call, or, where a row or a part takes the call's
                // nodes for its own, the room they take together.
                let placed =
                    |rect: &ui::vello::kurbo::Rect| rect.width() > 0.0 && rect.height() > 0.0;
                let rect = rects.get(in_tree).copied().filter(placed).or_else(|| {
                    covered
                        .get(in_tree)?
                        .iter()
                        .filter_map(|node| rects.get(node).copied())
                        .filter(placed)
                        .reduce(|a, b| a.union(b))
                });
                let Some(rect) = rect else {
                    // A view call layout has not placed yet: measured again after the next one.
                    continue;
                };
                // The room offered the call's own box, or, where its nodes are a row's or a
                // part's, the most room any of them is offered. A node no build of this tree
                // made (a list's row, laid out in a tree of its own) is offered what it was given.
                let room = offered(in_tree).or_else(|| {
                    covered
                        .get(in_tree)?
                        .iter()
                        .filter_map(|node| offered(node))
                        .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)))
                });
                let (width, height) = room.unwrap_or_else(|| (rect.width(), rect.height()));
                let space = (width as f32 / self.rem, height as f32 / self.rem, self.rem);
                let moved = spaces.get(key).is_none_or(|before| {
                    (before.0 - space.0).abs() >= SPACE_STEP_REM
                        || (before.1 - space.1).abs() >= SPACE_STEP_REM
                        // The rem is the step a view's size is told in: a root font size that
                        // moved re-measures every view, whatever its own space became.
                        || before.2.to_bits() != space.2.to_bits()
                });
                if moved {
                    spaces.insert(key.clone(), space);
                    told.push((level, key.clone(), space));
                }
            }
            spaces.retain(|key, _| slots.iter().any(|(wanted, _)| wanted == key));
        }
        self.out.extend(
            told.into_iter()
                .map(|(level, key, (width, height, rem))| ToApp::Space {
                    level,
                    key,
                    width,
                    height,
                    rem,
                }),
        );
    }

    /// The nodes each declared view call drew, by the call's key in the window's tree: what to
    /// measure where layout kept no box for the call itself. A row or a grid takes a call's nodes
    /// as its items, and a component takes them as a part of its own, so the call's room there is
    /// the room of what it drew.
    fn call_nodes(&self, wanted: &[Vec<(String, String)>; 2]) -> HashMap<String, Vec<String>> {
        fn walk(
            node: &UiNode,
            current: Option<&str>,
            calls: &HashSet<String>,
            out: &mut HashMap<String, Vec<String>>,
        ) {
            let mut current = current;
            if node.kind == "fragment" && calls.contains(&node.key) {
                out.entry(node.key.clone()).or_default();
                current = Some(node.key.as_str());
            }
            if let Some(call) = current
                && let Some(nodes) = out.get_mut(call)
            {
                nodes.push(node.key.clone());
            }
            for child in &node.children {
                walk(child, current, calls, out);
            }
        }
        let calls: HashSet<String> = wanted
            .iter()
            .flatten()
            .map(|(_, in_tree)| in_tree.clone())
            .collect();
        let mut out = HashMap::new();
        for tree in self.built.as_slice() {
            walk(tree, None, &calls, &mut out);
        }
        for rows in self.rows.borrow().values() {
            let rows = rows.borrow();
            for key in rows.rendered() {
                if let Some(row) = rows.row(&key) {
                    walk(row, None, &calls, &mut out);
                }
            }
        }
        out
    }

    /// The app says which view calls of `level` read the space layout gives them: what the window
    /// measures after every layout, and no others.
    fn take_spaces(&mut self, level: Level, keys: Vec<String>) {
        let readers = match level {
            Level::Root => &mut self.root_readers,
            Level::Detail => &mut self.detail_readers,
        };
        if *readers == keys {
            return;
        }
        *readers = keys;
        self.structure_dirty = true;
    }
}

/// The layout node of every widget of `window` that answers to a key, among the nodes `build_layout`
/// just made under `root` (`node_map` names the widget each stands for): where the room a view call
/// is offered is read from. Widgets are matched by address, never read through the map.
fn keyed_nodes(
    window: &dyn Widget,
    taffy: &ui::taffy::TaffyTree<()>,
    root: ui::taffy::NodeId,
    node_map: &HashMap<ui::taffy::NodeId, *mut dyn Widget>,
) -> HashMap<String, ui::taffy::NodeId> {
    fn keys<'a>(widget: &'a dyn Widget, out: &mut HashMap<*const (), &'a str>) {
        if let Some(id) = widget.test_id() {
            out.entry(std::ptr::from_ref(widget).cast::<()>())
                .or_insert(id);
        }
        for child in widget.children() {
            keys(child, out);
        }
    }
    let mut by_address = HashMap::new();
    keys(window, &mut by_address);
    let mut out = HashMap::new();
    let mut stack = vec![root];
    while let Some(node) = stack.pop() {
        if let Some(widget) = node_map.get(&node)
            && let Some(key) = by_address.get(&widget.cast_const().cast::<()>())
        {
            out.entry((*key).to_owned()).or_insert(node);
        }
        stack.extend(taffy.children(node).unwrap_or_default());
    }
    out
}

/// Every widget of `window` that answers to a key, with the rect layout gave it: where a view call
/// is placed, read from the layout that placed it.
fn widget_rects(window: &dyn Widget) -> HashMap<String, ui::vello::kurbo::Rect> {
    fn walk(widget: &dyn Widget, out: &mut HashMap<String, ui::vello::kurbo::Rect>) {
        if let Some(id) = widget.test_id() {
            out.insert(id.to_owned(), widget.get_rect());
        }
        for child in widget.children() {
            walk(child, out);
        }
    }
    let mut out = HashMap::new();
    walk(window, &mut out);
    out
}

/// The event a view's window frame declares where its search writes: what is typed in the window's
/// bar narrows what the frame lists, as a place chosen in its sidebar does.
const SEARCH: &str = "search";

impl AppWindow {
    fn searches(&self) -> bool {
        self.composed()
            .is_some_and(|shell| shell.events.contains_key(SEARCH))
    }

    /// How many rows the window's list shows now: the rows of the first list its views show, a
    /// tree's rows counting as a list's do.
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

    /// Whether the window's search narrows what the frame lists: the frame says where it goes. A
    /// document in the window is found in as well, so the one search does both.
    fn offers_search(&self) -> bool {
        self.searches()
    }

    /// Type `text` into the window's search: the frame narrows what it lists, as a place chosen in
    /// the sidebar narrows it. Closing the search widens it again.
    fn narrow_to(&mut self, text: &str) {
        let Some(shell) = self.composed() else {
            return;
        };
        if !shell.events.contains_key(SEARCH) {
            return;
        }
        let (level, key) = level_of(&shell.key);
        if text.is_empty() {
            self.search_total = None;
        } else if self.search_total.is_none() {
            // The rows the list had before anything was typed: what the search counts of.
            self.search_total = self.shown_rows();
        }
        self.out.push(ToApp::Fire {
            level,
            key,
            event: SEARCH.to_owned(),
            value: Some(Value::text(text)),
            gesture: None,
        });
    }
}

/// The window's search narrows what the views list where their frame declares it: the app hears the
/// text as the frame's `search` event and narrows its query, so the list beside the person narrows
/// in place, found by the same search the store answers every text field from.
impl ui::widget::Queryable for AppWindow {
    fn set_query_text(&mut self, text: &str) {
        self.narrow_to(text);
    }

    fn query_closed(&mut self) {
        self.narrow_to("");
    }

    /// The rows the list shows now, of the rows it held before anything was typed. The app answers
    /// a search a frame after it hears it, so the window reads the count off the frame the app
    /// sent, again each time the search asks.
    fn query_count(&self) -> Option<ui::widget::FilterCount> {
        let shown = self.shown_rows()?;
        Some(ui::widget::FilterCount::new(
            shown,
            self.search_total.unwrap_or(shown).max(shown),
        ))
    }
}

/// A window is on its way until its app sent the view it opens with: the keys a person types
/// meanwhile wait for that view (a shortcut the app declares, a field it puts the keyboard in),
/// and go to it once it stands. A part of a shell waits for nothing of the shell's.
impl ui::widget::AwaitsContent for AppWindow {
    fn awaits_content(&self) -> bool {
        self.shape == Shape::Window && self.root.is_none() && self.failed.is_none()
    }
}

/// The window bar's History shows the system's history of what the window shows: the app's
/// instance draws it in the window's page, and Back or a second press closes it. A window offers it
/// once it shows something.
impl ui::widget::WindowHistory for AppWindow {
    fn set_window_presents_history(&mut self, _tab_presents: bool) {}

    fn window_history_control(&self) -> Option<ui::widget::HistoryControl> {
        self.root.is_some().then_some(self.history)
    }

    fn invoke_window_history_control(
        &mut self,
        control: ui::widget::HistoryControl,
    ) -> Result<(), String> {
        self.tell(ToApp::History {
            open: !control.is_open(),
        });
        Ok(())
    }
}

/// The fullscreen intent a view declares, at any depth: a player sits inside the shell's
/// containers, so the window asks down the tree and reports the first intent it finds to the
/// host, which fullscreens the window.
fn fullscreen_intent_of(root: &dyn Widget) -> Option<bool> {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::FullscreenProvider>(root)
        && let Some(intent) = provider.fullscreen_intent()
    {
        return Some(intent);
    }
    let mut found = None;
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = fullscreen_intent_of(child);
        }
    });
    found
}

impl ui::widget::FullscreenProvider for AppWindow {
    fn fullscreen_intent(&self) -> Option<bool> {
        fullscreen_intent_of(self.body.as_ref())
    }

    fn fullscreen_params(&self) -> ui::fullscreen::FullscreenRequest {
        fullscreen_params_of(self.body.as_ref()).unwrap_or_else(|| {
            ui::fullscreen::FullscreenRequest::mirror(ui::fullscreen::FullscreenTarget::Largest)
        })
    }
}

/// The request the view's fullscreen intent carries, wherever it stands in the tree: which
/// screen a presentation fills, and whether it is the window or a surface of its own.
fn fullscreen_params_of(root: &dyn Widget) -> Option<ui::fullscreen::FullscreenRequest> {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::FullscreenProvider>(root)
        && provider.fullscreen_intent().is_some()
    {
        return Some(provider.fullscreen_params());
    }
    let mut found = None;
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = fullscreen_params_of(child);
        }
    });
    found
}

/// A player or a page in the view asked for picture-in-picture, at whatever depth the view's
/// containers nest it: the window answers for its tree and names the app's own picture as the
/// floating surface's source, so the surface shows the live video where it stands, with the role
/// a small video above the window has.
impl ui::widget::FloatingSurfaceProvider for AppWindow {
    fn take_floating_surface_request(&mut self) -> Option<FloatingSurfaceRequest> {
        let ask = picture_in_picture_of(self.body.as_ref())?;
        let aspect = ask.aspect;
        Some(FloatingSurfaceRequest {
            id: self.pip_surface_id(),
            title: ask.title,
            role: FloatingRole::PictureInPicture,
            source: FloatingSource::AppElement {
                app_id: self.app_id.clone(),
                element_id: "video_surface".to_owned(),
                region: ask.region,
            },
            preferred_size: ui::kurbo::Size::new(360.0, 360.0 / aspect),
            aspect_ratio: Some(aspect),
            initial_rect: None,
            presentation_rect: None,
            tracking_source: None,
        })
    }

    /// The surface this window's picture has while the view floats it. A picture the person
    /// brought back stops being one, and the host returns it to what draws it.
    fn active_floating_surface_ids(&self) -> Vec<String> {
        picture_in_picture_floating(self.body.as_ref())
            .then(|| self.pip_surface_id())
            .into_iter()
            .collect()
    }

    fn on_floating_surface_host_message(&mut self, message: &FloatingSurfaceHostMessage) {
        match message {
            FloatingSurfaceHostMessage::PrepareSurface {
                id,
                rect,
                content_rect,
                ..
            } if id == &self.pip_surface_id() => {
                if let (Ok(rect), Ok(content)) =
                    (protocol_rect(*rect), protocol_rect(*content_rect))
                {
                    self.pip_window = Some(PipWindow {
                        rect,
                        content_rect: content,
                    });
                }
            }
            FloatingSurfaceHostMessage::Closed { id }
            | FloatingSurfaceHostMessage::Reattach { id }
                if id == &self.pip_surface_id() =>
            {
                self.pip_window = None;
                self.pip_region_told = None;
                picture_in_picture_returned(self.body.as_mut());
            }
            _ => {}
        }
    }

    /// The floating window the shell stands for this window's picture, at the place it was told.
    fn floating_surface_presentations(&self) -> Vec<FloatingSurfacePresentation> {
        let Some(window) = self.pip_window else {
            return Vec::new();
        };
        if !picture_in_picture_floating(self.body.as_ref()) {
            return Vec::new();
        }
        vec![FloatingSurfacePresentation {
            id: self.pip_surface_id(),
            content_rect: window.content_rect,
            window_rect: window.rect,
            input_rect: None,
            returning_to_app: false,
        }]
    }

    /// The picture, drawn into the floating window the shell stood for it.
    fn draw_floating_surface(&mut self, id: &str, ctx: &mut ui::draw::DrawCtx<'_>) -> bool {
        if id != self.pip_surface_id() {
            return false;
        }
        let Some(window) = self.pip_window else {
            return false;
        };
        picture_in_picture_drawn(self.body.as_mut(), window.content_rect, ctx)
    }

    /// Where the picture stands now, once it moved: a page that scrolled or a window that resized
    /// carries the floated surface with it. A picture the shell stands nowhere has nothing to
    /// move, and one that stands where it did is not told again.
    fn take_floating_surface_source_updates(&mut self) -> Vec<(String, ui::kurbo::Rect)> {
        let region = self
            .pip_window
            .is_some()
            .then(|| picture_in_picture_region(self.body.as_ref()))
            .flatten();
        if region == self.pip_region_told {
            return Vec::new();
        }
        self.pip_region_told = region;
        region
            .map(|region| vec![(self.pip_surface_id(), region)])
            .unwrap_or_default()
    }

    /// The picture is dragged where the person wants it, and the card the shell's own keeps its
    /// buttons to itself.
    fn floating_surface_moves_on_drag(&self, id: &str) -> bool {
        id == self.pip_surface_id()
    }
}

impl AppWindow {
    /// The floating surface the view's picture stands on: one per window, whatever floats it.
    fn pip_surface_id(&self) -> String {
        format!("{app}:picture-in-picture", app = self.app_id)
    }
}

/// The shell's window for the view's picture went away, at any depth: what kept the float is
/// done, and the picture draws in its own window again.
fn picture_in_picture_returned(root: &mut dyn Widget) {
    if let Some(provider) =
        ui::widget::capability::capability_mut::<dyn ui::widget::PictureInPictureProvider>(root)
    {
        provider.picture_in_picture_returned();
    }
    for child in root.children_mut() {
        picture_in_picture_returned(child);
    }
}

/// Where the picture the view floats stands in its window now, at any depth: the region the
/// floated surface mirrors.
fn picture_in_picture_region(root: &dyn Widget) -> Option<ui::kurbo::Rect> {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::PictureInPictureProvider>(root)
        && let Some(region) = provider.floating_picture_in_picture_region()
    {
        return Some(region);
    }
    let mut found = None;
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = picture_in_picture_region(child);
        }
    });
    found
}

/// Draw the picture the view floats into the floating window, at any depth: the provider that
/// reports where the picture stands draws it.
fn picture_in_picture_drawn(
    root: &mut dyn Widget,
    target: ui::kurbo::Rect,
    ctx: &mut ui::draw::DrawCtx<'_>,
) -> bool {
    if let Some(provider) =
        ui::widget::capability::capability_mut::<dyn ui::widget::PictureInPictureProvider>(root)
        && let Some(source) = provider.floating_picture_in_picture_region()
        && provider.draw_picture_in_picture(source, target, ctx)
    {
        return true;
    }
    for child in root.children_mut() {
        if picture_in_picture_drawn(child, target, ctx) {
            return true;
        }
    }
    false
}

/// Whether the view floats a picture now, at any depth: a page's video or a player's, which the
/// app keeps floating until its own control takes it back.
fn picture_in_picture_floating(root: &dyn Widget) -> bool {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::PictureInPictureProvider>(root)
        && provider.picture_in_picture_floating()
    {
        return true;
    }
    let mut found = false;
    root.for_each_child(&mut |child| {
        if !found {
            found = picture_in_picture_floating(child);
        }
    });
    found
}

/// Whether a player in the view has a picture-in-picture request waiting, at any depth.
fn picture_in_picture_wanted(root: &dyn Widget) -> bool {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::PictureInPictureProvider>(root)
        && provider.wants_picture_in_picture()
    {
        return true;
    }
    let mut found = false;
    root.for_each_child(&mut |child| {
        if !found {
            found = picture_in_picture_wanted(child);
        }
    });
    found
}

/// The player in the view that asked for picture-in-picture, at any depth: a player sits inside
/// the shell's containers, so the window asks down the tree for the one that has a request.
fn picture_in_picture_of(root: &dyn Widget) -> Option<PictureInPictureAsk> {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::PictureInPictureProvider>(root)
        && let Some(ask) = provider.take_picture_in_picture()
    {
        return Some(ask);
    }
    let mut found = None;
    // Many children may hold a player; the one that asked is the only one with a request, and
    // asking each in turn takes it at most once.
    root.for_each_child(&mut |child| {
        if found.is_none() {
            found = picture_in_picture_of(child);
        }
    });
    found
}

/// The sound every player in the view decoded since it was last taken, at whatever depth the
/// view's containers nest it: the window answers for its tree, and the host plays the stream in
/// the places the session's outputs name.
fn audio_of(root: &dyn Widget, samples: &mut Vec<i16>) {
    if let Some(provider) =
        ui::widget::capability::capability::<dyn ui::widget::AudioTapProvider>(root)
    {
        samples.extend(provider.take_audio());
    }
    root.for_each_child(&mut |child| audio_of(child, samples));
}

impl ui::widget::Decorator for AppWindow {
    type Inner = dyn Widget;

    /// A view that declared `protect capture` is left out of captures and streams wherever the
    /// window shows it.
    fn withholds_captures(&self) -> bool {
        self.capture_protected()
    }

    // The window is the publisher of the app's actions and the one that answers choosing one
    // ([`crate::catalogue`]), and it answers for the views' search where their frame says where it
    // goes; everything else is the body's and forwarded to it.
    fn provide<'a>(&'a self, request: &mut ui::widget::capability::Request<'a, '_>) {
        if request.wants::<dyn ui::widget::Queryable>() && self.offers_search() {
            request.provide::<dyn ui::widget::Queryable>(self);
            return;
        }
        if request.wants::<dyn ui::widget::resources::PublishesResources>() {
            request.provide::<dyn ui::widget::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::host_operation::ResourceOperations>() {
            request.provide::<dyn ui::host_operation::ResourceOperations>(self);
            return;
        }
        if request.wants::<dyn ui::widget::AwaitsContent>() {
            request.provide::<dyn ui::widget::AwaitsContent>(self);
            return;
        }
        if request.wants::<dyn ui::widget::WindowHistory>() && self.shape == Shape::Window {
            request.provide::<dyn ui::widget::WindowHistory>(self);
            return;
        }
        // A player in the view asks the window for fullscreen, however the view's containers
        // nest it: the window answers the host's one question, and the host fullscreens it.
        if request.wants::<dyn ui::widget::FullscreenProvider>()
            && fullscreen_intent_of(self.body.as_ref()).is_some()
        {
            request.provide::<dyn ui::widget::FullscreenProvider>(self);
            return;
        }
        // Likewise a player's picture-in-picture: the window floats the picture the view asked
        // for, however deeply its containers nest it.
        if request.wants::<dyn ui::widget::FloatingSurfaceProvider>()
            && picture_in_picture_wanted(self.body.as_ref())
        {
            request.provide::<dyn ui::widget::FloatingSurfaceProvider>(self);
            return;
        }
        self.body.provide(request);
    }

    fn provide_mut<'a>(&'a mut self, request: &mut ui::widget::capability::RequestMut<'a, '_>) {
        if request.wants::<dyn ui::widget::Queryable>() && self.offers_search() {
            request.provide::<dyn ui::widget::Queryable>(self);
            return;
        }
        if request.wants::<dyn ui::widget::resources::PublishesResources>() {
            request.provide::<dyn ui::widget::resources::PublishesResources>(self);
            return;
        }
        if request.wants::<dyn ui::host_operation::ResourceOperations>() {
            request.provide::<dyn ui::host_operation::ResourceOperations>(self);
            return;
        }
        if request.wants::<dyn ui::widget::WindowHistory>() && self.shape == Shape::Window {
            request.provide::<dyn ui::widget::WindowHistory>(self);
            return;
        }
        if request.wants::<dyn ui::widget::FullscreenProvider>()
            && fullscreen_intent_of(self.body.as_ref()).is_some()
        {
            request.provide::<dyn ui::widget::FullscreenProvider>(self);
            return;
        }
        if request.wants::<dyn ui::widget::FloatingSurfaceProvider>()
            && picture_in_picture_wanted(self.body.as_ref())
        {
            request.provide::<dyn ui::widget::FloatingSurfaceProvider>(self);
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
        // A width class a layout found builds the body again, with what its empty states offer
        // there ([`Self::standing`]): before the tree is built is where the body may be
        // replaced.
        // A command the person moved since the controls were drawn shows its new keys on them.
        if self.compose_at.take().is_some() || self.moves_undrawn() {
            self.compose();
        }
        self.rem = ui::widget::arrange::rem_of(theme);
        let node = self.body.build_layout(taffy, theme, node_map);
        self.laid_nodes = Some((node, keyed_nodes(self.body.as_ref(), taffy, node, node_map)));
        node
    }

    // A remount changes the body's tree in place under the node it keeps: a keyed widget it moved
    // or replaced stands at a new node, and the one it left is gone from the tree.
    fn remount_children(&mut self, remount: &mut ui::widget::Remount<'_>) -> bool {
        let applied = self.body.remount_children(remount);
        if let Some((body, _)) = &self.laid_nodes {
            let body = *body;
            let (taffy, node_map) = remount.laid_out();
            self.laid_nodes = Some((body, keyed_nodes(self.body.as_ref(), taffy, body, node_map)));
        }
        applied
    }

    fn apply_layout(
        &mut self,
        taffy: &ui::taffy::TaffyTree<()>,
        node: ui::taffy::NodeId,
        origin: ui::kurbo::Point,
    ) {
        self.body.apply_layout(taffy, node, origin);
        self.follow_spaces(taffy, node);
        // Where the person is stays the window's across the shell being built anew: the column the
        // narrow stack has in front, as they left it (Back, a place, a row).
        if let Some(shell) = self.shell() {
            self.front = Some(shell.front_pane());
        }
        self.front_the_chooser();
        // What an empty state offers follows the width: where the sidebar is a screen of its own
        // it carries the primary action, and the body stands beside it at a wider width. A change
        // of that builds the body again.
        let standing = self
            .shell()
            .map_or(SidebarPlacement::None, |shell| shell.geometry().sidebar);
        if standing != self.standing {
            self.standing = standing;
            self.compose_at = Some(standing);
            self.structure_dirty = true;
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
            // A drag this window gave the desktop ended somewhere else: what is dropped here
            // later comes from another app, even the same files.
            ui::widget::drag_bus::forget_offer();
        }
        // What the person typed while the keyboard was on its way to the field a press opened goes
        // there first, before the input this event brings.
        self.release_landing();
        // Focus moves on a key, a press or a touch, never on a pointer that only moves.
        if matches!(
            event,
            WindowEvent::KeyboardInput { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::Touch(_)
                | WindowEvent::Ime(_)
        ) {
            self.keys.input = true;
        }
        let modifiers = ctx.modifiers.state();
        self.held = wire::Held {
            shift: modifiers.shift_key(),
            command: ui::PlatformConventions::default().command_modifier_down(modifiers),
            alt: modifiers.alt_key(),
        };
        if let WindowEvent::Ime(ime) = event {
            let composing =
                matches!(ime, ui::winit::event::Ime::Preedit(text, _) if !text.is_empty());
            if composing != self.keys.composing {
                self.keys.composing = composing;
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
        // A key answers the control the keyboard is in, unless a control declares it (below):
        // where a question its call raises stands.
        if is_key_press(event) {
            self.note_key_control(self.focused_control().as_deref());
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
            // A file dropped where nothing of the app's own took it is the window's: it is kept,
            // and the app's view hears of it as the `File` its process reads, so a document
            // dropped anywhere on an app that takes one is not lost.
            // A drag of this window's own files let go where no target took them lands nowhere:
            // they are not taken in again as another app's.
            if !*ctx.handled
                && let WindowEvent::DroppedFile(path) = event
                && (ui::widget::drag_bus::came_back_nowhere(path)
                    || self.drop_on_window(std::slice::from_ref(path)))
            {
                *ctx.handled = true;
            }
            return;
        };
        match press {
            // The field the keyboard is in carries the key out as its own press does: Enter, which
            // submits what it holds there.
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
                self.note_key_control(Some(&node_key));
                self.act(UiEvent::Fire(node_key, event, None, None));
                self.take_events();
            }
            // A key the window answers for an action no control of its tree declares: the action
            // runs from its address, as choosing it in the palette does.
            Press::RunAction(address) => {
                self.invoke_action(address, None);
                self.take_events();
            }
        }
        *ctx.handled = true;
    }

    fn take_focus_request(&mut self) -> Option<String> {
        // No field: the view ended the keyboard's place in an editor (`blur()`), or the editor
        // now edits another record.
        if std::mem::take(&mut self.keys.blur_request) {
            return Some(String::new());
        }
        let request = self
            .focus_request
            .take()
            .or_else(|| self.body.take_focus_request());
        // The app places the keyboard now: what the person typed while it was on its way is the
        // field's it is arriving in.
        if request.is_some() {
            self.landing = None;
        }
        request
    }

    fn take_structure_dirty(&mut self) -> bool {
        let dirty = std::mem::take(&mut self.structure_dirty);
        self.body.take_structure_dirty() || dirty
    }

    /// The window's own changes (a shell built, a bar shown) rebuild; what its body describes (a
    /// column's content changed in place) stays described.
    fn take_structure_change(&mut self) -> ui::widget::StructureChange {
        let own = ui::widget::StructureChange::of(std::mem::take(&mut self.structure_dirty));
        own.join(self.body.take_structure_change())
    }
}

/// What a page's file input takes, as the window asks the host's picker for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageFiles {
    /// The chooser's own title, empty for the picker's.
    pub title: String,
    /// One file or more.
    pub several: bool,
    /// One folder, with what it holds.
    pub folder: bool,
    /// What the input accepts, a hint to the picker and never a rule the page may rely on.
    pub accept: Vec<String>,
}

/// The person's own picker, as the window needs it: the files an input takes, answered with
/// exactly the paths the person chose there, or none when they cancelled. A host implements it
/// over its chooser; the window never draws a picker of its own.
pub trait PagePicker: Send + Sync {
    fn pick(&self, files: &PageFiles) -> Vec<String>;
}

#[cfg(test)]
#[path = "window_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "motion_tests.rs"]
mod motion_tests;

#[cfg(test)]
#[path = "window_motion_tests.rs"]
mod part_motion_tests;

#[cfg(test)]
#[path = "swap_tests.rs"]
mod swap_tests;
