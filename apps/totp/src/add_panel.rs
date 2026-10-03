//! The "Add account" sheet — the app's second screen.
//!
//! The flow follows how people actually add a 2FA account, easiest path first:
//!
//!  1. **Scan a QR image** — decode the QR out of a screenshot or photo, offline,
//!     via [`crate::qr`]. Choosing the file goes through the host file chooser
//!     (the XDG portal), never an app-drawn picker.
//!  2. **Paste a setup link** — an `otpauth://` link fills every field.
//!
//! Only when neither fits does the person type the details: issuer, account
//! name and secret. The cryptographic parameters (algorithm, digits, period)
//! sit behind **Advanced options**.
//!
//! The sheet is a [`ModalDialog`] around a form built from the toolkit's
//! sections, fields, list rows and segmented controls. The form's values live
//! in process-local state, so it can be rebuilt (a section opens, an error
//! shows) without losing what was typed, and its controls report what they
//! asked for through a queue the app drains with [`AddAccountPanel::pump`].
//! The outcome surfaces as an [`AddPanelEvent`] the app takes.

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, VecDeque};
use std::rc::Rc;

use taffy::{NodeId, Style, TaffyTree};
use uuid::Uuid;
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};
use winit::event::WindowEvent;
use zeroize::Zeroize;

use ui::LocalState;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::toggle_group::Segment;
use ui::widget::{
    AppModel, Button, Column, ControlSize, Element, EventCtx, Field, LabelPlacement, ListRow,
    ModalDialog, ModelPath, Scrollable, Section, SectionHeader, TaffyTreeExt, Text, TextInput,
    TextInputKind, ToggleGroup, Widget,
};

use crate::model::{self, Account, AccountId, Algorithm};

/// Outcome the app acts on.
#[derive(Clone, Debug)]
pub enum AddPanelEvent {
    Cancel,
    Save(Account),
    /// Replace an existing account's local secret and shared metadata. The
    /// stable id is retained, so search links, collaboration references and
    /// history all continue to identify the same account.
    Edit(Account),
    /// Attach a newly-entered local secret to an already-shared account id.
    /// This does not create a CRDT/action-history mutation.
    Provision(Account),
    /// The user asked to import a QR code from an image file. The app routes
    /// this to the **host** file chooser (the XDG Desktop Portal — the app
    /// never draws its own picker), then runs the decode and calls
    /// [`AddAccountPanel::apply_uri`] with the result.
    PickImageForQr,
}

/// What the sheet is for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Add,
    Edit,
    /// Install the local key of an account that is already shared.
    Provision,
}

/// Which text field has, or should take, the caret.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldId {
    Uri,
    Issuer,
    Label,
    Secret,
    Period,
}

impl FieldId {
    /// The field's stable identity in the window.
    fn test_id(self) -> &'static str {
        match self {
            Self::Uri => "totp-link",
            Self::Issuer => "totp-issuer",
            Self::Label => "totp-account-name",
            Self::Secret => "totp-secret-key",
            Self::Period => "totp-period",
        }
    }
}

/// What one of the sheet's controls asked for.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Msg {
    PickImage,
    ExpandLink,
    UriChanged,
    ToggleAdvanced,
    Save,
    Cancel,
}

/// A reason the form cannot be saved, shown under the field it concerns.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Problem {
    field: FieldId,
    message: String,
}

/// The sheet's state, shared with the callbacks of the controls it builds.
struct Sheet {
    flow: Flow,
    uri_expanded: bool,
    advanced_open: bool,
    problem: Option<Problem>,
    /// The field that takes the caret the next time the form is built.
    focus: Option<FieldId>,
    queue: VecDeque<Msg>,
}

impl Sheet {
    fn new() -> Self {
        Self {
            flow: Flow::Add,
            uri_expanded: false,
            advanced_open: false,
            problem: None,
            focus: None,
            queue: VecDeque::new(),
        }
    }
}

type Shared = Rc<RefCell<Sheet>>;

/// The values the form edits. Process-local state: the form's inputs bind to
/// it, so rebuilding the form keeps what was typed.
#[derive(Clone)]
struct Values {
    uri: LocalState<String>,
    issuer: LocalState<String>,
    label: LocalState<String>,
    secret: LocalState<String>,
    period: LocalState<String>,
    algorithm: LocalState<usize>,
    digits: LocalState<usize>,
}

/// The digit counts the form offers, in selector order.
const DIGITS: [u8; 2] = [6, 8];
/// The period a new account starts with, in seconds.
const DEFAULT_PERIOD: &str = "30";

