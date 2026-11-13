//! The add-account sheet: scan a QR image, paste an `otpauth://` link, or type
//! the details, with the cryptographic parameters behind Advanced options. The
//! form's values live in process-local state, so rebuilding a section keeps them.

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
    AppModel, Column, ControlSize, Element, EventCtx, Field, LabelPlacement, ListRow, ModalDialog,
    ModelPath, Row, Scrollable, Section, SectionHeader, Spacer, TaffyTreeExt, Text, TextInput,
    TextInputKind, ToggleGroup, Widget,
};

use crate::model::{self, Account, AccountId, Algorithm};
use ui::widget::DrawChild;

#[derive(Clone, Debug)]
pub enum AddPanelEvent {
    Cancel,
    Save(Account),
    /// Replace an account's local secret and shared metadata under the same id.
    Edit(Account),
    /// Attach a local secret to an already-shared account; no document change.
    Provision(Account),
    /// Import a QR code from an image: the app runs the host's file chooser, then
    /// `apply_uri`.
    PickImageForQr,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Flow {
    Add,
    Edit,
    Provision,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum FieldId {
    Uri,
    Issuer,
    Label,
    Secret,
    Period,
}

impl FieldId {
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

/// Process-local state the form's inputs bind to, so rebuilding keeps what was
/// typed.
#[derive(Clone)]
struct Values {
    uri: LocalState<String>,
    issuer: LocalState<String>,
    label: LocalState<String>,
    secret: LocalState<String>,
    period: LocalState<String>,
    algorithm: LocalState<usize>,
    /// The digit count itself, not a segment index: a link may carry a count the
    /// form does not offer.
    digits: LocalState<u8>,
    /// The count of the account or link last loaded; it stays on offer.
    loaded_digits: LocalState<u8>,
}

/// The digit counts the form offers, in selector order.
const DIGITS: [u8; 2] = [6, 8];

/// The usual counts, each of `extra` in its place, so an account never turns its
/// 7-digit codes into 6-digit ones.
fn digit_choices(extra: &[u8]) -> Vec<u8> {
    let mut counts = DIGITS.to_vec();
    for count in extra {
        if (4..=10).contains(count) && !counts.contains(count) {
            counts.push(*count);
        }
    }
    counts.sort_unstable();
    counts
}
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
            digits: LocalState::new(DIGITS[0]),
            loaded_digits: LocalState::new(DIGITS[0]),
        }
    }

    fn reset(&self) {
        self.uri.set(String::new());
        self.issuer.set(String::new());
        self.label.set(String::new());
        self.wipe_secret();
        self.period.set(DEFAULT_PERIOD.to_owned());
        self.algorithm.set(Algorithm::Sha1.index());
        self.digits.set(DIGITS[0]);
        self.loaded_digits.set(DIGITS[0]);
    }

    /// Zero the key in memory, not just in the field.
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

/// The sheet's form, rebuilt from [`Sheet`] and [`Values`] when its shape changes.
/// A [`ModalDialog`] lays its content out apart from the window, so a field's node
/// is not one the window knows: the app answers focus requests for it.
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
    theme: Option<Theme>,
    theme_key: Option<(u32, u32, u32)>,
    /// The form is taller than the window allows, so it shows a scroll bar.
    overflows: bool,
    /// Scroll to the end on the next build, where a section that just opened is.
    to_end: bool,
    offset: Rc<Cell<(f32, f32)>>,
    /// The window's height, once the sheet has been drawn in it.
    viewport_h: f32,
    stale: bool,
    structure_dirty: bool,
    relayout: bool,

    dirty: bool,
}

