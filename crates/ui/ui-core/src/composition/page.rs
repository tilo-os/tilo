//! A composition as a web page: one renderer over the semantics every window already produces for
//! assistive technology, written as semantic HTML in the shared components' styles.
//!
//! Each widget is the element its role names: a button a `<button>`, a field an `<input>` or a
//! `<textarea>`, a heading an `<h2>`, a document an `<article>` of its blocks, a list a
//! `role=list` of its items. What it shows is its words (its name, its value), styled by the
//! page's stylesheet from the theme's tokens, so the browser lays the text out, selects it and
//! finds it. A widget whose look its words cannot say (an icon, a chart, a canvas, a control
//! whose name does not fit its box) shows a picture of its paint, inside the element its role
//! names and with what it holds that acts (a toolbar's tools) as elements over it where they stand.
//!
//! A container whose children stand where its layout node's style puts them is a CSS flex or
//! grid box with that style, and one whose children tile it (a shell's columns) a grid of the
//! tracks their edges make; one that places its children anywhere else places them at their
//! bounds.
//!
//! Every element the page acts on carries `data-p`, a short number that [`Page`] keeps for a
//! widget's path from one render to the next. What the person does on it comes back as that
//! node's accessibility action: `data-a` is the action (`activate`, `toggle`, `value`, `edit`,
//! `increment`, `decrement`), `data-t` the node's action target and `data-v` the value it sets.
//! [`Page::render`] answers the whole document body. [`Page::patch`] answers only the elements
//! that changed since the last render, each replacing the element with its `data-p`.

use std::collections::{BTreeMap, HashMap};
use std::fmt::Write as _;

pub use tilo_headless_schema::PageAction;

use super::{Composed, Composition};
use crate::a11y::{AccessNode, Action, Autofill, Role};

/// One page's render of a window: the numbers its elements carry, kept by
/// path, and the elements it last sent, so the next render answers only what
/// changed.
#[derive(Default)]
pub struct Page {
    ids: HashMap<String, u32>,
    paths: Vec<String>,
    last: Option<Element>,
    /// What the page's host shows in each well, by place (a window node naming the place it
    /// shows, [`well_place`]): the markup that stands where the compositor would show the
    /// place's window natively (a link to the app's own page). A well the host fills with
    /// nothing stays empty.
    pub wells: HashMap<String, String>,
    /// What the last render showed other than as the browser's own elements.
    shown: Shown,
}

/// What a page shows other than as the browser's own elements, by widget, with how many of each
/// it holds: `painted`, widgets shown as a picture of their paint, which stays empty where no
/// host draws it; `placed`, containers that place their children at rectangles of their own,
/// which the page places at those rectangles rather than flowing them.
#[derive(Clone, Debug, Default, PartialEq, Eq, serde::Serialize)]
#[crate::component::not_a_component(
    framework,
    "the page's record of what it drew, never a view a person places"
)]
pub struct Shown {
    pub painted: BTreeMap<String, usize>,
    pub placed: BTreeMap<String, usize>,
}

/// The accessibility action `action` is on the node its element stands for; `None` for the
/// page's own size, which the window is laid out at rather than acted on.
#[must_use]
pub fn semantic(action: PageAction) -> Option<Action> {
    Some(match action {
        PageAction::Activate => Action::Activate,
        PageAction::Toggle => Action::ToggleCheck,
        PageAction::Focus => Action::Focus,
        PageAction::Value(value) => Action::SetValue(value),
        PageAction::Select { anchor, focus } => Action::SetSelection {
            anchor: usize::try_from(anchor).unwrap_or(usize::MAX),
            focus: usize::try_from(focus).unwrap_or(usize::MAX),
        },
        PageAction::Increment => Action::Increment,
        PageAction::Decrement => Action::Decrement,
        PageAction::Resize { .. } => return None,
    })
}

/// One change to the page, by its elements' numbers (`data-p`): what [`Page::patch`] answers, a
/// host applying them in order. An element that comes or goes is put in or taken out alone, so
/// everything around it stays the element it was: its scroll, its focus, its motion.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Patch {
    /// The element numbered `id` becomes `html`.
    Replace { id: u32, html: String },
    /// `html` goes in as the element numbered `parent`'s child element `at`, counted once every
    /// [`Patch::Remove`] before it is applied.
    Insert { parent: u32, at: u32, html: String },
    /// The element numbered `id` is taken out.
    Remove { id: u32 },
}

/// One element of the rendered page: its opening tag, what it shows of its own,
/// and its keyed children.
#[derive(Clone, Debug, PartialEq)]
struct Element {
    id: Option<u32>,
    tag: &'static str,
    attrs: String,
    /// Markup before its children (its words, a field's value).
    own: String,
    children: Vec<Element>,
}

impl Element {
    fn new(tag: &'static str) -> Self {
        Self {
            id: None,
            tag,
            attrs: String::new(),
            own: String::new(),
            children: Vec::new(),
        }
    }

    fn attr(&mut self, name: &str, value: &str) {
        let _ = write!(self.attrs, " {name}=\"{}\"", escape(value));
    }

    fn flag(&mut self, name: &str) {
        let _ = write!(self.attrs, " {name}");
    }

    fn write(&self, out: &mut String) {
        out.push('<');
        out.push_str(self.tag);
        if let Some(id) = self.id {
            let _ = write!(out, " data-p=\"{id}\"");
        }
        out.push_str(&self.attrs);
        out.push('>');
        if VOID.contains(&self.tag) {
            return;
        }
        out.push_str(&self.own);
        for child in &self.children {
            child.write(out);
        }
        let _ = write!(out, "</{}>", self.tag);
    }

    fn html(&self) -> String {
        let mut out = String::new();
        self.write(&mut out);
        out
    }