impl Values {
    fn new() -> Self {
        Self {
            uri: LocalState::new(String::new()),
            issuer: LocalState::new(String::new()),
            label: LocalState::new(String::new()),
            secret: LocalState::new(String::new()),
            period: LocalState::new(DEFAULT_PERIOD.to_owned()),
            algorithm: LocalState::new(0),
            digits: LocalState::new(0),
        }
    }

    fn reset(&self) {
        self.uri.set(String::new());
        self.issuer.set(String::new());
        self.label.set(String::new());
        self.wipe_secret();
        self.period.set(DEFAULT_PERIOD.to_owned());
        self.algorithm.set(Algorithm::Sha1.index());
        self.digits.set(0);
    }

    /// Clear the key from memory, not merely from the field.
    fn wipe_secret(&self) {
        self.secret.update(Zeroize::zeroize);
        self.uri.update(Zeroize::zeroize);
    }

    fn text(state: &LocalState<String>) -> String {
        state.with(Clone::clone)
    }
}

fn t(key: &str) -> String {
    crate::i18n::t(key)
}

/// The sheet's form: a section tree rebuilt from [`Sheet`] and [`Values`]
/// whenever its shape changes, laid out and drawn by the toolkit.
///
/// A [`ModalDialog`] draws and lays its content out apart from the window's
/// own tree, so the node a field reports when a click asks for the caret is not
/// one the window knows. The form keeps the pointer's position and answers
/// which field is under it ([`Self::field_under`]); the app turns that into the
/// window's own node for the field ([`Self::host_field_nodes`]).
#[allow(
    clippy::struct_excessive_bools,
    reason = "independent rebuild, relayout and scroll flags, not one state machine"
)]
struct AddForm {
    sheet: Shared,
    values: Values,
    tree: Scrollable,
    cursor: Point,
    rect: Rect,
    /// The theme the form was last laid out under: what it is built from.
    theme: Option<Theme>,
    /// What of that theme the form's building reads.
    theme_key: Option<(u32, u32, u32)>,
    /// The form is taller than the window allows, so it shows a scroll bar.
    overflows: bool,
    /// Scroll to the end the next time the form is built: a section that just
    /// opened is there.
    to_end: bool,
    /// The scroll position, kept across rebuilds.
    offset: Rc<Cell<(f32, f32)>>,
    /// The window's height, once the sheet has been drawn in it; it bounds how
    /// tall the form may grow before it scrolls.
    viewport_h: f32,
    /// The tree must be built again the next time the window lays it out.
    stale: bool,
    /// The window must lay the form out again.
    structure_dirty: bool,
    /// The sheet must measure the form again.
    relayout: bool,
}

impl AddForm {
    fn new(sheet: Shared, values: Values) -> Self {
        Self {
            tree: Scrollable::new_vertical(),
            sheet,
            values,
            cursor: Point::ZERO,
            rect: Rect::ZERO,
            theme: None,
            theme_key: None,
            overflows: false,
            to_end: false,
            offset: Rc::new(Cell::new((0.0, 0.0))),
            viewport_h: 0.0,
            stale: true,
            structure_dirty: true,
            relayout: false,
        }
    }

    /// Build the tree again from the current state the next time the window
    /// lays the form out.
    fn rebuild(&mut self) {
        self.stale = true;
        self.structure_dirty = true;
    }

    /// Whether the sheet must measure the form again, once.
    fn take_relayout(&mut self) -> bool {
        std::mem::take(&mut self.relayout)
    }

    /// Scroll to the end of the form, where a section that just opened is.
    fn scroll_to_end(&mut self) {
        self.to_end = true;
    }

    /// Visit `widget` and every widget under it, depth first.
    fn visit(widget: &mut dyn Widget, f: &mut dyn FnMut(&mut dyn Widget)) {
        f(widget);
        for child in widget.children_mut() {
            Self::visit(child, f);
        }
    }

    /// How many widgets `kind` selects, which one has focus, and the last one
    /// whose rect contains `p`, all in tree order.
    fn survey(
        &mut self,
        kind: fn(&dyn Widget) -> bool,
        p: Point,
    ) -> (usize, Option<usize>, Option<usize>) {
        let (mut count, mut focused, mut under) = (0, None, None);
        Self::visit(&mut self.tree, &mut |widget| {
            if kind(widget) {
                if widget.is_focused() {
                    focused = Some(count);
                }
                if widget.get_rect().contains(p) {
                    under = Some(count);
                }
                count += 1;
            }
        });
        (count, focused, under)
    }

    /// Which text field (counting in tree order) is under `p`, if any.
    fn field_under(&mut self, p: Point) -> Option<usize> {
        self.survey(is_text_field, p).2
    }