impl AddForm {
    fn new(sheet: Shared, values: Values) -> Self {
        Self {
            dirty: true,
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

    fn rebuild(&mut self) {
        self.stale = true;
        self.structure_dirty = true;
    }

    fn take_relayout(&mut self) -> bool {
        std::mem::take(&mut self.relayout)
    }

    fn scroll_to_end(&mut self) {
        self.to_end = true;
    }

    fn visit(widget: &mut dyn Widget, f: &mut dyn FnMut(&mut dyn Widget)) {
        f(widget);
        for child in widget.children_mut() {
            Self::visit(child, f);
        }
    }

    /// How many widgets `kind` selects, which one has focus, and the last one whose
    /// rect contains `p`.
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

    /// The window's own nodes for each text field, in tree order.
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

    fn child_node(taffy: &TaffyTree<()>, node: NodeId) -> NodeId {
        taffy
            .children(node)
            .ok()
            .and_then(|children| children.first().copied())
            .unwrap_or(node)
    }

    fn forward(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        // A focus request names a node of the sheet's own layout, which the window
        // does not know: the app answers it instead.
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
    fn sync(&mut self, state: &AppModel, path: &ModelPath) {
        self.dirty = true;
        // The tree is replaced here, where the window expects a widget to change its
        // structure, never while anything is laying it out.
        if self.stale
            && let Some(theme) = self.theme.clone()
        {
            self.stale = false;
            // The window's height is known only once the sheet has been drawn.
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

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        // The tree is built once the theme is known.
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
        // The form has a node of its own over the scroller's, so the window's focus
        // order pairs each widget with its node.
        let child = self.tree.build_layout(taffy, theme, node_map);
        let style = Style {
            display: taffy::Display::Flex,
            flex_direction: taffy::FlexDirection::Column,
            size: taffy::Size {
                width: taffy::style_helpers::percent(1.0_f32),
                height: taffy::Dimension::auto(),
            },
            ..Style::default()
        };
        taffy.widget_node(style, &[child], node_map, self)
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        let at = self.rect.origin();
        let child = Self::child_node(taffy, node);
        self.tree.apply_layout(taffy, child, at);
        // The scroll bar takes room from the fields, so it shows only when the form
        // is taller than the window allows.
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
        self.tree
            .draw_child(scene, transform, text, theme, presence);
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        self.dirty = true;
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.cursor = ctx.logical(position);
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

    fn needs_redraw(&self) -> bool {
        ui::widget::redraw_or_children(self.dirty, self)
    }
    fn mark_drawn(&mut self) {
        self.dirty = false;
        ui::widget::Widget::mark_children_drawn(self);
    }

    fn name(&self) -> &'static str {
        "AddForm"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }
}

fn is_text_field(widget: &dyn Widget) -> bool {
    widget.ime_content().is_some()
}

fn text_field(
    sheet: &Shared,
    id: FieldId,
    label: &str,
    state: &LocalState<String>,
    kind: TextInputKind,
    placeholder: &str,
    on_change: Option<Msg>,
) -> Element {
    // A stable id keeps the caret in the field when the form is built again.
    let mut input = TextInput::from_state(state)
        .with_id(id.test_id())
        .with_kind(kind)
        .with_placeholder(placeholder);
    // The key grants every future code, so it stays masked until asked for.
    if id == FieldId::Secret {
        input = input.with_secret(true);
    }
    if sheet.borrow().focus == Some(id) {
        input.set_focus(true);
    }
    if let Some(msg) = on_change {
        let queue = Rc::clone(sheet);
        input = input.on_change_view(move |_| queue.borrow_mut().queue.push_back(msg));
    }
    let mut field = Field::new(label, input).with_label_placement(Some(LabelPlacement::Above));
    if let Some(problem) = sheet.borrow().problem.as_ref().filter(|p| p.field == id) {
        field = field.with_error_live(problem.message.clone(), ui::a11y::Politeness::Assertive);
    }
    Box::new(field)
}

fn choice(
    id: &str,
    label: &str,
    labels: &[String],
    selected: usize,
    set: impl FnMut(usize) + 'static,
) -> [Element; 2] {
    let segments = labels.iter().map(Segment::new).collect();
    [
        Box::new(Text::new(label).with_role(TextRole::Label)),
        Box::new(ToggleGroup::new(segments, selected).on_view(id, set)),
    ]
}

fn queued(sheet: &Shared, msg: Msg) -> impl FnMut() + 'static {
    let sheet = Rc::clone(sheet);
    move || sheet.borrow_mut().queue.push_back(msg)
}

fn import_section(sheet: &Shared, values: &Values, uri_expanded: bool) -> Vec<Element> {
    let mut items: Vec<Element> = vec![Box::new(Section::new(vec![
        Box::new(
            ListRow::new("totp-scan", t("totp-add-scan-title"))
                .icon(Icon::Image)
                .secondary(t("totp-add-scan-description"))
                .disclosure()
                .on_effect(queued(sheet, Msg::PickImage)),
        ),
        Box::new(
            ListRow::new("totp-paste-link", t("totp-add-link-title"))
                .icon(Icon::ExternalLink)
                .secondary(t("totp-add-link-description"))
                .disclosure()
                .on_view(queued(sheet, Msg::ExpandLink)),
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
            &t("totp-add-placeholder-issuer"),
            None,
        ),
        text_field(
            sheet,
            FieldId::Label,
            &t("totp-add-label-account"),
            &values.label,
            TextInputKind::Username,
            &t("totp-add-placeholder-account"),
            None,
        ),
        text_field(
            sheet,
            FieldId::Secret,
            &t("totp-add-label-secret"),
            &values.secret,
            TextInputKind::Password,
            &t("totp-add-placeholder-secret"),
            None,
        ),
    ]);
    if headed {
        section.with_header(SectionHeader::group(t("totp-add-details")))
    } else {
        section
    }
}

fn advanced_section(sheet: &Shared, values: &Values, theme: &Theme, open: bool) -> Section {
    let mut toggle = queued(sheet, Msg::ToggleAdvanced);
    let mut items: Vec<Element> = Vec::new();
    if open {
        let algorithms: Vec<String> = Algorithm::ALL
            .iter()
            .map(|algorithm| algorithm.as_str().to_owned())
            .collect();
        let algorithm = values.algorithm.clone();
        let offered = digit_choices(&[
            values.loaded_digits.with(|count| *count),
            values.digits.with(|count| *count),
        ]);
        let counts: Vec<String> = offered.iter().map(u8::to_string).collect();
        let chosen = offered
            .iter()
            .position(|count| values.digits.with(|current| current == count))
            .unwrap_or(0);
        let digits = values.digits.clone();
        // The two choices share a line, so the form stays within an ordinary window.
        let mut row = Row::new().with_gap(theme.tokens.spacing.s3);
        for part in choice(
            "totp-add-algorithm",
            &t("totp-add-label-algorithm"),
            &algorithms,
            values.algorithm.with(|selected| *selected),
            move |index| algorithm.set(index),
        ) {
            row = row.push(part);
        }
        row = row.push(Box::new(Spacer::new()));
        for part in choice(
            "totp-add-digits",
            &t("totp-add-label-digits"),
            &counts,
            chosen,
            move |index| {
                if let Some(count) = offered.get(index) {
                    digits.set(*count);
                }
            },
        ) {
            row = row.push(part);
        }
        items.push(Box::new(row));
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
    let section = Section::new(items).with_header(SectionHeader::group(t("totp-add-advanced")));
    let section = if open {
        section.collapsible()
    } else {
        section.collapsed()
    };
    section.on_expand(move |_| toggle())
}

/// What is left of the window once the sheet's padding, title and footer are out.
/// Unbounded until the window is known.
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

struct Frame<'a> {
    cap: f32,
    bar: bool,
    offset: &'a Rc<Cell<(f32, f32)>>,
    cursor: Point,
}

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
        .push(Box::new(advanced_section(
            sheet,
            values,
            theme,
            advanced_open,
        )));
    Scrollable::new_vertical()
        .with_max_height(frame.cap)
        .with_scrollbar(frame.bar)
        .always_visible()
        .with_persisted_offset(Rc::clone(frame.offset))
        .with_cursor(frame.cursor)
        .with_child(Box::new(groups))
}

fn problem_field(message: &str) -> FieldId {
    if message.contains("secret key") || message.contains("base32") {
        FieldId::Secret
    } else {
        FieldId::Issuer
    }
}

pub struct AddAccountPanel {
    sheet: Shared,
    values: Values,
    /// Boxed so the window's layout map keeps valid addresses into it.
    dialog: Option<Box<ModalDialog<AddForm>>>,
    /// Sheets that closed or were replaced, kept until [`Self::reap`]: the layout
    /// map still points at their widgets.
    retired: Vec<Box<ModalDialog<AddForm>>>,
    /// The account this panel edits; its secret stays in this form and this device's
    /// vault.
    editing: Option<AccountId>,
    /// The shared account this panel sets a local secret up for.
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

    pub fn widget(&self) -> Option<&dyn Widget> {
        self.dialog.as_deref().map(|dialog| dialog as &dyn Widget)
    }

    pub fn widget_mut(&mut self) -> Option<&mut dyn Widget> {
        self.dialog
            .as_deref_mut()
            .map(|dialog| dialog as &mut dyn Widget)
    }

    pub fn field_under(&mut self, p: Point) -> Option<usize> {
        self.dialog.as_mut()?.content_mut().field_under(p)
    }

    pub fn host_field_nodes(&mut self, node_map: &HashMap<NodeId, *mut dyn Widget>) -> Vec<NodeId> {
        self.dialog.as_mut().map_or_else(Vec::new, |dialog| {
            dialog.content_mut().host_field_nodes(node_map)
        })
    }

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

    /// Open the sheet with every field at its default.
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

    fn retire(&mut self) {
        if let Some(mut dialog) = self.dialog.take() {
            dialog.set_open(false);
            self.retired.push(dialog);
        }
    }

    /// Drop the sheets that left the tree; `true` when any did.
    pub fn reap(&mut self) -> bool {
        let reaped = !self.retired.is_empty();
        self.retired.clear();
        reaped
    }

    pub fn open_with_advanced(&mut self) {
        self.open();
        self.sheet.borrow_mut().advanced_open = true;
        self.rebuild();
    }

    /// Open the form for an existing account; only this device's vault supplies the
    /// secret.
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

    /// Open the form to set a local secret up for an existing shared account.
    pub fn open_for_provision(&mut self, account: &Account) {
        self.open();
        self.editing = Some(account.id);
        self.provisioning = Some(account.clone());
        self.load(account, false);
        self.sheet.borrow_mut().focus = Some(FieldId::Secret);
        self.present();
    }

    fn load(&self, account: &Account, with_secret: bool) {
        self.values.issuer.set(account.issuer.clone());
        self.values.label.set(account.label.clone());
        if with_secret {
            self.values.secret.set(account.secret.clone());
        }
        self.values.period.set(account.period.to_string());
        self.values.algorithm.set(account.algorithm.index());
        self.values.digits.set(account.digits);
        self.values.loaded_digits.set(account.digits);
    }

    pub fn take_event(&mut self) -> Option<AddPanelEvent> {
        self.event.take()
    }

    fn rebuild(&mut self) {
        if let Some(dialog) = &mut self.dialog {
            dialog.content_mut().rebuild();
            dialog.set_max_width(ui::widget::modal_dialog::MODAL_MAX_WIDTH);
        }
    }

    /// Act on what the sheet's controls asked for; the app calls this after every
    /// event and each frame.
    pub fn pump(&mut self) {
        loop {
            let msg = self.sheet.borrow_mut().queue.pop_front();
            let Some(msg) = msg else { break };
            self.apply(msg);
        }
        if let Some(dialog) = &mut self.dialog
            && dialog.content_mut().take_relayout()
        {
            dialog.set_max_width(ui::widget::modal_dialog::MODAL_MAX_WIDTH);
        }
    }

    /// Budget the form against a new window height.
    pub fn set_window_height(&mut self, height: f32) {
        if let Some(dialog) = &mut self.dialog {
            let form = dialog.content_mut();
            if (height - form.viewport_h).abs() > 0.5 {
                form.viewport_h = height;
                form.rebuild();
                form.relayout = true;
            }
        }
    }

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
            if field == FieldId::Period {
                sheet.advanced_open = true;
            }
        }
        self.rebuild();
        if field == FieldId::Period
            && let Some(dialog) = &mut self.dialog
        {
            dialog.content_mut().scroll_to_end();
        }
    }

    /// Build an account from the field values, or the problem that stops it being
    /// saved.
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
            .ok()
            .filter(|period| (5..=300).contains(period))
            .ok_or_else(|| problem(FieldId::Period, &t("totp-add-invalid-period")))?;
        let algorithm = Algorithm::ALL
            .get(self.values.algorithm.with(|selected| *selected))
            .copied()
            .unwrap_or_default();
        let digits = Some(self.values.digits.with(|count| *count))
            .filter(|count| (4..=10).contains(count))
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

    /// Fill the form from a decoded `otpauth://` URI, or show inline why it does not
    /// parse.
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
                    // Show non-default parameters so a person sees what the link carried.
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

    #[cfg(test)]
    pub(crate) fn field_snapshot(&self) -> (String, String, String) {
        (
            Values::text(&self.values.issuer),
            Values::text(&self.values.label),
            Values::text(&self.values.secret),
        )
    }

    #[cfg(test)]
    pub(crate) fn caret(&mut self) -> Option<(usize, Option<usize>)> {
        let form = self.dialog.as_mut()?.content_mut();
        let (count, focused, _) = form.survey(is_text_field, Point::ZERO);
        Some((count, focused))
    }

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
mod file_picker_tests;

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::app::AuthenticatorApp;
    use crate::resource::{TotpEffect, TotpShell};
    use ui::prelude::TestApp;

    pub(crate) type Hosted = TestApp<TotpShell>;

    pub(crate) fn settle(app: &mut Hosted) {
        for _ in 0..5 {
            app.harness().relayout();
            app.settle();
        }
    }

    pub(crate) fn hosted(viewport: (u32, u32), scenario: Option<&str>) -> Hosted {
        struct Store(std::sync::Arc<dyn ui::secrets::SecretBackend>);
        impl ui::host::SecretsProvider for Store {
            fn store(&self, app_id: &str) -> ui::secrets::SecretStore {
                ui::secrets::SecretStore::with_backend(app_id, std::sync::Arc::clone(&self.0))
            }
        }
        let backend = ui::secrets::memory_backend();
        let store =
            ui::secrets::SecretStore::with_backend("so.tilo.totp", std::sync::Arc::clone(&backend));
        let mut seeds = crate::secrets::Seeds::new(store, true);
        let accounts = crate::model::demo_accounts();
        let missing = usize::from(scenario == Some("missing-secret"));
        for account in accounts.iter().skip(missing) {
            seeds.put(account.id, &account.secret);
        }
        let mut services = ui::host::current();
        services.secrets = std::sync::Arc::new(Store(backend));
        let _host = ui::host::enter(&services);
        let mut app = Hosted::new().unwrap();
        for account in &accounts {
            let add = app.state().add(account).unwrap();
            app.invoke_confirmed(add).unwrap();
        }
        app.harness().resize(viewport).unwrap();
        settle(&mut app);
        app
    }

    pub(crate) fn panel<T>(app: &mut Hosted, visit: impl FnOnce(&mut AddAccountPanel) -> T) -> T {
        app.harness()
            .with_widget_mut(|widget: &mut AuthenticatorApp| visit(widget.test_panel()))
            .unwrap()
    }

    pub(crate) fn nodes(app: &mut Hosted) -> Vec<ui::a11y::AccessNode> {
        fn access(node: &ui::a11y::AccessNode, out: &mut Vec<ui::a11y::AccessNode>) {
            out.push(node.clone());
            for child in &node.children {
                access(child, out);
            }
        }
        fn walk(node: &ui::headless::InspectionNode, out: &mut Vec<ui::a11y::AccessNode>) {
            if let Some(semantic) = &node.semantics {
                access(semantic, out);
            }
            for child in &node.children {
                walk(child, out);
            }
        }
        let mut out = Vec::new();
        walk(&app.harness().inspection().root, &mut out);
        out
    }

    pub(crate) fn rect(app: &mut Hosted, name: &str) -> Rect {
        if let Some(bounds) = app
            .harness()
            .a11y_nodes()
            .into_iter()
            .find(|(_, node)| {
                node.label() == Some(name)
                    && node.bounds().is_some()
                    && (matches!(
                        format!("{:?}", node.role()).as_str(),
                        "Button" | "TextInput" | "RadioButton"
                    ))
            })
            .and_then(|(_, node)| node.bounds())
        {
            return Rect::new(bounds.x0, bounds.y0, bounds.x1, bounds.y1);
        }
        if let Some(rect) = app.harness().targets().get(name).copied() {
            return rect;
        }
        nodes(app)
            .into_iter()
            .find(|node| node.name.as_deref() == Some(name) && node.bounds.is_some())
            .and_then(|node| node.bounds)
            .map_or_else(
                || panic!("missing actual control {name}"),
                ui::a11y::AccessBounds::to_rect,
            )
    }

    fn reveal(app: &mut Hosted, name: &str) {
        for _ in 0..30 {
            let target = rect(app, name);
            let body = panel(app, |panel| {
                panel
                    .dialog
                    .as_mut()
                    .unwrap()
                    .content_mut()
                    .tree
                    .content_viewport_rect()
            });
            if body.contains(target.center()) {
                return;
            }
            if matches!(name, "Cancel" | "Add account" | "Close") {
                return;
            }
            let at = body.center();
            app.harness()
                .send(&ui::synthetic_input::cursor_moved(at.x, at.y));
            app.harness()
                .scroll(0.0, if target.y0 < body.y0 { 3.0 } else { -3.0 });
            settle(app);
        }
        let target = rect(app, name);
        let geometry = panel(app, |panel| {
            let form = panel.dialog.as_mut().unwrap().content_mut();
            (
                form.tree.content_viewport_rect(),
                form.tree.offset_y(),
                form.tree.max_offset_y(),
                form.viewport_h,
            )
        });
        panic!(
            "control {name} {target:?} cannot be scrolled into the actual form viewport {geometry:?}"
        );
    }

    pub(crate) fn click(app: &mut Hosted, name: &str) {
        if panel(app, |panel| panel.is_open()) {
            reveal(app, name);
        }
        let at = rect(app, name).center();
        app.harness().click_at(at.x, at.y);
        settle(app);
    }

    pub(crate) fn type_into(app: &mut Hosted, name: &str, text: &str) {
        reveal(app, name);
        app.harness().click_named(name).unwrap();
        app.harness().type_text(text);
        settle(app);
    }

    pub(crate) fn opened(viewport: (u32, u32)) -> Hosted {
        let mut app = hosted(viewport, None);
        app.effect(&TotpEffect::AddAccount).unwrap();
        settle(&mut app);
        app
    }

    #[test]
    fn secret_key_is_masked_until_revealed_on_request() {
        let mut app = opened((1280, 900));
        type_into(&mut app, "totp-secret-key", "JBSWY3DPEHPK3PXP");
        let names = |app: &mut Hosted| -> Vec<String> {
            nodes(app)
                .into_iter()
                .filter_map(|node| node.name)
                .collect()
        };
        assert!(
            names(&mut app).iter().any(|name| name == "Show password"),
            "the secret key is masked and offers a show control"
        );
        click(&mut app, "Show password");
        assert!(
            names(&mut app).iter().any(|name| name == "Hide password"),
            "showing the key is a request that can be taken back"
        );
        assert_eq!(
            panel(&mut app, |p| p.field_snapshot().2),
            "JBSWY3DPEHPK3PXP",
            "masking hides the key on screen, not from the form"
        );
    }

    #[test]
    fn add_panel_metrics_reproduce_and_scale_the_authored_bands() {
        let mut app = opened((1280, 900));
        let normal = rect(&mut app, "totp-issuer");
        app.harness().set_theme(Theme::dark().with_text_scale(2.0));
        settle(&mut app);
        let large = rect(&mut app, "totp-issuer");
        assert!(
            large.height() > normal.height(),
            "shared input follows large text"
        );
        let issuer = rect(&mut app, "totp-issuer");
        let label = rect(&mut app, "totp-account-name");
        let secret = rect(&mut app, "totp-secret-key");
        assert!(
            label.y0 >= issuer.y1 && secret.y0 >= label.y1,
            "labelled fields never overlap"
        );
        let cancel = rect(&mut app, "Cancel");
        let save = rect(&mut app, "Add account");
        assert!(
            cancel.intersect(save).area() <= 0.0,
            "footer targets do not overlap"
        );
        assert!(
            cancel.x0 >= 0.0 && save.x1 <= 1280.0 && save.y1 <= 900.0,
            "large text footer stays inside window"
        );
    }

    #[test]
    fn add_panel_disclosures_start_collapsed_and_reveal_on_demand() {
        let mut app = opened((800, 600));
        assert!(!panel(&mut app, |p| p.sheet.borrow().advanced_open));
        assert!(!panel(&mut app, |p| p.sheet.borrow().uri_expanded));
        assert!(!app.harness().targets().contains_key("totp-period"));
        assert!(!app.harness().targets().contains_key("totp-link"));
        click(&mut app, "totp-paste-link");
        assert!(panel(&mut app, |p| p.sheet.borrow().uri_expanded));
        assert!(
            panel(&mut app, |p| p.caret().unwrap().1.is_some()),
            "new setup-link field takes caret"
        );
        click(&mut app, "Advanced options");
        let disclosure = rect(&mut app, "Advanced options");
        let geometry = panel(&mut app, |p| {
            let dialog = p.dialog.as_mut().unwrap();
            (
                dialog.body_rect(),
                dialog.content_mut().tree.content_viewport_rect(),
                dialog.content_mut().tree.offset_y(),
                dialog.content_mut().tree.max_offset_y(),
            )
        });
        assert!(
            panel(&mut app, |p| p.sheet.borrow().advanced_open),
            "actual disclosure {disclosure:?}, modal/form/offset geometry {geometry:?}"
        );
        let period = rect(&mut app, "totp-period");
        let body = panel(&mut app, |p| {
            p.dialog
                .as_mut()
                .unwrap()
                .content_mut()
                .tree
                .content_viewport_rect()
        });
        assert!(
            period.y0 >= body.y0 - 1.0 && period.y1 <= body.y1 + 1.0,
            "revealed period is inside scrolled body"
        );
        assert!(
            panel(&mut app, |p| !p
                .dialog
                .as_mut()
                .unwrap()
                .content_mut()
                .to_end),
            "one-shot reveal consumed"
        );
        panel(&mut app, |p| {
            p.dialog
                .as_mut()
                .unwrap()
                .content_mut()
                .tree
                .scroll_by(0.0, -10000.0);
        });
        settle(&mut app);
        let offset = panel(&mut app, |p| {
            p.dialog.as_mut().unwrap().content_mut().offset.get()
        });
        settle(&mut app);
        assert_eq!(
            panel(&mut app, |p| p
                .dialog
                .as_mut()
                .unwrap()
                .content_mut()
                .offset
                .get()),
            offset,
            "ordinary relayout preserves person's scroll"
        );
        assert_eq!(
            panel(&mut app, |p| p.caret().unwrap().0),
            5,
            "setup link, issuer, account, secret and period all mounted"
        );
    }

    #[test]
    fn add_panel_fits_scrolls_and_reflows_with_the_window() {
        let mut app = opened((1280, 900));
        let wide = rect(&mut app, "totp-add-dialog");
        assert!(wide.y1 <= 900.0 && wide.x1 <= 1280.0);
        assert!(
            !panel(&mut app, |p| p
                .dialog
                .as_mut()
                .unwrap()
                .content_mut()
                .overflows),
            "default form fits tall window"
        );
        app.harness().resize((400, 420)).unwrap();
        settle(&mut app);
        let small = rect(&mut app, "totp-add-dialog");
        assert!(
            small.width() < wide.width() && small.y1 <= 420.0,
            "same dialog narrows and stays in window"
        );
        assert!(
            panel(&mut app, |p| p
                .dialog
                .as_mut()
                .unwrap()
                .content_mut()
                .overflows),
            "short window scrolls shared form"
        );
        let save = rect(&mut app, "Add account");
        assert!(save.y1 <= 420.0, "footer remains reachable");
        app.harness().resize((1280, 900)).unwrap();
        settle(&mut app);
        assert!(
            (rect(&mut app, "totp-add-dialog").width() - wide.width()).abs() < 0.5,
            "widens back"
        );
        assert!(!panel(&mut app, |p| p
            .dialog
            .as_mut()
            .unwrap()
            .content_mut()
            .overflows));
    }

    #[test]
    fn invalid_period_reveals_the_collapsed_field_and_error() {
        fn error_rect(node: &ui::headless::InspectionNode, message: &str) -> Option<Rect> {
            if node.semantics.as_ref().and_then(|s| s.name.as_deref()) == Some(message) {
                let [x, y, width, height] = node.rect;
                return Some(Rect::new(x, y, x + width, y + height));
            }
            node.children
                .iter()
                .find_map(|child| error_rect(child, message))
        }
        for viewport in [(1280, 900), (800, 600)] {
            let mut app = opened(viewport);
            panel(&mut app, |p| {
                p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&period=45");
            });
            settle(&mut app);
            click(&mut app, "Advanced options");
            assert!(!app.harness().targets().contains_key("totp-period"));
            panel(&mut app, |p| p.values.period.set("4".into()));
            click(&mut app, "Add account");
            let field = rect(&mut app, "totp-period");
            let body = panel(&mut app, |p| {
                p.dialog
                    .as_mut()
                    .unwrap()
                    .content_mut()
                    .tree
                    .content_viewport_rect()
            });
            assert!(
                body.contains(field.center()),
                "period field {field:?} outside {body:?}"
            );
            let bounds = error_rect(
                &app.harness().inspection().root,
                &t("totp-add-invalid-period"),
            )
            .expect("the actual form exposes the period error");
            assert!(bounds.area() > 0.0, "period error has a visible layout");
            assert!(
                body.contains(bounds.center()),
                "period error {bounds:?} outside {body:?}"
            );
        }
    }

    #[test]
    fn invalid_period_keeps_the_form_open_until_corrected() {
        let mut app = opened((1280, 900));
        panel(&mut app, |p| {
            p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&period=45");
        });
        settle(&mut app);
        let count = app.state().list().len();
        for invalid in ["", "typo", "0", "4", "301", "4294967296"] {
            panel(&mut app, |p| p.values.period.set(invalid.into()));
            click(&mut app, "Add account");
            assert!(panel(&mut app, |p| p.is_open()));
            assert_eq!(app.state().list().len(), count);
            assert_eq!(panel(&mut app, |p| Values::text(&p.values.period)), invalid);
            assert_eq!(
                panel(&mut app, |p| p.problem()),
                Some(t("totp-add-invalid-period"))
            );
        }
        panel(&mut app, |p| p.values.period.set("45".into()));
        click(&mut app, "Add account");
        assert!(!panel(&mut app, |p| p.is_open()));
        let accounts = app.state().list();
        assert_eq!(accounts.len(), count + 1);
        assert_eq!(
            accounts.iter().find(|a| a.issuer == "Acme").unwrap().period,
            45
        );
    }

    #[test]
    fn add_panel_uri_build_and_save_fill_and_validate_the_form() {
        let mut app = opened((1280, 900));
        panel(&mut app, |p| {
            p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&algorithm=SHA256&digits=8&period=45");
        });
        settle(&mut app);
        assert_eq!(
            panel(&mut app, |p| p.field_snapshot()),
            ("Acme".into(), "bob".into(), "JBSWY3DPEHPK3PXP".into())
        );
        let account = panel(&mut app, |p| p.build_account().unwrap());
        assert_eq!(account.algorithm, Algorithm::Sha256);
        assert_eq!(account.digits, 8);
        assert_eq!(account.period, 45);
        assert!(panel(&mut app, |p| p.sheet.borrow().advanced_open));
        panel(&mut app, super::AddAccountPanel::open);
        panel(&mut app, |p| {
            p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP");
        });
        settle(&mut app);
        assert!(!panel(&mut app, |p| p.sheet.borrow().advanced_open));
        panel(&mut app, |p| p.values.secret.set("not base32 !!".into()));
        assert!(panel(&mut app, |p| p.build_account().is_err()));
        panel(&mut app, |p| p.values.secret.set("JBSWY3DPEHPK3PXP".into()));
        assert!(panel(&mut app, |p| p.build_account().is_ok()));
        let count = app.state().list().len();
        app.harness().key("Enter");
        settle(&mut app);
        assert!(!panel(&mut app, |p| p.is_open()));
        assert_eq!(
            app.state().list().len(),
            count + 1,
            "typed add persists actual account metadata"
        );
        assert!(
            app.state().list().iter().all(|a| a.secret.is_empty()),
            "seeds never reach shared document"
        );
    }

    #[test]
    fn a_digit_count_beyond_six_and_eight_survives_the_form() {
        let mut app = opened((1280, 900));
        panel(&mut app, |p| {
            p.apply_uri("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP&digits=7");
        });
        settle(&mut app);
        assert!(panel(&mut app, |p| p.sheet.borrow().advanced_open));
        assert_eq!(
            panel(&mut app, |p| p.build_account().unwrap()).digits,
            7,
            "a 7-digit setup link makes 7-digit codes"
        );
        click(&mut app, "8");
        assert_eq!(panel(&mut app, |p| p.build_account().unwrap()).digits, 8);
        click(&mut app, "7");
        assert_eq!(panel(&mut app, |p| p.build_account().unwrap()).digits, 7);
        let count = app.state().list().len();
        app.harness().key("Enter");
        settle(&mut app);
        let added = app.state().list();
        assert_eq!(added.len(), count + 1);
        assert!(
            added.iter().any(|a| a.issuer == "Acme" && a.digits == 7),
            "the saved account keeps its 7 digits"
        );

        let mut ten = crate::model::demo_accounts().remove(0);
        ten.digits = 10;
        panel(&mut app, |p| p.open_for_edit(&ten));
        settle(&mut app);
        let edited = panel(&mut app, |p| p.build_account().unwrap());
        assert_eq!(edited.digits, 10, "editing a name leaves the digits alone");
    }

    #[test]
    fn add_panel_clicks_drive_cards_chips_fill_cancel_and_save() {
        let (services, recorder) = ui::host::HostServices::recording();
        let _host = ui::host::enter(&services);
        let mut app = opened((1280, 900));
        click(&mut app, "totp-scan");
        assert_eq!(
            recorder.pick_requests().len(),
            1,
            "pointer QR scan reaches host picker"
        );
        click(&mut app, "totp-paste-link");
        reveal(&mut app, "totp-link");
        app.harness().click_named("totp-link").unwrap();
        app.harness()
            .commit_text("otpauth://totp/Acme:bob?secret=JBSWY3DPEHPK3PXP");
        settle(&mut app);
        assert_eq!(panel(&mut app, |p| p.field_snapshot().0), "Acme");
        assert_eq!(
            panel(&mut app, |p| p.field_snapshot().2),
            "JBSWY3DPEHPK3PXP"
        );
        click(&mut app, "Advanced options");
        click(&mut app, "SHA512");
        click(&mut app, "8");
        let account = panel(&mut app, |p| p.build_account().unwrap());
        assert_eq!(account.algorithm, Algorithm::Sha512);
        assert_eq!(account.digits, 8);
        let count = app.state().list().len();
        click(&mut app, "Cancel");
        assert!(!panel(&mut app, |p| p.is_open()));
        assert_eq!(app.state().list().len(), count, "cancel creates no account");
        app.effect(&TotpEffect::AddAccount).unwrap();
        settle(&mut app);
        type_into(&mut app, "totp-issuer", "Acme");
        type_into(&mut app, "totp-secret-key", "JBSWY3DPEHPK3PXP");
        click(&mut app, "Add account");
        assert!(!panel(&mut app, |p| p.is_open()));
        assert_eq!(
            app.state().list().len(),
            count + 1,
            "pointer Save reaches typed metadata action"
        );
    }
}