    /// The elements of `self` that differ from `old`, outermost first: an element
    /// whose own markup or whose children changed is replaced whole. One with no
    /// number of its own is replaced as part of the nearest one that has.
    fn diff(&self, old: &Self, out: &mut Vec<Patch>) -> bool {
        if self.tag != old.tag
            || self.id != old.id
            || self.attrs != old.attrs
            || self.own != old.own
        {
            return false;
        }
        let same_children = self.children.len() == old.children.len()
            && self
                .children
                .iter()
                .zip(&old.children)
                .all(|(new, old)| new.id == old.id && new.tag == old.tag);
        if same_children {
            return Self::diff_children(self.children.iter().zip(&old.children), out);
        }
        // Children came or went (a menu opening over the window, a row added to a list): each is
        // put in or taken out by its number, and the ones that stayed are compared as they are.
        let Some(parent) = self.id else {
            return false;
        };
        let numbers = |children: &[Self]| {
            children
                .iter()
                .map(|child| child.id)
                .collect::<Option<std::collections::HashSet<u32>>>()
        };
        let (Some(new_numbers), Some(old_numbers)) =
            (numbers(&self.children), numbers(&old.children))
        else {
            return false;
        };
        let stayed_new = self
            .children
            .iter()
            .filter(|child| child.id.is_some_and(|id| old_numbers.contains(&id)));
        let stayed_old = old
            .children
            .iter()
            .filter(|child| child.id.is_some_and(|id| new_numbers.contains(&id)));
        if !stayed_new
            .clone()
            .zip(stayed_old.clone())
            .all(|(new, old)| new.id == old.id && new.tag == old.tag)
        {
            return false;
        }
        let mut inner = Vec::new();
        inner.extend(
            old.children
                .iter()
                .filter_map(|child| child.id)
                .filter(|id| !new_numbers.contains(id))
                .map(|id| Patch::Remove { id }),
        );
        if !Self::diff_children(stayed_new.zip(stayed_old), &mut inner) {
            return false;
        }
        for (at, child) in self.children.iter().enumerate() {
            if child.id.is_some_and(|id| !old_numbers.contains(&id)) {
                inner.push(Patch::Insert {
                    parent,
                    at: u32::try_from(at).unwrap_or(u32::MAX),
                    html: child.html(),
                });
            }
        }
        out.extend(inner);
        true
    }

    /// The changes within children that stand in the same places, each pair compared: `false`
    /// when one changed and has no number to be replaced by.
    fn diff_children<'a>(
        pairs: impl Iterator<Item = (&'a Self, &'a Self)>,
        out: &mut Vec<Patch>,
    ) -> bool {
        for (new, old) in pairs {
            if new == old {
                continue;
            }
            let mut inner = Vec::new();
            if new.diff(old, &mut inner) {
                out.extend(inner);
            } else if let Some(id) = new.id {
                out.push(Patch::Replace {
                    id,
                    html: new.html(),
                });
            } else {
                return false;
            }
        }
        true
    }
}

const VOID: [&str; 5] = ["input", "br", "hr", "img", "wbr"];

/// How a node shows on the page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Shows {
    /// Its words, in its role's element.
    Words,
    /// An editable field holding its value.
    Field,
    /// The nodes it publishes, each its own element.
    Structure,
    /// A picture of its paint, with what acts in it over it.
    Paint,
    /// A live picture the host decodes into the element's own canvas, as a call's tile shows.
    Call,
    /// The window of the place it names, which the page's host fills.
    Well,
}

/// Whether the widget `semantics` describes, standing at `rect` with no children laid out under
/// it, shows on a page as a picture of its paint: what its words cannot say.
#[must_use]
pub fn shows_paint(semantics: Option<&AccessNode>, rect: [f64; 4]) -> bool {
    rect[2] > 0.0 && rect[3] > 0.0 && shows(semantics, Some(rect[2])) == Shows::Paint
}

/// How the node `semantics` describes shows, for a widget `width` wide (`None` for a node with no
/// widget of its own, which has no paint).
fn shows(semantics: Option<&AccessNode>, width: Option<f64>) -> Shows {
    let Some(node) = semantics else {
        return Shows::Paint;
    };
    if well_place(node).is_some() {
        return Shows::Well;
    }
    let words = node_words(node);
    let fits = |text: &str| {
        width.is_none_or(|width| {
            let chars = text.chars().count().min(400);
            #[allow(clippy::cast_precision_loss, reason = "a count of at most 400")]
            let needs = chars as f64 * 6.5 + 8.0;
            needs <= width
        })
    };
    let structured = !node.children.is_empty();
    // A part that holds something drawn and controls standing on it, nothing else (a screenshot
    // being marked up), is its paint, with the controls over it.
    if width.is_some()
        && node.children.iter().any(drawn)
        && node
            .children
            .iter()
            .all(|child| drawn(child) || child.action_target.is_some() && child.bounds.is_some())
    {
        return Shows::Paint;
    }
    match node.role {
        Role::TextInput | Role::MultilineTextInput | Role::SpinButton if !node.state.read_only => {
            Shows::Field
        }
        Role::StaticText
        | Role::Label
        | Role::Heading { .. }
        | Role::Paragraph
        | Role::Tooltip
        | Role::TextInput
        | Role::MultilineTextInput
        | Role::SpinButton
        // A link is its words, wherever they fit or not, as any link on the web is.
        | Role::Link => {
            // A link's own controls (its Copy link) are what the browser's own menu offers on
            // an <a href>, so they never make it a container.
            let structured = if node.role == Role::Link {
                node.children.iter().any(|child| child.role != Role::Button)
            } else {
                structured
            };
            if structured {
                Shows::Structure
            } else if words.is_some() || width.is_none() {
                Shows::Words
            } else {
                Shows::Paint
            }
        }
        Role::List
        | Role::ListBox
        | Role::Table
        | Role::Tree
        | Role::Menu
        | Role::TabBar
        | Role::TabPanel
        | Role::Document
        | Role::Dialog
        | Role::Row
        | Role::Group
        | Role::Window
        | Role::Generic => {
            if structured || width.is_none() {
                Shows::Structure
            } else {
                Shows::Paint
            }
        }
        Role::Button
        | Role::ToggleButton
        | Role::MenuItem
        | Role::Tab
        | Role::ListItem
        | Role::ListBoxOption
        | Role::TreeItem
        | Role::Cell
        | Role::ComboBox => {
            if structured {
                Shows::Structure
            } else if words.is_some_and(|words| fits(&words.title)) || width.is_none() {
                Shows::Words
            } else {
                Shows::Paint
            }
        }
        Role::Checkbox
        | Role::RadioButton
        | Role::Switch
        | Role::Slider
        | Role::Separator
        | Role::ProgressBar
        | Role::Spinner
        | Role::Image
        | Role::Canvas
        | Role::Terminal
        | Role::Toolbar
        | Role::Unknown => {
            if width.is_none() {
                Shows::Words
            } else {
                Shows::Paint
            }
        }
    }
}

/// Whether a node is drawn and says nothing more of itself: a picture or a canvas with no parts.
fn drawn(node: &AccessNode) -> bool {
    matches!(node.role, Role::Image | Role::Canvas) && node.children.is_empty()
}