    /// The window's own node of each text field, in tree order, read from the
    /// node map the window just built the sheet into.
    fn host_field_nodes(&mut self, node_map: &HashMap<NodeId, *mut dyn Widget>) -> Vec<NodeId> {
        let mut fields: Vec<*const ()> = Vec::new();
        Self::visit(&mut self.tree, &mut |widget| {
            if is_text_field(widget) {
                fields.push(std::ptr::from_mut(widget).cast::<()>());
            }
        });
        fields
            .iter()
            .filter_map(|field| {
                node_map
                    .iter()
                    .find(|(_, widget)| widget.cast::<()>().cast_const() == *field)
                    .map(|(node, _)| *node)
            })
            .collect()
    }

    /// The layout node of the scroller, the one child of the form's own.
    fn child_node(taffy: &TaffyTree<()>, node: NodeId) -> NodeId {
        taffy
            .children(node)
            .ok()
            .and_then(|children| children.first().copied())
            .unwrap_or(node)
    }

    fn forward(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        // A focus request from a field names a node of the sheet's own layout,
        // which the window does not know: the app answers it instead.
        let mut requested = None;
        let node = Self::child_node(ctx.taffy, ctx.node_id);
        let path = ctx.path;
        let mut inner = EventCtx {
            focus_target: &mut requested,
            ..ctx.reborrow(path, node)
        };
        self.tree.handle_event(event, &mut inner);
    }
}

impl Widget for AddForm {
    fn sync(&mut self, state: &mut AppModel, path: &ModelPath) {
        // The tree is replaced here, where the window expects a widget to
        // change its structure (and then asks it whether it did), never while
        // the window or the sheet is laying it out.
        if self.stale
            && let Some(theme) = self.theme.clone()
        {
            self.stale = false;
            // The window's height is known once the sheet has been drawn; only
            // then is there an end to scroll to.
            if self.to_end && self.viewport_h > 0.0 {
                self.offset.set((0.0, f32::MAX));
                self.to_end = false;
            }
            self.tree = build(
                &self.sheet,
                &self.values,
                &theme,
                &Frame {
                    cap: body_cap(&theme, self.viewport_h),
                    bar: self.overflows,
                    offset: &self.offset,
                    cursor: self.cursor,
                },
            );
            self.sheet.borrow_mut().focus = None;
            self.structure_dirty = true;
        }
        self.tree.sync(state, path);
    }