/// What a call's tile holds on a page: the canvas the host decodes the call's stream into, the
/// face that stands there until a picture arrives, and the name it wears with what the person
/// shows (their microphone off, the screen they share). The host marks the tile while it draws
/// frames (`data-picture`), which takes the face away, and its state while the person talks or
/// is muted (`data-speaking`, `data-muted`).
fn call_into(element: &mut Element, node: &Composed) {
    element.attr("class", "tilo-call");
    if let Some(live) = node.live.as_deref() {
        element.attr("data-live", &escape(live));
    }
    let name = node
        .semantics
        .as_ref()
        .and_then(|access| access.title.as_deref())
        .filter(|name| !name.trim().is_empty());
    if let Some(name) = name {
        let mut face = Element::new("span");
        face.attr("class", "tilo-call-face");
        face.attr("aria-hidden", "true");
        face.own
            .push_str(&escape(&crate::widget::initials_for_name(name)));
        element.children.push(face);
    }
    let mut picture = Element::new("canvas");
    picture.attr("class", "tilo-call-picture");
    picture.attr("aria-hidden", "true");
    element.children.push(picture);
    if let Some(name) = name {
        let mut pill = Element::new("span");
        pill.attr("class", "tilo-call-name");
        pill.attr("aria-hidden", "true");
        let screen = node
            .live
            .as_deref()
            .is_some_and(|live| live.ends_with("/screen"));
        if screen {
            pill.own
                .push_str(&crate::icons::glyph_svg(crate::icons::Icon::ScreenShare));
        }
        let mut muted = Element::new("span");
        muted.attr("class", "tilo-call-muted");
        muted
            .own
            .push_str(&crate::icons::glyph_svg(crate::icons::Icon::MicOff));
        pill.children.push(muted);
        let mut words = Element::new("span");
        words.own.push_str(&escape(name));
        pill.children.push(words);
        element.children.push(pill);
    }
}

/// The place whose window a node shows, which the page's host fills ([`Page::wells`]): a window
/// node inside the window, naming the place as the item it shows.
#[must_use]
pub fn well_place(node: &AccessNode) -> Option<&str> {
    (node.role == Role::Window)
        .then_some(node.item.as_deref())
        .flatten()
}

/// What a node says on a page: its title (the words it leads with) and what it says after them.
struct Words {
    title: String,
    detail: Option<String>,
}

fn node_words(node: &AccessNode) -> Option<Words> {
    let shown = match node.role {
        Role::StaticText | Role::Label | Role::Heading { .. } | Role::Paragraph => {
            node.value.as_ref().or(node.name.as_ref())
        }
        Role::ComboBox => node.value.as_ref().or(node.name.as_ref()),
        _ => node.name.as_ref().or(node.value.as_ref()),
    }?;
    let shown = shown.trim();
    if shown.is_empty() {
        return None;
    }
    // A row's name joins its title and the lines after it: the title leads, the rest is said
    // after it.
    let (title, rest) = match node.title.as_deref().map(str::trim) {
        Some(title) if !title.is_empty() && shown.starts_with(title) => {
            let rest = shown
                .strip_prefix(title)
                .unwrap_or_default()
                .trim_start_matches(|c: char| c == ',' || c.is_whitespace())
                .trim();
            (
                title.to_owned(),
                (!rest.is_empty()).then(|| rest.to_owned()),
            )
        }
        _ => (shown.to_owned(), None),
    };
    let detail = rest.or_else(|| {
        node.description
            .as_ref()
            .map(|description| description.trim().to_owned())
            .filter(|description| !description.is_empty() && *description != title)
    });
    Some(Words { title, detail })
}

/// Where a node's element sits among its parent's: in the flow, at its bounds from the
/// parent's corner, or over the window as a layer of its own.
#[derive(Clone, Copy)]
enum Place {
    Flow,
    At([f64; 2]),
    Over,
}

/// What an element in a document is: a field a person edits in place, as a document's text.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Context {
    Plain,
    Document,
}

impl Page {
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// The number a widget's element carries, for its `path`.
    fn id(&mut self, path: &str) -> u32 {
        if let Some(id) = self.ids.get(path) {
            return *id;
        }
        let id = u32::try_from(self.paths.len()).unwrap_or(u32::MAX);
        self.paths.push(path.to_owned());
        self.ids.insert(path.to_owned(), id);
        id
    }

    /// The path of the widget whose element carries `id`.
    #[must_use]
    pub fn path(&self, id: u32) -> Option<&str> {
        self.paths.get(id as usize).map(String::as_str)
    }

    /// What the person did on the element numbered `id` (the semantic `target` it carries),
    /// performed on `frame` as that node's accessibility action. An element the page numbers
    /// for a part of a widget (a document's block, a control) is acted on through the widget it
    /// is a part of.
    ///
    /// # Errors
    /// The page numbers no such element, `action` is the page's size rather than an action on
    /// an element, or the view refuses it.
    pub fn act(
        &self,
        frame: &mut crate::headless::Driver,
        id: u32,
        target: Option<&str>,
        action: PageAction,
    ) -> Result<(), String> {
        let path = self
            .path(id)
            .ok_or_else(|| format!("the page numbers no element {id}"))?;
        let widget = path.split_once('#').map_or(path, |(widget, _)| widget);
        let action = semantic(action)
            .ok_or_else(|| "a page's size is laid out, not acted on an element".to_owned())?;
        frame.perform_semantic_named(widget, target, &action)
    }

    /// What the last render or patch showed other than as the browser's own elements.
    #[must_use]
    pub fn shown(&self) -> &Shown {
        &self.shown
    }

    /// The whole page body for `composition`, which [`Self::patch`] later
    /// answers changes to.
    pub fn render(&mut self, composition: &Composition) -> String {
        let body = self.build(composition);
        let html = body.html();
        self.last = Some(body);
        html
    }

    /// What changed on the page since the last render: each element to replace, put in or take
    /// out, outermost first. With nothing rendered before, it is the whole body.
    pub fn patch(&mut self, composition: &Composition) -> Vec<Patch> {
        let body = self.build(composition);
        let mut patches = Vec::new();
        let whole = match &self.last {
            Some(old) => !body.diff(old, &mut patches),
            None => true,
        };
        if whole {
            patches = vec![Patch::Replace {
                id: body.id.unwrap_or_default(),
                html: body.html(),
            }];
        }
        self.last = Some(body);
        patches
    }

    fn build(&mut self, composition: &Composition) -> Element {
        self.shown = Shown::default();
        let mut body = Element::new("div");
        body.id = Some(self.id(""));
        body.attr("class", "tilo-view");
        let mut root = self.node(&composition.root, Place::Flow, Context::Plain);
        root.attr("data-root", "");
        body.children.push(root);
        for (index, layer) in composition.layers.iter().enumerate() {
            let mut over = Element::new("div");
            over.id = Some(self.id(&format!("#layer:{index}:{}", layer.path)));
            over.attr("data-layer", "");
            over.attr("style", &appear_style(layer));
            over.children
                .push(self.node(layer, Place::Over, Context::Plain));
            body.children.push(over);
        }
        if let Some(focus) = &composition.focus {
            let id = self.id(focus);
            body.attr("data-focus", &id.to_string());
        }
        body
    }

    /// A widget's element: its role's, laid out as its node is, holding its children or what it
    /// shows of its own.
    fn node(&mut self, node: &Composed, place: Place, context: Context) -> Element {
        let semantics = node.semantics.as_ref();
        let leaf = node.children.is_empty();
        let shows = if node.live.is_some() {
            // The host decodes the picture where the widget stands: the page carries the stream,
            // never a still of it.
            Shows::Call
        } else if leaf {
            if node.picture.is_some() {
                Shows::Paint
            } else {
                match shows(semantics, Some(node.rect[2])) {
                    // A widget the walk did not picture (one with no area) shows nothing.
                    Shows::Paint => Shows::Structure,
                    other => other,
                }
            }
        } else if semantics.and_then(well_place).is_some() {
            Shows::Well
        } else {
            Shows::Structure
        };
        let context = if semantics.is_some_and(|node| node.role == Role::Document) {
            Context::Document
        } else {
            context
        };
        let mut element = match (semantics, shows) {
            (Some(access), Shows::Field) => field(access, context),
            (Some(access), _) => Element::new(tag_of(access, shows, leaf)),
            (None, _) => Element::new("div"),
        };
        element.id = Some(self.id(&node.path));
        element.attr("data-w", &node.widget);
        let mut css = String::new();
        if let Some(layout) = &node.layout {
            if leaf {
                css.push_str(&sizing(layout));
            } else {
                css.push_str(layout);
            }
        } else if !leaf {
            css.push_str("display:flex;flex-direction:column;");
        }
        css.push_str("min-width:0;");
        match place {
            // A surface over the window whose rows the page draws itself (a menu, a list to pick
            // from) is as wide as its widest row set in the face the page draws, never narrower
            // than the app laid it out, and as tall as its rows: a width measured in another
            // renderer would cut its labels short.
            Place::Over if leaf && shows == Shows::Structure => {
                // It keeps the window's margin (`floating_surface::viewport_margin`, the menu's
                // inset) at the right edge, as the app places it.
                let [at_x, at_y, width, _] = node.rect;
                let _ = write!(
                    css,
                    "position:absolute;left:{at_x}px;top:{at_y}px;width:max-content;\
                     min-width:{width}px;max-width:calc(100% - {at_x}px - var(--tilo-menu-inset));\
                     margin:0;flex:none;",
                );
            }
            Place::At(_) | Place::Over => {
                let [x, y] = match place {
                    Place::At(corner) => corner,
                    _ => [0.0, 0.0],
                };
                let [at_x, at_y, width, height] = node.rect;
                let _ = write!(
                    css,
                    "position:absolute;left:{}px;top:{}px;width:{width}px;height:{height}px;\
                     margin:0;min-height:0;max-width:none;max-height:none;flex:none;",
                    at_x - x,
                    at_y - y,
                );
            }
            Place::Flow => {}
        }
        let places = !leaf && !node.flows;
        for (counts, held) in [
            (&mut self.shown.painted, shows == Shows::Paint),
            (&mut self.shown.placed, places),
        ] {
            if held {
                let count = counts.entry(node.widget.clone()).or_default();
                *count = count.saturating_add(1);
            }
        }
        let overlaid = shows == Shows::Paint && semantics.is_some_and(acts_within);
        if (places || overlaid) && matches!(place, Place::Flow) {
            css.push_str("position:relative;");
        }
        // What it places stands at points of its own, past its edges too: the box scrolls to
        // them.
        if places {
            css.push_str("overflow:auto;");
        } else if node.scroll.is_some() {
            css.push_str("overflow-y:auto;overflow-x:hidden;overscroll-behavior:contain;");
        } else if node.clip.is_some() {
            css.push_str("overflow:clip;");
        }
        if places || shows == Shows::Paint {
            // What it places stands at its own size, which a flow around it does not give it.
            let _ = write!(css, "min-height:{}px;", node.rect[3]);
        }
        if !css.is_empty() {
            element.attr("style", &css);
        }
        if let Some(access) = semantics {
            self.semantics_attrs(&mut element, access, shows);
            if shows != Shows::Field && pressable(access) && !followed(&element, access) {
                act(
                    &mut element,
                    press_of(access),
                    access.action_target.as_deref(),
                );
            }
        }
        match (shows, semantics) {
            (Shows::Well, Some(access)) => {
                if let Some(place) = well_place(access) {
                    element.attr("data-well", place);
                    if let Some(shown) = self.wells.get(place) {
                        element.own.clone_from(shown);
                    }
                }
            }
            (Shows::Words, Some(access)) => words_into(&mut element, access),
            (Shows::Call, _) => call_into(&mut element, node),
            (Shows::Paint, _) => {
                if let Some(data) = node
                    .picture
                    .as_ref()
                    .and_then(|picture| picture.data.clone())
                {
                    let mut picture = Element::new("img");
                    picture.attr("class", "tilo-paint");
                    picture.attr("src", &data);
                    picture.attr("alt", "");
                    element.children.push(picture);
                } else if let Some(glyph) = node.glyph.as_deref() {
                    // A glyph is a vector: stroked from the icon set's own sources where the host
                    // drew no picture of it.
                    if let Some(icon) = crate::icons::Icon::from_name(glyph) {
                        element.own.push_str(&crate::icons::glyph_svg(icon));
                    }
                } else if let Some(access) = semantics {
                    // Nothing drew its paint: what it says of itself is what the page shows.
                    caption_into(&mut element, access);
                }
                // What acts in what it paints stands over the picture where it is.
                if let Some(access) = semantics {
                    let corner = [node.rect[0], node.rect[1]];
                    let mut index = 0;
                    self.overlays(&mut element, &node.path, access, corner, &mut index);
                }
            }
            (Shows::Structure, Some(access)) if leaf => {
                // What its paint says above what it publishes, its words say on the page.
                caption_into(&mut element, access);
                for (index, child) in access.children.iter().enumerate() {
                    let key = format!("{}#{}", node.path, synthetic_key(child, index));
                    element.children.push(self.synthetic(&key, child, context));
                }
            }
            _ => {}
        }
        if !leaf {
            let flows = node.flows;
            let mut corner = [node.rect[0], node.rect[1]];
            if let Some([offset, _, _]) = node.scroll {
                // Its children stand where its content scrolled them; the page scrolls its own.
                corner[1] -= offset;
            }
            for child in &node.children {
                if child.rect[2] <= 0.0 || child.rect[3] <= 0.0 {
                    continue;
                }
                let place = if flows {
                    Place::Flow
                } else {
                    Place::At(corner)
                };
                element.children.push(self.node(child, place, context));
            }
        }
        element
    }