    fn update(&mut self, state: &mut AppModel, path: &ModelPath) -> bool {
        self.tree.update(state, path)
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The tree is built once the theme is known; until then (and whenever
        // the theme changes) it waits for the next sync to be built.
        let key = (
            theme.tokens.spacing.rhythm.group.to_bits(),
            theme.text_scale.to_bits(),
            theme.ui_scale.to_bits(),
        );
        if self.theme_key != Some(key) {
            self.theme_key = Some(key);
            self.theme = Some(theme.clone());
            self.stale = true;
        }
        // The form has a node of its own over the scroller's, so that the
        // window's focus order pairs each widget with its node.
        let child = self.tree.build_layout(taffy, theme, node_map);
        let node = taffy.parent_node(
            Style {
                display: taffy::Display::Flex,
                flex_direction: taffy::FlexDirection::Column,
                size: taffy::Size {
                    width: taffy::style_helpers::percent(1.0_f32),
                    height: taffy::Dimension::auto(),
                },
                ..Style::default()
            },
            &[child],
        );
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let at = Point::new(
            origin.x + f64::from(layout.location.x),
            origin.y + f64::from(layout.location.y),
        );
        self.rect = Rect::from_origin_size(
            at,
            (f64::from(layout.size.width), f64::from(layout.size.height)),
        );
        let child = Self::child_node(taffy, node);
        self.tree.apply_layout(taffy, child, at);
        // A scroll bar takes room from the fields, so it shows only when the
        // form is taller than the window allows.
        let overflows = self.tree.vertical_overflows();
        if overflows != self.overflows {
            self.overflows = overflows;
            self.rebuild();
            self.relayout = true;
        }
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        presence: &PresenceCtx,
    ) {
        // How tall the form may grow depends on the window it stands in.
        if let Some(window) = presence.viewport.get() {
            let height = window.height() as f32;
            if (height - self.viewport_h).abs() > 0.5 {
                self.viewport_h = height;
                self.rebuild();
                self.relayout = true;
            }
        }
        self.tree.draw(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        self.forward(event, ctx);
    }

    fn take_structure_dirty(&mut self) -> bool {
        let own = std::mem::take(&mut self.structure_dirty);
        self.tree.take_structure_dirty() | own
    }

    fn children(&self) -> Vec<&dyn Widget> {
        vec![&self.tree]
    }

    fn children_mut(&mut self) -> Vec<&mut (dyn Widget + '_)> {
        vec![&mut self.tree]
    }

    fn name(&self) -> &'static str {
        "AddForm"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

/// Whether `widget` takes typed text (and so the caret).
fn is_text_field(widget: &dyn Widget) -> bool {
    widget.ime_purpose().is_some()
}

/// One labelled text field bound to `state`.
fn text_field(
    sheet: &Shared,
    id: FieldId,
    label: &str,
    state: &LocalState<String>,
    kind: TextInputKind,
    placeholder: &str,
    on_change: Option<Msg>,
) -> Element {
    // A stable id lets the window keep the caret in the field when the form is
    // built again (a section opened, a problem shown).
    let mut input = TextInput::from_state(state)
        .with_id(id.test_id())
        .with_kind(kind)
        .with_placeholder(placeholder);
    if sheet.borrow().focus == Some(id) {
        input.set_focus(true);
    }
    if let Some(msg) = on_change {
        let queue = Rc::clone(sheet);
        input = input.on_change(move |_| queue.borrow_mut().queue.push_back(msg));
    }
    let mut field = Field::new(label, input).with_label_placement(Some(LabelPlacement::Above));
    if let Some(problem) = sheet.borrow().problem.as_ref().filter(|p| p.field == id) {
        field = field.with_error(problem.message.clone());
    }
    Box::new(field)
}

/// A labelled segmented choice: the segments' labels, which one is chosen and
/// what choosing one does.
fn choice(
    label: &str,
    labels: &[String],
    selected: usize,
    set: impl FnMut(usize) + 'static,
) -> Element {
    let segments = labels.iter().map(Segment::new).collect();
    Box::new(
        Column::new()
            .with_no_padding()
            .with_intrinsic_height()
            .push(Box::new(Text::new(label).with_role(TextRole::Label)))
            .push(Box::new(
                ToggleGroup::new(segments, selected).on_change(set),
            )),
    )
}

/// What pressing a control of the sheet asks for: a callback that queues it.
fn queued(sheet: &Shared, msg: Msg) -> impl FnMut() + 'static {
    let sheet = Rc::clone(sheet);
    move || sheet.borrow_mut().queue.push_back(msg)
}

/// The ways to import an account: scan an image, paste a link.
fn import_section(sheet: &Shared, values: &Values, uri_expanded: bool) -> Vec<Element> {
    let mut items: Vec<Element> = vec![Box::new(Section::new(vec![
        Box::new(
            ListRow::new("totp-scan", t("totp-add-scan-title"))
                .icon(Icon::Image)
                .secondary(t("totp-add-scan-description"))
                .disclosure()
                .on_activate(queued(sheet, Msg::PickImage)),
        ),
        Box::new(
            ListRow::new("totp-link", t("totp-add-link-title"))
                .icon(Icon::ExternalLink)
                .secondary(t("totp-add-link-description"))
                .disclosure()
                .on_activate(queued(sheet, Msg::ExpandLink)),
        ),
    ]))];
    if uri_expanded {
        items.push(text_field(
            sheet,
            FieldId::Uri,
            &t("totp-add-link-label"),
            &values.uri,
            TextInputKind::Url,
            "otpauth://totp/Issuer:account?secret=\u{2026}",
            Some(Msg::UriChanged),
        ));
    }
    items
}

/// The three fields almost every account needs; headed when it follows the
/// import choices.
fn details_section(sheet: &Shared, values: &Values, headed: bool) -> Section {
    let section = Section::new(vec![
        text_field(
            sheet,
            FieldId::Issuer,
            &t("totp-add-label-issuer"),
            &values.issuer,
            TextInputKind::Name,
            "GitHub",
            None,
        ),
        text_field(
            sheet,
            FieldId::Label,
            &t("totp-add-label-account"),
            &values.label,
            TextInputKind::Username,
            "ada@example.com",
            None,
        ),
        text_field(
            sheet,
            FieldId::Secret,
            &t("totp-add-label-secret"),
            &values.secret,
            TextInputKind::Password,
            "JBSWY3DPEHPK3PXP",
            None,
        ),
    ]);
    if headed {
        section.with_header(SectionHeader::group(t("totp-add-details")))
    } else {
        section
    }
}

/// The cryptographic parameters, behind a Show / Hide action in the header.
fn advanced_section(sheet: &Shared, values: &Values, open: bool) -> Section {
    let toggle = Button::new(
        "totp-advanced",
        &t(if open {
            "totp-add-hide"
        } else {
            "totp-add-show"
        }),
        None,
    )
    .on_click(queued(sheet, Msg::ToggleAdvanced));
    let mut items: Vec<Element> = Vec::new();
    if open {
        let algorithms: Vec<String> = Algorithm::ALL
            .iter()
            .map(|algorithm| algorithm.as_str().to_owned())
            .collect();
        let algorithm = values.algorithm.clone();
        items.push(choice(
            &t("totp-add-label-algorithm"),
            &algorithms,
            values.algorithm.with(|selected| *selected),
            move |index| algorithm.set(index),
        ));
        let counts: Vec<String> = DIGITS.iter().map(u8::to_string).collect();
        let digits = values.digits.clone();
        items.push(choice(
            &t("totp-add-label-digits"),
            &counts,
            values.digits.with(|selected| *selected),
            move |index| digits.set(index),
        ));
        items.push(text_field(
            sheet,
            FieldId::Period,
            &t("totp-add-label-period"),
            &values.period,
            TextInputKind::Number,
            DEFAULT_PERIOD,
            None,
        ));
    }
    Section::new(items).with_header(SectionHeader::group(t("totp-add-advanced")).action(toggle))
}

/// How tall the form may be in a window `viewport_h` tall before it scrolls:
/// what is left of the window once the sheet's padding, its title and its
/// footer are taken out. Unbounded until the window is known.
fn body_cap(theme: &Theme, viewport_h: f32) -> f32 {
    if viewport_h <= 0.0 {
        return f32::from(u16::MAX);
    }
    let spacing = &theme.tokens.spacing;
    let title = theme
        .text(TextRole::Title)
        .line_height_px()
        .max(theme.tokens.metrics.min_pointer_target);
    let footer = spacing.s6 + ControlSize::Regular.height(theme) as f32;
    let chrome = 4.0 * spacing.s6 + title + spacing.s4 + footer;
    (viewport_h - chrome).max(footer)
}

/// How the form is framed in its window.
struct Frame<'a> {
    /// The tallest the form may be before it scrolls.
    cap: f32,
    /// Whether the scroll bar shows.
    bar: bool,
    offset: &'a Rc<Cell<(f32, f32)>>,
    cursor: Point,
}

/// The form for the sheet's current state: its groups a group-gap apart, in a
/// scroller that keeps the sheet inside the window.
fn build(sheet: &Shared, values: &Values, theme: &Theme, frame: &Frame<'_>) -> Scrollable {
    crate::i18n::ensure();
    let (flow, uri_expanded, advanced_open) = {
        let state = sheet.borrow();
        (state.flow, state.uri_expanded, state.advanced_open)
    };
    let imports = flow != Flow::Edit;
    let mut groups = Column::new()
        .with_no_padding()
        .with_intrinsic_height()
        .with_gap(theme.tokens.spacing.rhythm.group);
    if imports {
        groups = groups.push(Box::new(Section::new(import_section(
            sheet,
            values,
            uri_expanded,
        ))));
    }
    groups = groups
        .push(Box::new(details_section(sheet, values, imports)))
        .push(Box::new(advanced_section(sheet, values, advanced_open)));
    Scrollable::new_vertical()
        .with_max_height(frame.cap)
        .with_scrollbar(frame.bar)
        .with_persisted_offset(Rc::clone(frame.offset))
        .with_cursor(frame.cursor)
        .with_child(Box::new(groups))
}

/// Which of the sheet's fields a validation message is about.
fn problem_field(message: &str) -> FieldId {
    if message.contains("secret key") || message.contains("base32") {
        FieldId::Secret
    } else {
        FieldId::Issuer
    }
}

/// The add / edit / set-up sheet and the account it produces.
pub struct AddAccountPanel {
    sheet: Shared,
    values: Values,
    /// Boxed so the sheet's widgets stay where the window's layout map found
    /// them whatever the panel does with it: a map entry is a raw address.
    dialog: Option<Box<ModalDialog<AddForm>>>,
    /// Sheets that closed or were replaced: the window's layout map still
    /// points at their widgets until it is built again, so they are dropped
    /// the next time the app syncs ([`Self::reap`]), not while an event is
    /// being handled.
    retired: Vec<Box<ModalDialog<AddForm>>>,
    /// The stable account identity being edited, if this panel is an edit
    /// rather than a new-account flow. The secret remains only in this local
    /// form and the encrypted vault; it is never copied to the CRDT.
    editing: Option<AccountId>,
    /// Immutable identity guard for local setup of a shared account.
    provisioning: Option<Account>,
    event: Option<AddPanelEvent>,
}

impl Default for AddAccountPanel {
    fn default() -> Self {
        Self::new()
    }
}

impl AddAccountPanel {
    pub fn new() -> Self {
        Self {
            sheet: Rc::new(RefCell::new(Sheet::new())),
            values: Values::new(),
            dialog: None,
            retired: Vec::new(),
            editing: None,
            provisioning: None,
            event: None,
        }
    }