    /// A node a widget publishes with no widget of its own (a list's row, a document's block), at
    /// `key` under the widget's path, as the element its role names.
    fn synthetic(&mut self, key: &str, node: &AccessNode, context: Context) -> Element {
        let shows = shows(Some(node), None);
        let context = if node.role == Role::Document {
            Context::Document
        } else {
            context
        };
        let mut element = if shows == Shows::Field {
            field(node, context)
        } else {
            Element::new(tag_of(node, shows, node.children.is_empty()))
        };
        element.id = Some(self.id(key));
        self.semantics_attrs(&mut element, node, shows);
        if shows != Shows::Field
            && pressable(node)
            && node.action_target.is_some()
            && !followed(&element, node)
        {
            act(&mut element, press_of(node), node.action_target.as_deref());
        }
        if shows == Shows::Field {
            if let Some(target) = &node.action_target {
                element.attr("data-t", target);
            }
        } else if node.children.is_empty() || matches!(element.tag, "button" | "a") {
            if let Some(icon) = node.icon {
                // Its glyph stands before its words, as the app paints the row.
                element.own.push_str(&crate::icons::glyph_svg(icon));
            }
            words_into(&mut element, node);
        }
        if !matches!(element.tag, "button" | "a" | "input" | "textarea") {
            if !node.children.is_empty() {
                caption_into(&mut element, node);
            }
            for (index, child) in node.children.iter().enumerate() {
                let key = format!("{key}/{}", synthetic_key(child, index));
                element.children.push(self.synthetic(&key, child, context));
            }
        }
        element
    }

    /// Every node under `node` that acts, with where it stands, as a button over the picture of
    /// the widget that paints it.
    fn overlays(
        &mut self,
        element: &mut Element,
        path: &str,
        node: &AccessNode,
        corner: [f64; 2],
        index: &mut usize,
    ) {
        for child in &node.children {
            if let (Some(target), Some(bounds)) = (&child.action_target, child.bounds)
                && bounds.x1 > bounds.x0
                && bounds.y1 > bounds.y0
            {
                *index = index.saturating_add(1);
                let mut over = Element::new("button");
                over.id = Some(self.id(&format!("{path}#over:{target}")));
                over.attr("type", "button");
                over.attr("class", "tilo-over");
                over.attr(
                    "style",
                    &format!(
                        "left:{}px;top:{}px;width:{}px;height:{}px;",
                        bounds.x0 - corner[0],
                        bounds.y0 - corner[1],
                        bounds.x1 - bounds.x0,
                        bounds.y1 - bounds.y0
                    ),
                );
                self.semantics_attrs(&mut over, child, Shows::Paint);
                if !child.state.disabled {
                    act(&mut over, press_of(child), Some(target));
                }
                element.children.push(over);
            }
            self.overlays(element, path, child, corner, index);
        }
    }

    /// The element's role, name and state, for assistive technology and for the
    /// page's own find, focus and selection.
    #[allow(
        clippy::unused_self,
        reason = "the page's own attributes, beside the ones it numbers"
    )]
    fn semantics_attrs(&self, element: &mut Element, access: &AccessNode, shows: Shows) {
        let role = aria_role(access.role).filter(|role| !native(element.tag, role));
        let live = access.live.map(|live| match live {
            crate::a11y::Politeness::Assertive => "alert",
            crate::a11y::Politeness::Polite => "status",
        });
        let role = match (live, role) {
            (Some(live), None | Some("group")) => Some(live),
            (_, role) => role,
        };
        if let Some(role) = role
            && !matches!(element.tag, "input" | "textarea")
        {
            element.attr("role", role);
        }
        // A name the element does not show is told.
        let shown = shows == Shows::Words || matches!(element.tag, "input" | "textarea");
        if let Some(name) = &access.name
            && (!shown || matches!(element.tag, "input" | "textarea"))
            && !name.trim().is_empty()
        {
            element.attr("aria-label", name);
        }
        if let Some(description) = access.role_description.as_deref() {
            element.attr("aria-roledescription", description);
        }
        let state = &access.state;
        if state.selected {
            element.attr("aria-selected", "true");
        }
        match access.role {
            Role::ToggleButton => element.attr(
                "aria-pressed",
                if state.checked == Some(true) {
                    "true"
                } else {
                    "false"
                },
            ),
            Role::Checkbox | Role::Switch | Role::RadioButton if element.tag != "input" => {
                if state.mixed {
                    element.attr("aria-checked", "mixed");
                } else if let Some(checked) = state.checked {
                    element.attr("aria-checked", if checked { "true" } else { "false" });
                }
            }
            Role::Slider | Role::ProgressBar | Role::SpinButton => {
                if let Some(value) = &access.value {
                    element.attr("aria-valuetext", value);
                }
            }
            Role::Heading { level } if !element.tag.starts_with('h') => {
                element.attr("aria-level", &level.to_string());
            }
            _ => {}
        }
        if let Some(expanded) = state.expanded {
            element.attr("aria-expanded", if expanded { "true" } else { "false" });
        }
        if state.disabled {
            element.attr("aria-disabled", "true");
            if matches!(element.tag, "input" | "textarea") {
                element.flag("disabled");
            }
        }
        if state.busy {
            element.attr("aria-busy", "true");
        }
        if access.role == Role::Dialog && state.modal {
            element.attr("aria-modal", "true");
        }
        if let (Some(position), Some(size)) = (access.pos_in_set, access.set_size) {
            element.attr("aria-posinset", &position.to_string());
            element.attr("aria-setsize", &size.to_string());
        }
        if let Some(level) = access.level
            && matches!(access.role, Role::TreeItem | Role::ListItem | Role::Row)
        {
            element.attr("aria-level", &level.to_string());
        }
        if (state.focusable || pressable(access))
            && !state.disabled
            && !matches!(element.tag, "button" | "a" | "input" | "textarea")
        {
            element.attr("tabindex", "0");
        }
        if element.tag == "button" {
            element.attr("type", "button");
        }
        // A link goes where its address names, as any link on the web does.
        if element.tag == "a"
            && let Some(address) = address(access)
        {
            element.attr("href", address);
        }
    }
}

/// The key a node published without a widget stands at under its parent: its own key where it has
/// one, its place among its siblings otherwise.
fn synthetic_key(node: &AccessNode, index: usize) -> String {
    node.semantic_key
        .clone()
        .or_else(|| node.key.map(|key| format!("k{key:x}")))
        .unwrap_or_else(|| index.to_string())
}