    pub fn is_open(&self) -> bool {
        self.dialog.as_ref().is_some_and(|dialog| dialog.is_open())
    }

    /// The open sheet, for the root to mount in its tree.
    pub fn widget(&self) -> Option<&dyn Widget> {
        self.dialog.as_deref().map(|dialog| dialog as &dyn Widget)
    }

    pub fn widget_mut(&mut self) -> Option<&mut dyn Widget> {
        self.dialog
            .as_deref_mut()
            .map(|dialog| dialog as &mut dyn Widget)
    }

    /// Which text field of the open sheet is under `p`, counting in tree order.
    pub fn field_under(&mut self, p: Point) -> Option<usize> {
        self.dialog.as_mut()?.content_mut().field_under(p)
    }

    /// The window's own nodes for the open sheet's text fields, in tree order,
    /// from the node map the window just built the sheet into.
    pub fn host_field_nodes(&mut self, node_map: &HashMap<NodeId, *mut dyn Widget>) -> Vec<NodeId> {
        self.dialog.as_mut().map_or_else(Vec::new, |dialog| {
            dialog.content_mut().host_field_nodes(node_map)
        })
    }

    /// Whether the open sheet changed shape since the window last laid it out.
    pub fn take_structure_dirty(&mut self) -> bool {
        self.dialog
            .as_mut()
            .is_some_and(|dialog| dialog.take_structure_dirty())
    }