/// The editable field a text node is: an `<input>`, a `<textarea>`, or in a document a block a
/// person edits in place.
fn field(access: &AccessNode, context: Context) -> Element {
    let value = access.value.clone().unwrap_or_default();
    let multiline = access.state.multiline || access.role == Role::MultilineTextInput;
    if context == Context::Document && !access.state.protected {
        let mut block = Element::new("div");
        block.attr("class", "tilo-block");
        block.attr("data-a", "edit");
        if let Some(name) = &access.name {
            block.attr("aria-label", name);
        }
        block.attr("role", "textbox");
        block.attr("aria-multiline", "true");
        block.own = if value.is_empty() {
            "<br>".to_owned()
        } else {
            escape(&value)
        };
        return block;
    }
    let mut element = Element::new(if multiline { "textarea" } else { "input" });
    if access.state.protected {
        element.attr("type", "password");
    } else if !multiline {
        element.attr("type", "text");
    }
    if access.role == Role::SpinButton {
        element.attr("inputmode", "decimal");
        element.attr("role", "spinbutton");
    }
    if let Some(autofill) = access.state.autofill {
        element.attr("autocomplete", autocomplete(autofill));
    }
    if let Some(name) = &access.name {
        element.attr("aria-label", name);
    }
    let placeholder = if value.is_empty() {
        access.description.as_ref().or(access.name.as_ref())
    } else {
        None
    };
    if let Some(placeholder) = placeholder {
        element.attr("placeholder", placeholder);
    }
    element.attr("class", "tilo-field");
    element.attr("data-a", "value");
    if access.state.read_only {
        element.flag("readonly");
    }
    if element.tag == "textarea" {
        element.attr("rows", "1");
        element.own = escape(&value);
    } else if !access.state.protected {
        element.attr("value", &value);
    } else if let Some(length) = access.value_length {
        element.attr("data-length", &length.to_string());
    }
    element
}

fn autocomplete(autofill: Autofill) -> &'static str {
    match autofill {
        Autofill::Username => "username",
        Autofill::Email => "email",
        Autofill::CurrentPassword => "current-password",
        Autofill::NewPassword => "new-password",
        Autofill::OneTimeCode => "one-time-code",
        Autofill::Name => "name",
        Autofill::Tel => "tel",
    }
}

/// A node's words in its element: its title, and what it says after it, dimmer.
fn words_into(element: &mut Element, access: &AccessNode) {
    let Some(words) = node_words(access) else {
        return;
    };
    if matches!(
        access.role,
        Role::Checkbox | Role::Switch | Role::RadioButton
    ) && element.tag == "label"
    {
        let _ = write!(
            element.own,
            "<input type=\"{}\"{}{} tabindex=\"-1\" aria-hidden=\"true\">",
            if access.role == Role::RadioButton {
                "radio"
            } else {
                "checkbox"
            },
            if access.role == Role::Switch {
                " role=\"switch\""
            } else {
                ""
            },
            if access.state.checked == Some(true) {
                " checked"
            } else {
                ""
            },
        );
    }
    let time = time_of(access);
    let detail = words
        .detail
        .filter(|_| !matches!(element.tag, "h1" | "h2" | "h3" | "h4" | "h5" | "h6"));
    // The time a node stands for is told on the words that say it: what it says after its title
    // (an event's hours), or its title (a day).
    let (title, detail) = match detail {
        Some(detail) => (escape(&words.title), Some(timed(&detail, time))),
        None => (timed(&words.title, time), None),
    };
    match detail {
        Some(detail) => {
            let _ = write!(
                element.own,
                "<span class=\"tilo-title\">{title}</span><span class=\"tilo-detail\">{detail}</span>"
            );
        }
        None if element.own.is_empty() => element.own = title,
        None => {
            let _ = write!(element.own, "<span class=\"tilo-title\">{title}</span>");
        }
    }
}

/// The ISO 8601 time a node's value stands for: a date, a date and time, or a span of two of them
/// (`2026-11-23T09:00/2026-11-23T09:30`).
fn time_of(access: &AccessNode) -> Option<&str> {
    access.value.as_deref().filter(|value| is_iso_time(value))
}

/// `words`, escaped, in a `<time>` that tells the browser, a reader and a script the time `time`
/// they say, when they say one.
fn timed(words: &str, time: Option<&str>) -> String {
    match time {
        Some(time) => format!(
            "<time datetime=\"{}\">{}</time>",
            escape(time),
            escape(words)
        ),
        None => escape(words),
    }
}

/// Whether `value` is an ISO 8601 date or date and time (`2026-11-23`, `2026-11-23T09:30`,
/// `2026-11-23T09:30:00Z`), or a span of two of them.
fn is_iso_time(value: &str) -> bool {
    let mut ends = value.split('/');
    let (Some(first), second, None) = (ends.next(), ends.next(), ends.next()) else {
        return false;
    };
    is_iso_instant(first) && second.is_none_or(is_iso_instant)
}

/// Whether `value` is one ISO 8601 date, or date and time with an optional offset.
fn is_iso_instant(value: &str) -> bool {
    let digits = |text: &str, count: usize| {
        text.len() == count && text.bytes().all(|byte| byte.is_ascii_digit())
    };
    let (date, time) = value
        .split_once('T')
        .map_or((value, None), |(date, time)| (date, Some(time)));
    let mut parts = date.split('-');
    let dated = matches!(
        (parts.next(), parts.next(), parts.next(), parts.next()),
        (Some(year), Some(month), Some(day), None)
            if digits(year, 4) && digits(month, 2) && digits(day, 2)
    );
    let Some(time) = time else {
        return dated;
    };
    // The offset, if any: `Z`, or a sign and hours and minutes.
    let clock = time
        .strip_suffix('Z')
        .or_else(|| {
            let at = time.rfind(['+', '-'])?;
            let (clock, offset) = time.split_at(at);
            let (hours, minutes) = offset.get(1..)?.split_once(':')?;
            (digits(hours, 2) && digits(minutes, 2)).then_some(clock)
        })
        .unwrap_or(time);
    let (whole, fraction) = clock
        .split_once('.')
        .map_or((clock, None), |(whole, fraction)| (whole, Some(fraction)));
    let mut fields = whole.split(':');
    let timed = match (fields.next(), fields.next(), fields.next(), fields.next()) {
        (Some(hours), Some(minutes), seconds, None) => {
            digits(hours, 2)
                && digits(minutes, 2)
                && seconds.is_none_or(|seconds| digits(seconds, 2))
                && fraction.is_none_or(|fraction| {
                    seconds.is_some()
                        && !fraction.is_empty()
                        && fraction.bytes().all(|byte| byte.is_ascii_digit())
                })
        }
        _ => false,
    };
    dated && timed
}

/// The words of a named section or dialog, said above what it holds: its heading and what it
/// says after it, as the widget that publishes it paints them.
fn caption_into(element: &mut Element, access: &AccessNode) {
    if !matches!(access.role, Role::Group | Role::Dialog) {
        return;
    }
    let Some(words) = node_words(access) else {
        return;
    };
    let time = time_of(access);
    let _ = write!(
        element.own,
        "<header class=\"tilo-caption\"><span class=\"tilo-title\">{}</span>",
        timed(&words.title, time.filter(|_| words.detail.is_none()))
    );
    if let Some(detail) = &words.detail {
        let _ = write!(
            element.own,
            "<span class=\"tilo-detail\">{}</span>",
            timed(detail, time)
        );
    }
    element.own.push_str("</header>");
}

/// Whether a node publishes something under it that acts where it stands.
fn acts_within(node: &AccessNode) -> bool {
    node.children
        .iter()
        .any(|child| child.action_target.is_some() && child.bounds.is_some() || acts_within(child))
}

/// Whether a press on a node's element is its action: a button, a row, a link, a tab, a box.
fn pressable(access: &AccessNode) -> bool {
    !access.state.disabled
        && (access.action_target.is_some() && !matches!(access.role, Role::Window)
            || crate::a11y::activation_role(access.role)
            || matches!(access.role, Role::Slider | Role::ComboBox))
}

/// The web address a link node goes to: its value, when it is one.
fn address(access: &AccessNode) -> Option<&str> {
    access.value.as_deref().filter(|value| {
        ["https://", "http://", "mailto:"]
            .iter()
            .any(|scheme| value.starts_with(scheme))
    })
}

/// Whether a press on `element` is the browser following its link, not the node's action.
fn followed(element: &Element, access: &AccessNode) -> bool {
    element.tag == "a" && address(access).is_some()
}

/// The action a press on a node is.
fn press_of(access: &AccessNode) -> &'static str {
    match access.role {
        Role::Checkbox | Role::Switch | Role::RadioButton => "toggle",
        Role::Slider => "slider",
        _ => "activate",
    }
}

/// `element` carries the semantic action `action` on `target`.
fn act(element: &mut Element, action: &str, target: Option<&str>) {
    element.attr("data-a", action);
    if let Some(target) = target {
        element.attr("data-t", target);
    }
}

/// The element a node is.
fn tag_of(access: &AccessNode, shows: Shows, leaf: bool) -> &'static str {
    match (access.role, shows) {
        (Role::Heading { level }, Shows::Words) => match level {
            0 | 1 => "h1",
            2 => "h2",
            3 => "h3",
            4 => "h4",
            5 => "h5",
            _ => "h6",
        },
        (Role::Paragraph, Shows::Words) => "p",
        (Role::StaticText | Role::Label | Role::Tooltip, _) => "span",
        (Role::Checkbox | Role::Switch | Role::RadioButton, Shows::Words) => "label",
        (
            Role::Button
            | Role::ToggleButton
            | Role::Tab
            | Role::MenuItem
            | Role::ListBoxOption
            | Role::ComboBox
            | Role::Checkbox
            | Role::Switch
            | Role::RadioButton,
            Shows::Words | Shows::Paint,
        ) if leaf => "button",
        (Role::Link, Shows::Words) => "a",
        (Role::Document, _) => "article",
        (Role::Dialog, _) => "dialog",
        (Role::Separator, Shows::Words) => "hr",
        (Role::Group, Shows::Structure) if access.name.is_some() => "section",
        _ => "div",
    }
}

fn native(tag: &str, role: &str) -> bool {
    matches!(
        (tag, role),
        ("button", "button")
            | ("a", "link")
            | ("dialog", "dialog")
            | ("hr", "separator")
            | ("article", "document")
    )
}

fn aria_role(role: Role) -> Option<&'static str> {
    Some(match role {
        Role::Toolbar => "toolbar",
        Role::TabBar => "tablist",
        Role::Tab => "tab",
        Role::TabPanel => "tabpanel",
        Role::List => "list",
        Role::ListItem => "listitem",
        Role::Table => "table",
        Role::Row => "row",
        Role::Cell => "cell",
        Role::Tree => "tree",
        Role::TreeItem => "treeitem",
        Role::Menu => "menu",
        Role::MenuItem => "menuitem",
        Role::ListBox => "listbox",
        Role::ListBoxOption => "option",
        Role::Checkbox => "checkbox",
        Role::RadioButton => "radio",
        Role::Switch => "switch",
        Role::Slider => "slider",
        Role::Separator => "separator",
        Role::ComboBox => "combobox",
        Role::ProgressBar | Role::Spinner => "progressbar",
        Role::Tooltip => "tooltip",
        Role::Image | Role::Canvas => "img",
        Role::Dialog => "dialog",
        Role::Button | Role::ToggleButton => "button",
        Role::Link => "link",
        Role::Heading { .. } => "heading",
        Role::Document => "document",
        Role::Group => "group",
        _ => return None,
    })
}

/// A leaf's own size from its layout node's style: what it asks of its parent,
/// never how the parent lays it out (a leaf has nothing to lay out).
fn sizing(layout: &str) -> String {
    layout
        .split(';')
        .filter(|declaration| {
            let name = declaration.split(':').next().unwrap_or_default();
            matches!(
                name,
                "width"
                    | "height"
                    | "min-width"
                    | "min-height"
                    | "max-width"
                    | "max-height"
                    | "flex-grow"
                    | "flex-shrink"
                    | "flex-basis"
                    | "flex"
                    | "margin"
                    | "margin-top"
                    | "margin-right"
                    | "margin-bottom"
                    | "margin-left"
                    | "align-self"
                    | "justify-self"
                    | "grid-row"
                    | "grid-column"
                    | "padding"
            )
        })
        .map(|declaration| format!("{declaration};"))
        .collect()
}

fn rgba(color: vello::peniko::Color) -> String {
    let [r, g, b, a] = {
        let rgba = color.to_rgba8();
        [rgba.r, rgba.g, rgba.b, rgba.a]
    };
    format!("rgba({r},{g},{b},{:.3})", f32::from(a) / 255.0)
}

/// `text` safe in an element's content and in a quoted attribute.
#[must_use]
pub fn escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Where a surface over the window starts arriving, as the layer's style: about the point of it
/// nearest the control that summoned it (its own centre when nothing did), smaller and a few
/// pixels toward that control, as [`crate::motion::Appear`] starts it natively. The page's
/// stylesheet runs it to rest (`@starting-style`) on the theme's appear timing.
fn appear_style(layer: &Composed) -> String {
    use crate::motion::{Appear, Direction};
    use vello::kurbo::{Point, Rect};
    fn summoned(node: &Composed) -> Option<(&Composed, [f64; 2])> {
        node.appears_from
            .map(|point| (node, point))
            .or_else(|| node.children.iter().find_map(summoned))
    }
    let (surface, anchor) = summoned(layer).unwrap_or_else(|| {
        let [x, y, width, height] = layer.rect;
        (layer, [x + width / 2.0, y + height / 2.0])
    });
    let [x, y, width, height] = surface.rect;
    let surface = Rect::new(x, y, x + width, y + height);
    let anchor = Point::new(anchor[0], anchor[1]);
    let origin = Appear::origin(surface, anchor);
    // The tokens' own motion, never reduced: the stylesheet decides whether it moves.
    let moving = crate::tokens::EffectiveMotion::new(crate::tokens::Motion::default(), false);
    let start = Appear::coming(0.0, Direction::Enter, surface, anchor, moving);
    format!(
        "transform-origin:{:.1}px {:.1}px;--tilo-appear-from:translate({:.1}px,{:.1}px) scale({:.3});",
        origin.x, origin.y, start.offset.x, start.offset.y, start.scale
    )
}