    fn flow(&self) -> Flow {
        if self.provisioning.is_some() {
            Flow::Provision
        } else if self.editing.is_some() {
            Flow::Edit
        } else {
            Flow::Add
        }
    }

    /// Show the sheet for the current flow and values.
    fn present(&mut self) {
        let flow = self.flow();
        {
            let mut sheet = self.sheet.borrow_mut();
            sheet.flow = flow;
            sheet.queue.clear();
        }
        crate::i18n::ensure();
        let (title, save) = match flow {
            Flow::Add => ("totp-add-title-add", "totp-add-save-add"),
            Flow::Edit => ("totp-add-title-edit", "totp-add-save-edit"),
            Flow::Provision => ("totp-add-title-provision", "totp-add-save-provision"),
        };
        let sheet = Rc::clone(&self.sheet);
        let queue = |msg: Msg| {
            let sheet = Rc::clone(&sheet);
            move || sheet.borrow_mut().queue.push_back(msg)
        };
        self.retire();
        let form = AddForm::new(Rc::clone(&self.sheet), self.values.clone());
        self.dialog = Some(Box::new(
            ModalDialog::new(t(title), form)
                .open(true)
                .with_test_id("totp-add-dialog")
                .primary_action(t(save), queue(Msg::Save))
                .secondary_action(t("totp-add-cancel"), queue(Msg::Cancel))
                .on_dismiss(queue(Msg::Cancel)),
        ));
    }

    /// Open the sheet, resetting every field to defaults and collapsing the
    /// disclosures so each visit starts from the calm default state.
    pub fn open(&mut self) {
        self.editing = None;
        self.provisioning = None;
        self.values.reset();
        {
            let mut sheet = self.sheet.borrow_mut();
            sheet.uri_expanded = false;
            sheet.advanced_open = false;
            sheet.problem = None;
            sheet.focus = None;
        }
        self.present();
    }

    pub fn close(&mut self) {
        self.values.wipe_secret();
        self.retire();
    }

    /// Take the open sheet out of the tree, keeping it alive until [`Self::reap`].
    fn retire(&mut self) {
        if let Some(mut dialog) = self.dialog.take() {
            dialog.set_open(false);
            self.retired.push(dialog);
        }
    }

    /// Drop the sheets that left the tree. Returns whether any did, so the
    /// window lays its tree out again.
    pub fn reap(&mut self) -> bool {
        let reaped = !self.retired.is_empty();
        self.retired.clear();
        reaped
    }

    /// Open with the Advanced-options section already expanded.
    pub fn open_with_advanced(&mut self) {
        self.open();
        self.sheet.borrow_mut().advanced_open = true;
        self.rebuild();
        if let Some(dialog) = &mut self.dialog {
            dialog.content_mut().scroll_to_end();
        }
    }

    /// Open the same calm form for an existing account. This is intentionally
    /// local: only this device's encrypted vault supplies the secret field.
    pub fn open_for_edit(&mut self, account: &Account) {
        self.open();
        self.editing = Some(account.id);
        self.load(account, true);
        let mut sheet = self.sheet.borrow_mut();
        sheet.flow = Flow::Edit;
        sheet.focus = Some(FieldId::Issuer);
        sheet.advanced_open = account.algorithm != Algorithm::Sha1
            || account.digits != DIGITS[0]
            || account.period != 30;
        drop(sheet);
        self.present();
    }

    /// Provision the secret for this exact existing resource. The form starts
    /// with the shared identity, then accepts only local credential material.
    pub fn open_for_provision(&mut self, account: &Account) {
        self.open();
        self.editing = Some(account.id);
        self.provisioning = Some(account.clone());
        self.load(account, false);
        self.sheet.borrow_mut().focus = Some(FieldId::Secret);
        self.present();
    }

    /// Put `account`'s fields in the form.
    fn load(&self, account: &Account, with_secret: bool) {
        self.values.issuer.set(account.issuer.clone());
        self.values.label.set(account.label.clone());
        if with_secret {
            self.values.secret.set(account.secret.clone());
        }
        self.values.period.set(account.period.to_string());
        self.values.algorithm.set(account.algorithm.index());
        self.values
            .digits
            .set(usize::from(account.digits == DIGITS[1]));
    }

    pub fn take_event(&mut self) -> Option<AddPanelEvent> {
        self.event.take()
    }

    /// Build the form again after its shape or an error changed.
    fn rebuild(&mut self) {
        if let Some(dialog) = &mut self.dialog {
            dialog.content_mut().rebuild();
            // The sheet's size follows its content: ask for a fresh layout.
            dialog.set_max_width(ui::widget::modal_dialog::MODAL_MAX_WIDTH);
        }
    }

    /// Act on what the sheet's controls asked for since the last call. The
    /// app calls this after every event it routes to the sheet and each frame.
    pub fn pump(&mut self) {
        loop {
            let msg = self.sheet.borrow_mut().queue.pop_front();
            let Some(msg) = msg else { break };
            self.apply(msg);
        }
        // The form measured against a new window height: the sheet's size
        // follows.
        if let Some(dialog) = &mut self.dialog
            && dialog.content_mut().take_relayout()
        {
            dialog.set_max_width(ui::widget::modal_dialog::MODAL_MAX_WIDTH);
        }
    }

    /// Scroll the form with the wheel: a modal dialog does not pass wheel
    /// events to its content.
    pub fn handle_wheel(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        if let Some(dialog) = &mut self.dialog {
            dialog.content_mut().handle_event(event, ctx);
        }
    }

    fn apply(&mut self, msg: Msg) {
        match msg {
            Msg::PickImage => self.event = Some(AddPanelEvent::PickImageForQr),
            Msg::ExpandLink => {
                {
                    let mut sheet = self.sheet.borrow_mut();
                    sheet.uri_expanded = true;
                    sheet.problem = None;
                    sheet.focus = Some(FieldId::Uri);
                }
                self.rebuild();
            }
            Msg::UriChanged => self.read_uri_field(),
            Msg::ToggleAdvanced => {
                let open = {
                    let mut sheet = self.sheet.borrow_mut();
                    sheet.advanced_open = !sheet.advanced_open;
                    sheet.focus = None;
                    sheet.advanced_open
                };
                self.rebuild();
                if open && let Some(dialog) = &mut self.dialog {
                    dialog.content_mut().scroll_to_end();
                }
            }
            Msg::Save => self.try_save(),
            Msg::Cancel => {
                self.close();
                self.event = Some(AddPanelEvent::Cancel);
            }
        }
    }

    /// Fill the form from a setup link as soon as the field holds a complete
    /// one; a link that does not parse says why.
    fn read_uri_field(&mut self) {
        let uri = Values::text(&self.values.uri);
        let trimmed = uri.trim();
        if trimmed.is_empty() {
            return;
        }
        match model::parse_otpauth_uri(trimmed, Uuid::new_v4()) {
            Ok(_) => self.apply_uri(trimmed),
            Err(message) if trimmed.starts_with("otpauth://") => {
                self.show_problem(FieldId::Uri, message);
            }
            Err(_) => {}
        }
    }

    fn show_problem(&mut self, field: FieldId, message: String) {
        {
            let mut sheet = self.sheet.borrow_mut();
            sheet.problem = Some(Problem { field, message });
            sheet.focus = None;
        }
        self.rebuild();
    }

    // ---- account building / actions ------------------------------------