/// The theme as the page's custom properties (`--tilo-*`): its colours, its type roles, its
/// corners, its motion, and the `color-scheme` the browser draws its own parts in.
#[must_use]
pub fn theme_css(theme: &crate::Theme) -> String {
    use crate::tokens::typography::TextRole;
    let colors = &theme.tokens.colors;
    let look = if theme.document.is_dark() {
        "dark"
    } else {
        "light"
    };
    // The pointer's tints, as the app answers it on each ground it stands on.
    let tint = |ground, hover, pressed| {
        rgba(
            crate::chrome::selection_tier_on(
                theme,
                crate::chrome::SelectionState {
                    hover,
                    pressed,
                    ..crate::chrome::SelectionState::default()
                },
                ground,
            )
            .fill,
        )
    };
    let content = colors.surfaces.content;
    let mut type_roles = String::new();
    for (name, role) in [
        ("title", TextRole::Title),
        ("heading", TextRole::Heading),
        ("subheading", TextRole::Subheading),
        ("body", TextRole::Body),
        ("label", TextRole::Label),
        ("caption", TextRole::Caption),
        ("code", TextRole::Code),
    ] {
        let style = theme.tokens.typography.style(role);
        let weight = match style.weight {
            crate::text_render::TextWeight::Regular => 400,
            crate::text_render::TextWeight::Medium => 500,
            crate::text_render::TextWeight::Semibold => 600,
            crate::text_render::TextWeight::Bold => 700,
        };
        let _ = write!(
            type_roles,
            "--tilo-{name}-size:{}px;--tilo-{name}-leading:{};--tilo-{name}-weight:{weight};--tilo-{name}-tracking:{:.3}em;",
            style.size,
            style.line_height,
            style.letter_spacing / style.size.max(1.0)
        );
    }
    // The page moves by the named motions the native controls move by. A control's tint and a
    // checkbox's fill fade over `fast` (`--tilo-fade`), a switch's knob travels over it
    // (`--tilo-move`), and both follow `ControlChange`'s spring (`--tilo-settle`, a CSS `linear()`
    // curve). A popover, menu or dialog arrives on `Appear`: its opacity and then its scale and
    // reach each on the curve `Appear::arriving` draws them on natively (one `linear()` per property,
    // in `--tilo-appear-props`' order); under reduced motion it only fades. What it holds (a menu's
    // rows) fades in and rises into place on `Appear::holding` (`--tilo-held-curve`,
    // `--tilo-held-rise`) once its panel is opaque, so its words never read over the page behind it.
    // Reduced motion keeps the fades and lands the travel; a theme that holds still does neither.
    let motion = theme.motion();
    let fast = motion.tokens().durations.fast();
    let (_, settle) = crate::motion::ControlChange::SHOW.css_entrance(motion, |p| p);
    let appear = crate::motion::Appear::SPEC;
    let (appear_time, appear_curve, appear_props, held_curve, held_rise) = if motion.still()
        || motion.reduced()
    {
        let (time, curve) = appear.css_entrance(motion, |p| p);
        (time, curve.clone(), "opacity", curve, 0.0)
    } else {
        let (time, opacity) = appear.css_entrance(motion, |p| crate::motion::Appear::arriving(p).0);
        let (_, transform) = appear.css_entrance(motion, |p| crate::motion::Appear::arriving(p).1);
        let (_, held) = appear.css_entrance(motion, crate::motion::Appear::holding);
        (
            time,
            format!("{opacity},{transform}"),
            "opacity,transform",
            held,
            crate::motion::Appear::HELD_RISE,
        )
    };
    let motion_props = format!(
        "--tilo-fade:{}ms;--tilo-move:{}ms;--tilo-settle:{settle};--tilo-appear-time:{}ms;--tilo-appear-curve:{appear_curve};--tilo-appear-props:{appear_props};--tilo-held-curve:{held_curve};--tilo-held-rise:{held_rise}px;",
        motion.fade(fast).as_millis(),
        motion.fast().as_millis(),
        appear_time.as_millis(),
    );
    // A menu's rows stand in the page as `ContextMenu` lays them out and paints them: its row
    // height, the panel's inset round the rows (the armed row's too), what is left of the row's
    // content inset inside that, the glyph's size and the air after it.
    let tokens = &theme.tokens;
    let inset = crate::widget::floating_surface::menu_inset(theme);
    let line = f64::from(theme.text(TextRole::Body).line_height_px());
    let menu_props = format!(
        "--tilo-menu-row:{}px;--tilo-menu-inset:{inset}px;--tilo-menu-pad:{}px;--tilo-menu-icon:{}px;--tilo-menu-gap:{}px;",
        (line + f64::from(tokens.spacing.s2)).max(f64::from(tokens.metrics.menu_row_height)),
        crate::widget::context_menu::label_inset(theme) - inset,
        tokens.metrics.icon_sm,
        tokens.spacing.s2,
    );
    format!(
        ":root{{color-scheme:{look};{motion_props}{menu_props}--tilo-hover:{};--tilo-press:{};--tilo-selected:{};--tilo-ground:{};--tilo-sidebar:{};--tilo-raised:{};--tilo-overlay:{};--tilo-text:{};--tilo-text-dim:{};--tilo-text-tertiary:{};--tilo-border:{};--tilo-focus:{};--tilo-accent:{};--tilo-on-accent:{};--tilo-radius:{:.1}px;--tilo-control-radius:{}px;{type_roles}}}",
        tint(content, true, false),
        tint(content, false, true),
        rgba(colors.surfaces.raised),
        rgba(colors.surfaces.content),
        rgba(colors.surfaces.sidebar),
        rgba(colors.surfaces.raised),
        rgba(colors.surfaces.overlay),
        rgba(theme.text_primary),
        rgba(theme.text_dim),
        rgba(theme.text_tertiary),
        rgba(theme.border_standard),
        rgba(theme.border_focus),
        rgba(colors.intent.accent),
        rgba(colors.intent.on_accent),
        theme.radius_standard,
        theme.tokens.radii.control,
    )
}

/// A whole page: `title`, the theme's properties and the page's stylesheet in
/// its head, `body` (what [`Page::render`] answered) and `tail` (its script) in
/// its body.
#[must_use]
pub fn document(title: &str, theme: &crate::Theme, body: &str, tail: &str) -> String {
    format!(
        "<!doctype html><html lang=\"en\"><head><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><title>{}</title><style>{}{PAGE_CSS}</style></head><body>{body}{tail}</body></html>",
        escape(title),
        theme_css(theme),
    )
}

/// The page's own stylesheet: the shared components' styles every rendered view takes its looks
/// from, by role, in the theme's properties ([`theme_css`]).
pub const PAGE_CSS: &str = include_str!("page.css");

#[cfg(test)]
mod tests;