    /// Build an account from the current field values, or the problem that
    /// stops it being saved.
    fn build_account(&self) -> Result<Account, Problem> {
        let problem = |field, message: &str| Problem {
            field,
            message: message.to_owned(),
        };
        let secret = model::normalise_secret(&Values::text(&self.values.secret));
        if secret.is_empty() {
            return Err(problem(FieldId::Secret, "Enter a secret key"));
        }
        if crate::totp::decode_secret(&secret).is_none_or(|b| b.is_empty()) {
            return Err(problem(
                FieldId::Secret,
                "That secret isn\u{2019}t valid base32",
            ));
        }
        let issuer = Values::text(&self.values.issuer).trim().to_owned();
        let label = Values::text(&self.values.label).trim().to_owned();
        if model::contains_secret_shaped_text(&issuer) || model::contains_secret_shaped_text(&label)
        {
            let field = if model::contains_secret_shaped_text(&issuer) {
                FieldId::Issuer
            } else {
                FieldId::Label
            };
            return Err(problem(
                field,
                "Account names must not include a secret key",
            ));
        }
        if issuer.is_empty() && label.is_empty() {
            return Err(problem(FieldId::Issuer, "Add an issuer or account name"));
        }
        let period = Values::text(&self.values.period)
            .trim()
            .parse::<u32>()
            .unwrap_or(30)
            .clamp(5, 300);
        let algorithm = Algorithm::ALL
            .get(self.values.algorithm.with(|selected| *selected))
            .copied()
            .unwrap_or_default();
        let digits = DIGITS
            .get(self.values.digits.with(|selected| *selected))
            .copied()
            .unwrap_or(DIGITS[0]);
        let color = (issuer.len().wrapping_add(label.len()) % 7) as u8;
        Ok(Account {
            id: self.editing.unwrap_or_else(Uuid::new_v4),
            issuer,
            label,
            secret,
            algorithm,
            digits,
            period,
            color,
            order: 0,
        })
    }

    /// Apply a decoded `otpauth://` URI (from paste or QR image import) into
    /// the fields. On success the form shows the filled values so the person
    /// can review them before saving; on failure the reason shows inline.
    pub fn apply_uri(&mut self, uri: &str) {
        match model::parse_otpauth_uri(uri, Uuid::new_v4()) {
            Ok(acc) => {
                self.load(&acc, true);
                self.values.uri.update(Zeroize::zeroize);
                self.values.uri.set(String::new());
                {
                    let mut sheet = self.sheet.borrow_mut();
                    sheet.uri_expanded = false;
                    sheet.problem = None;
                    sheet.focus = Some(FieldId::Issuer);
                    // Surface non-default crypto params so a reviewing person
                    // can see what the code carried.
                    if acc.algorithm != Algorithm::Sha1
                        || acc.digits != DIGITS[0]
                        || acc.period != 30
                    {
                        sheet.advanced_open = true;
                    }
                }
                self.rebuild();
            }
            Err(message) => self.show_problem(problem_field(&message), message),
        }
    }

    fn try_save(&mut self) {
        match self.build_account() {
            Ok(mut acc) => {
                if let Some(existing) = &self.provisioning {
                    if acc.issuer.trim() != existing.issuer.trim()
                        || acc.label.trim() != existing.label.trim()
                    {
                        let message = "This setup link names a different account. Edit shared details separately.";
                        self.show_problem(FieldId::Issuer, message.to_owned());
                        return;
                    }
                    if acc.algorithm != existing.algorithm
                        || acc.digits != existing.digits
                        || acc.period != existing.period
                    {
                        let message = "This setup link uses different TOTP settings. Update shared settings first, then set up this device.";
                        self.show_problem(FieldId::Secret, message.to_owned());
                        return;
                    }
                    acc.id = existing.id;
                    acc.color = existing.color;
                    acc.order = existing.order;
                }
                self.event = Some(match self.flow() {
                    Flow::Provision => AddPanelEvent::Provision(acc),
                    Flow::Edit => AddPanelEvent::Edit(acc),
                    Flow::Add => AddPanelEvent::Save(acc),
                });
                self.close();
            }
            Err(problem) => self.show_problem(problem.field, problem.message),
        }
    }

    /// Test hook: run the exact production save path (validate, emit event).
    #[cfg(test)]
    pub(crate) fn save_for_test(&mut self) {
        self.try_save();
    }

    /// Test hook: current (issuer, label, secret) field values.
    #[cfg(test)]
    pub(crate) fn field_snapshot(&self) -> (String, String, String) {
        (
            Values::text(&self.values.issuer),
            Values::text(&self.values.label),
            Values::text(&self.values.secret),
        )
    }

    /// Test hook: how many text fields the form has and which one has the
    /// caret.
    #[cfg(test)]
    pub(crate) fn caret(&mut self) -> Option<(usize, Option<usize>)> {
        let form = self.dialog.as_mut()?.content_mut();
        let (count, focused, _) = form.survey(is_text_field, Point::ZERO);
        Some((count, focused))
    }

    /// Test hook: the validation message under a field, if any.
    #[cfg(test)]
    pub(crate) fn problem(&self) -> Option<String> {
        self.sheet
            .borrow()
            .problem
            .as_ref()
            .map(|problem| problem.message.clone())
    }
}

#[cfg(test)]
#[path = "add_panel_tests.rs"]
mod tests;
