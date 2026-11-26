//! `ContextMenu` primitive — a right-click / long-press invoked list of
//! actions with optional separators. Standalone owners draw it in their
//! overlay pass. `open_anchored` opts into measured, viewport-bounded geometry;
//! legacy `open_at` callers retain their point placement and fixed rows.
//!
//! Callers push `MenuItem::action(label, fn)` and optional
//! `MenuItem::separator()` entries, then call `open_at` on right-click.
//! A menu the context-menu gesture opened takes typing (`query`).

use std::cell::Cell;

use crate::widget::floating_list::{FloatingList, FloatingRow};
use taffy::{NodeId, Size, Style, TaffyTree};
use tilo_ui_base::{widget::AppModel, widget::EventCtx, widget::ModelPath};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Stroke, Vec2};
use winit::event::{ElementState, KeyEvent, MouseButton, MouseScrollDelta, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::widget::floating_surface::{self, PanelAlign, PopoverSide};
use tilo_ui_base::icons::Icon;
use tilo_ui_base::text_render::TextRenderer;
use tilo_ui_base::theme::Theme;
use tilo_ui_base::tokens::TextRole;
use tilo_ui_base::widget::TaffyTreeExt;
use tilo_ui_base::widget::Widget;
use tilo_ui_base::{platform::PlatformConventions, platform::Shortcut};

mod editing;
mod emoji_row;
mod folding;
mod placement;
mod published;
mod query;
mod rows;
mod send_to_box;

pub use editing::EditingCommand;
pub use emoji_row::EmojiRow;
pub use folding::MenuPick;
use placement::is_point;
use query::MenuQuery;
use rows::MenuRow;

/// What a menu row runs. Menus live on the UI thread with the widgets that
/// open them, so a row takes the toolkit's usual single-threaded callback:
/// it may capture `Rc` state like any `Button::on_click`.
pub type MenuAction = std::rc::Rc<dyn Fn()>;

/// Separators are purely visual and ignored by [`ContextMenu::click_item`].
#[derive(Clone)]
pub enum MenuItem {
    Action {
        label: String,
        action: MenuAction,
        /// Optional leading icon for command recognition. Icons are visual
        /// reinforcement only; `label` remains the accessible name.
        icon: Option<Icon>,
        /// The app's own artwork, painted in the icon's place once it has pixels: a row that
        /// names an app ([`MenuItem::with_image`]).
        image: Option<tilo_ui_base::icons::AppImage>,
        /// The face the label is set in, when it previews one: a font picker's rows show each
        /// family in itself ([`MenuItem::with_sample`]).
        sample: Option<tilo_ui_base::text_render::FontChoice>,
        /// `Some` reserves the leading selection gutter and publishes a
        /// semantic checked state. `None` is an ordinary command row.
        checked: Option<bool>,
        /// Destructive rows are styled as danger actions and are never the
        /// implicit keyboard target when a menu opens.
        destructive: bool,
        /// Disabled rows remain visible and announced but cannot be invoked.
        enabled: bool,
        /// Disabled only because the person may not run its action here
        /// ([`MenuItem::refused_unless`]): the keyboard passes over it, and a click still sends
        /// the press, which the dispatch refuses and says why.
        refused: bool,
        /// The keyboard shortcut that runs the same command, shown at the
        /// row's trailing edge in the platform's spelling.
        shortcut: Option<Shortcut>,
        /// A short fact at the row's trailing edge in place of a shortcut:
        /// how many results share a filter's value.
        detail: Option<String>,
        /// What choosing the row does beyond its name ("Open apps close"),
        /// read by assistive technology and never painted.
        description: Option<String>,
        /// `Some` makes the row open a nested menu (with a chevron) instead
        /// of running `action`.
        submenu: Option<Vec<MenuItem>>,
        /// The run of a resource's actions this row stands in (`group: "Due"`),
        /// as the app names it: consecutive rows of one run fold into one row
        /// that opens the run's commands ([`MenuItem::fold_groups`]).
        group: Option<String>,
        /// The row's choice is confirmed before it runs, except that Shift
        /// skips the question for what Undo takes back
        /// ([`MenuItem::asking_first`]).
        asks_first: bool,
        /// The published action choosing the row runs, when it is one
        /// ([`MenuItem::for_action`]).
        address: Option<tilo_actions::ActionAddress>,
        /// What a press of the row does, when the view bound it: the action runs through the
        /// menu's ownership, as a control's own press does.
        press: tilo_ui_base::widget::Press,
    },
    Separator,
    /// A line of secondary text about the rows around it ("Hosted by
    /// example.org"): read, never chosen. Keyboard and pointer pass over it.
    Caption(String),
    /// A row of emoji to pick one from (a message's quick reactions), and a
    /// last cell that opens the emoji picker for any other
    /// ([`MenuItem::emoji_row`]).
    Emoji(EmojiRow),
}

impl std::fmt::Debug for MenuItem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MenuItem::Action { label, submenu, .. } => f
                .debug_struct("MenuItem::Action")
                .field("label", label)
                .field("action", &"<fn>")
                .field("submenu", submenu)
                .finish(),
            MenuItem::Separator => f.write_str("MenuItem::Separator"),
            MenuItem::Caption(text) => f.debug_tuple("MenuItem::Caption").field(text).finish(),
            MenuItem::Emoji(row) => f.debug_tuple("MenuItem::Emoji").field(&row.emoji).finish(),
        }
    }
}

impl MenuItem {
    fn command(label: String, action: MenuAction) -> Self {
        MenuItem::Action {
            label,
            action,
            icon: None,
            image: None,
            sample: None,
            checked: None,
            destructive: false,
            enabled: true,
            refused: false,
            shortcut: None,
            detail: None,
            description: None,
            submenu: None,
            group: None,
            asks_first: false,
            address: None,
            press: tilo_ui_base::widget::Press::default(),
        }
    }

    /// A row that presses what the view bound to it: the action it names runs through the
    /// menu's ownership when it is chosen, as a control's own press does, and the open menu
    /// publishes the row as that action. Every menu of the shell and the toolkit builds its
    /// rows this way.
    #[must_use]
    pub fn bound(label: impl Into<String>, press: tilo_ui_base::widget::Press) -> Self {
        Self::command(label.into(), std::rc::Rc::new(|| {})).for_press(press)
    }

    pub fn action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + 'static,
    {
        Self::command(label.into(), std::rc::Rc::new(action))
    }

    /// A command row with a themed leading icon. Menus containing icon rows
    /// reserve the same gutter for every command so labels stay aligned.
    pub fn icon_action<F>(icon: Icon, label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + 'static,
    {
        Self::action(label, action).with_icon(icon)
    }

    pub fn selection(label: impl Into<String>) -> Self {
        Self::action(label, || {})
    }

    pub fn destructive_action<F>(label: impl Into<String>, action: F) -> Self
    where
        F: Fn() + 'static,
    {
        let mut item = Self::action(label, action);
        if let MenuItem::Action { destructive, .. } = &mut item {
            *destructive = true;
        }
        item
    }

    pub fn destructive_selection(label: impl Into<String>) -> Self {
        Self::destructive_action(label, || {})
    }

    /// A row that opens a nested menu of `items` beside it: on hover, a
    /// click, Enter or the arrow towards it. Its chevron says so.
    pub fn submenu(label: impl Into<String>, items: Vec<MenuItem>) -> Self {
        let mut item = Self::command(label.into(), std::rc::Rc::new(|| {}));
        if let MenuItem::Action { submenu, .. } = &mut item {
            *submenu = Some(items);
        }
        item
    }

    #[must_use]
    pub fn with_icon(mut self, icon: tilo_ui_base::icons::Icon) -> Self {
        if let MenuItem::Action {
            icon: item_icon, ..
        } = &mut self
        {
            *item_icon = Some(icon);
        }
        self
    }

    /// Paint the app `image` names in the row's icon place, its glyph ([`Self::with_icon`]) until
    /// the artwork has pixels: a row that opens or names an app.
    #[must_use]
    pub fn with_image(mut self, image: tilo_ui_base::icons::AppImage) -> Self {
        if let MenuItem::Action {
            image: item_image, ..
        } = &mut self
        {
            *item_image = Some(image);
        }
        self
    }

    /// Set the row's label in `face` instead of the interface's own: a font picker shows each
    /// family in itself, so a person chooses by reading it.
    #[must_use]
    pub fn with_sample(mut self, face: tilo_ui_base::text_render::FontChoice) -> Self {
        if let MenuItem::Action {
            sample: row_sample, ..
        } = &mut self
        {
            *row_sample = Some(face);
        }
        self
    }

    /// The face this row's label is set in, when it previews one
    /// ([`Self::with_sample`]).
    #[must_use]
    pub fn sample(&self) -> Option<&tilo_ui_base::text_render::FontChoice> {
        match self {
            MenuItem::Action { sample, .. } => sample.as_ref(),
            _ => None,
        }
    }

    /// Show the keyboard shortcut that runs the same command at the row's
    /// trailing edge (`Shortcut::command("S")`, `Shortcut::key("Delete")`).
    #[must_use]
    pub fn with_shortcut(mut self, shortcut: impl Into<Shortcut>) -> Self {
        if let MenuItem::Action {
            shortcut: item_shortcut,
            ..
        } = &mut self
        {
            *item_shortcut = Some(shortcut.into());
        }
        self
    }

    /// Show `text` at the row's trailing edge, where a shortcut would stand:
    /// a short fact about the choice, such as how many results share it.
    #[must_use]
    pub fn with_detail(mut self, text: impl Into<String>) -> Self {
        if let MenuItem::Action { detail, .. } = &mut self {
            *detail = Some(text.into());
        }
        self
    }

    /// Say what choosing the row does beyond its name ("Open apps close"):
    /// assistive technology reads it with the row; it is not painted.
    #[must_use]
    pub fn with_description(mut self, text: impl Into<String>) -> Self {
        if let MenuItem::Action { description, .. } = &mut self {
            *description = Some(text.into());
        }
        self
    }

    /// Say that choosing this row asks before it goes ahead (Delete in a
    /// message's menu), which Shift skips for what Undo takes back. The row
    /// says so the way menus do: "Delete message…" while it will ask, and
    /// "Delete message" while Shift is held and it will not.
    #[must_use]
    pub fn asking_first(mut self) -> Self {
        if let MenuItem::Action { asks_first, .. } = &mut self {
            *asks_first = true;
        }
        self
    }

    /// Say that choosing this row runs the published action `address` (a
    /// row's resource action, a bound command): the open menu publishes no
    /// control of its own for the row and names the action as its binding,
    /// so an agent finds the action itself rather than a menu row.
    #[must_use]
    pub fn for_action(mut self, action_address: tilo_actions::ActionAddress) -> Self {
        if let MenuItem::Action { address, .. } = &mut self {
            *address = Some(action_address);
        }
        self
    }

    /// Say that choosing the row presses what the view bound to it (`press`): the row runs the
    /// action itself, through the menu's ownership.
    #[must_use]
    pub fn for_press(mut self, press: tilo_ui_base::widget::Press) -> Self {
        if let MenuItem::Action {
            press: item_press, ..
        } = &mut self
        {
            *item_press = press;
        }
        self
    }

    /// The press choosing the row does, when the view bound it.
    pub fn press(&self) -> Option<&tilo_ui_base::widget::Press> {
        match self {
            MenuItem::Action { press, .. } if !press.is_none() => Some(press),
            _ => None,
        }
    }

    /// What choosing the row does, as the window publishes it for anyone who looks the row up by
    /// what it does rather than by its words: the action on the one record it acts on
    /// ([`tilo_actions::ActionBinding::identity`]), or — for a row that names an action it runs
    /// no press of its own for — the action the view named ([`Self::for_action`]).
    #[must_use]
    pub fn identity(&self) -> Option<String> {
        match self {
            MenuItem::Action { address, press, .. } => press
                .identity()
                .or_else(|| address.clone())
                .map(|identity| identity.encode()),
            MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
        }
    }

    /// The published action choosing the row runs, if it is one: the action the view bound to
    /// the row's press, else the one it named ([`Self::for_action`]).
    pub fn action_address(&self) -> Option<tilo_actions::ActionAddress> {
        match self {
            MenuItem::Action { address, press, .. } => press.address().or_else(|| address.clone()),
            MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
        }
    }

    /// The row's label as it reads now: with the ellipsis of a choice that
    /// asks first, unless Shift is held ([`shift_held`]).
    pub fn shown_label(&self) -> Option<std::borrow::Cow<'_, str>> {
        match self {
            MenuItem::Action {
                label,
                asks_first: true,
                ..
            } if !shift_held() => Some(std::borrow::Cow::Owned(format!("{label}\u{2026}"))),
            _ => self.label().map(std::borrow::Cow::Borrowed),
        }
    }

    /// What the row shows at its trailing edge: its shortcut, else its
    /// detail.
    fn trailing_text(&self, platform: PlatformConventions) -> Option<String> {
        match self {
            MenuItem::Action {
                shortcut: Some(shortcut),
                ..
            } => Some(shortcut.label(platform)),
            MenuItem::Action {
                detail: Some(detail),
                ..
            } => Some(detail.clone()),
            _ => None,
        }
    }

    /// Show the row disabled unless the person may run its action here: a click on it then still
    /// sends the press, to be refused out loud rather than dropped unseen.
    #[must_use]
    pub fn refused_unless(mut self, available: bool) -> Self {
        if let MenuItem::Action {
            enabled, refused, ..
        } = &mut self
        {
            *enabled = available;
            *refused = !available;
        }
        self
    }

    /// Whether a click on the row is taken: an enabled row, or one only its action's refusal
    /// keeps from running ([`Self::refused_unless`]).
    fn takes_click(&self) -> bool {
        self.is_enabled() || matches!(self, MenuItem::Action { refused: true, .. })
    }

    /// Set whether this command can currently be invoked.
    #[must_use]
    pub fn with_enabled(mut self, enabled: bool) -> Self {
        if let MenuItem::Action {
            enabled: item_enabled,
            ..
        } = &mut self
        {
            *item_enabled = enabled;
        }
        self
    }

    /// Make this a state row ("Copy on select", a sort order): a check mark
    /// when `checked`, and a semantic checked state either way. Checked and
    /// unchecked peers share the same leading gutter, so labels never jump
    /// as state changes.
    #[must_use]
    pub fn with_checked(mut self, checked: bool) -> Self {
        if let MenuItem::Action {
            checked: item_checked,
            ..
        } = &mut self
        {
            *item_checked = Some(checked);
        }
        self
    }

    pub fn separator() -> Self {
        MenuItem::Separator
    }

    /// A line of secondary text, never a command; see [`MenuItem::Caption`].
    pub fn caption(text: impl Into<String>) -> Self {
        MenuItem::Caption(text.into())
    }

    pub fn label(&self) -> Option<&str> {
        match self {
            MenuItem::Action { label, .. } => Some(label.as_str()),
            MenuItem::Caption(text) => Some(text.as_str()),
            MenuItem::Separator | MenuItem::Emoji(_) => None,
        }
    }

    /// Semantic checked state for choice/toggle rows. `None` means this is a
    /// regular command, rather than an unchecked choice.
    pub fn checked(&self) -> Option<bool> {
        match self {
            MenuItem::Action { checked, .. } => *checked,
            MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
        }
    }

    /// The shortcut shown on the row, if any.
    pub fn shortcut(&self) -> Option<&Shortcut> {
        match self {
            MenuItem::Action { shortcut, .. } => shortcut.as_ref(),
            MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
        }
    }

    /// The nested menu's rows, for a row that opens one.
    pub fn submenu_items(&self) -> Option<&[MenuItem]> {
        match self {
            MenuItem::Action { submenu, .. } => submenu.as_deref(),
            MenuItem::Separator | MenuItem::Caption(_) | MenuItem::Emoji(_) => None,
        }
    }

    /// Whether it destroys what it acts on: it is drawn as a danger.
    #[must_use]
    pub fn is_destructive(&self) -> bool {
        matches!(
            self,
            MenuItem::Action {
                destructive: true,
                ..
            }
        )
    }

    pub fn is_enabled(&self) -> bool {
        matches!(
            self,
            MenuItem::Action { enabled: true, .. } | MenuItem::Emoji(_)
        )
    }

    /// Whether it is a rule between groups.
    #[must_use]
    pub fn is_separator(&self) -> bool {
        matches!(self, MenuItem::Separator)
    }

    /// A row of `emoji` to pick one from: `pick` runs with the one chosen,
    /// and `more`, when given, is a last cell that opens the emoji picker.
    pub fn emoji_row(
        emoji: Vec<String>,
        pick: impl Fn(&str) + 'static,
        more: Option<MenuAction>,
    ) -> Self {
        MenuItem::Emoji(EmojiRow {
            emoji,
            pick: std::rc::Rc::new(pick),
            more,
        })
    }
}

thread_local! {
    static CONTEXT_GESTURE: Cell<bool> = const { Cell::new(false) };
    static SHIFT: Cell<bool> = const { Cell::new(false) };
}

/// Whether the last press or key was the context-menu gesture ([`note_input`]).
pub(crate) fn context_gesture() -> bool {
    CONTEXT_GESTURE.with(Cell::get)
}

/// Whether the person holds Shift right now, as the window's last
/// modifier change said. Shift is how a person skips a confirmation that
/// Undo makes unnecessary: a menu row that asks first runs at once, and
/// the host's question is not asked (see `action_palette`).
#[must_use]
pub fn shift_held() -> bool {
    SHIFT.with(Cell::get)
}

/// The one rule of every confirmation a person is asked: Shift skips it,
/// if and only if Undo takes the action back (`can_be_undone`, from the
/// action's contract). What Undo cannot take back is asked, Shift or not.
#[must_use]
pub fn shift_skips_confirmation(can_be_undone: bool) -> bool {
    can_be_undone && shift_held()
}

/// Say the person holds Shift, or not, as a window's modifier change does:
/// for a host that learns it otherwise, and for tests.
pub fn note_shift(held: bool) {
    SHIFT.with(|shift| shift.set(held));
}

/// Observe `event` before its window dispatches it, so a menu it opens
/// knows whether it is a context menu: one opened by a secondary click, the
/// Menu key or Shift+F10, rather than by a button. For window hosts.
pub fn note_input(event: &WindowEvent) {
    match event {
        WindowEvent::ModifiersChanged(modifiers) => {
            SHIFT.with(|shift| shift.set(modifiers.state().shift_key()));
        }
        WindowEvent::MouseInput {
            state: ElementState::Pressed,
            button,
            ..
        } => CONTEXT_GESTURE.with(|gesture| gesture.set(*button == MouseButton::Right)),
        WindowEvent::KeyboardInput {
            event:
                KeyEvent {
                    logical_key,
                    state: ElementState::Pressed,
                    ..
                },
            ..
        } => {
            let opens = matches!(logical_key, Key::Named(NamedKey::ContextMenu))
                || (matches!(logical_key, Key::Named(NamedKey::F10)) && SHIFT.with(Cell::get));
            CONTEXT_GESTURE.with(|gesture| gesture.set(opens));
        }
        _ => {}
    }
}

/// Whether `event` is the keyboard's way to open a context menu: the Menu key, or Shift+F10
/// with `modifiers` held. Whatever opens a menu of its own from the keyboard asks this, so the
/// same two keys open every menu.
#[must_use]
pub fn opens_from_keyboard(
    event: &WindowEvent,
    modifiers: winit::keyboard::ModifiersState,
) -> bool {
    matches!(
        event,
        WindowEvent::KeyboardInput {
            event: KeyEvent {
                logical_key,
                state: ElementState::Pressed,
                ..
            },
            ..
        } if matches!(logical_key, Key::Named(NamedKey::ContextMenu))
            || (matches!(logical_key, Key::Named(NamedKey::F10)) && modifiers.shift_key())
    )
}

#[tilo_ui_base::component::not_a_component(
    part,
    "opened by the `ui.ContextMenu` attachment and a row's menu"
)]
pub struct ContextMenu {
    items: Vec<MenuItem>,
    open: bool,
    /// What opened the menu, which it stands against: a control, a row, or a
    /// point (a box with no area). `None` before it has ever opened.
    anchor: Option<Rect>,
    rect: Rect,
    /// `None` when the cursor is off the menu or on a separator.
    hovered: Option<usize>,
    /// `MouseInput` carries no position, so this is tracked from
    /// `CursorMoved` to map a press to a row.
    last_cursor: Point,
    /// The row a press took, which the release on it runs: a press highlights
    /// the row and the menu stays open, so letting go elsewhere cancels it and
    /// what a command starts (a rename's field) never sees half a click.
    held: Option<usize>,
    /// Where the menu was opened at the pointer, in the window's own
    /// logical coordinates, until the pointer moves away from there. A host
    /// repeats a still pointer's position after a rebuild
    /// ([`crate::pointer_replay`]); that repeat is not the person pointing
    /// at a row, so it leaves the keyboard's row where it is.
    opened_under: Option<Point>,
    /// Set by `handle_event`, drained by [`ContextMenu::take_selection`].
    /// The `&mut self` + CRDT-friendly action channel: instead of forcing
    /// every menu action through an `Arc<dyn Fn()>` (which can't splice
    /// into an Automerge doc the host owns), the host polls
    /// `take_selection()` after dispatching the event and mutates itself.
    /// The legacy `Arc` callback still fires too.
    selection: Option<usize>,
    row_height: f64,
    measured_width: f64,
    /// The rows as the one floating list lays them out: their heights, the
    /// bar beside them, the armed row and the arrow keys are the engine's.
    /// It keeps the theme too, so a menu owned by many widgets stays small.
    list: Box<FloatingList<MenuRow>>,
    active_touch: Option<u64>,
    text_scale: f64,
    viewport: Option<Rect>,
    /// The row the menu acts on, ringed while the menu is open.
    anchor_row: Option<Rect>,
    /// Which edge of its anchor the menu lines up with.
    align: PanelAlign,
    /// Which side of its anchor the menu opens on: below a control, beside
    /// the row of a parent menu.
    side: PopoverSide,
    /// The open nested menu and the row that opened it.
    submenu: Option<(usize, Box<ContextMenu>)>,
    /// A pick in a nested menu as `(row, nested row)`, drained by
    /// [`ContextMenu::take_submenu_selection`].
    submenu_selection: Option<(usize, usize)>,
    /// Keyboard focus is in the nested menu.
    submenu_focused: bool,
    platform: PlatformConventions,
    /// The host's input capabilities: on a coarse pointer a row is as tall as
    /// a target must be, since the row is the target.
    interaction: tilo_ui_base::interaction::InteractionEnvironment,
    /// The trailing column (shortcuts, chevrons) from the last measure.
    trailing_width: f64,
    /// Its coming from the pointer or the control that opened it, and
    /// going back (`motion::Appear`).
    appear: tilo_ui_base::motion::Appearing,
    /// The pose it was last drawn in: rows are hit where they are drawn
    /// while it moves.
    drawn: tilo_ui_base::motion::Pose,
    /// What the menu acts on, which a typed question is about.
    about: Option<tilo_ui_base::shared_subject::SharedSelection>,
    /// Where the row this menu adds itself (asking the typed sentence)
    /// sits; its owner's rows end there, and choosing it is never reported
    /// through [`ContextMenu::take_selection`].
    own_rows: Option<usize>,
    /// The owner runs the rows' commands itself, hearing the choice through
    /// [`ContextMenu::take_selection`] ([`ContextMenu::rows_are_the_owners`]).
    rows_are_the_owners: bool,
    /// The query field, on a menu that takes typing.
    query: Option<Box<MenuQuery>>,
    /// The rows shown, in the order shown: every row with no query, else
    /// the entries that match it, best first, and the row that asks it.
    shown: Vec<usize>,
    /// The owner answers a typed sentence itself ([`Self::take_ask`])
    /// rather than the shell, and the menu takes typing however it was
    /// opened: the shell's own menus.
    owner_asks: bool,
    /// The owner hands what its box sends to the session itself, with no app
    /// runner behind it: a shell part's menu, whose host is the session.
    owner_sends: bool,
    /// A sentence asked, for an owner that answers it.
    asked: Option<String>,
    /// While a query narrows the rows, the panel's top edge stays where it
    /// opened: the field under the person's eyes does not move.
    pinned_top: Option<f64>,
    /// Whether anything the menu paints has changed since it was last drawn.
    /// A menu that has settled says so, and the window it floats in stops
    /// asking for frames.
    dirty: bool,
    /// The send-to box the menu's send row opens in the menu's place,
    /// holding the sentence typed into the menu and sending it
    /// ([`super::SendToBox`]).
    send_to: super::SendToBox,
    /// Where the box stands, with no menu around it: beside this rect, which
    /// is what the window pointed at, what the menu was opened over, or its
    /// pointer's place.
    alone: Option<Rect>,
}

impl Default for ContextMenu {
    fn default() -> Self {
        Self::new()
    }
}

impl ContextMenu {
    pub fn new() -> Self {
        Self {
            items: Vec::new(),
            open: false,
            anchor: None,
            rect: Rect::ZERO,
            hovered: None,
            last_cursor: Point::ZERO,
            held: None,
            opened_under: None,
            selection: None,
            row_height: ROW_HEIGHT,
            measured_width: MENU_WIDTH,
            list: {
                let mut list = Box::new(FloatingList::new());
                list.set_overlaid_bars();
                list
            },
            active_touch: None,
            text_scale: 1.0,
            viewport: None,
            anchor_row: None,
            align: PanelAlign::Leading,
            side: PopoverSide::Below,
            submenu: None,
            submenu_selection: None,
            submenu_focused: false,
            platform: PlatformConventions::default(),
            interaction: tilo_ui_base::interaction::InteractionEnvironment::default(),
            trailing_width: 0.0,
            appear: tilo_ui_base::motion::Appearing::closed(),
            drawn: tilo_ui_base::motion::Pose::REST,
            about: None,
            own_rows: None,
            rows_are_the_owners: false,
            query: None,
            shown: Vec::new(),
            owner_asks: false,
            owner_sends: false,
            asked: None,
            pinned_top: None,
            dirty: true,
            send_to: super::SendToBox::new(),
            alone: None,
        }
    }

    pub fn with_items(items: Vec<MenuItem>) -> Self {
        let mut menu = Self::new();
        menu.items = MenuItem::fold_groups(items);
        menu.refilter();
        menu
    }

    /// Say that the rows stand for commands the widget that opened this menu runs: a menu an
    /// area opens over its content does, keeping them beside the menu so the choice survives it
    /// closing. Choosing a row here reports it ([`Self::take_selection`]) and runs nothing, so a
    /// row a person presses, a key takes and a tool or a screen reader activates all end in the
    /// one place the owner runs the command.
    pub fn rows_are_the_owners(&mut self) {
        self.rows_are_the_owners = true;
    }

    /// Replace every row (closes an open nested menu).
    pub fn set_items(&mut self, items: Vec<MenuItem>) {
        self.items = MenuItem::fold_groups(items);
        self.own_rows = None;
        self.submenu = None;
        if self.hovered.is_some_and(|i| i >= self.items.len()) {
            self.set_hover(None);
        }
        self.refilter();
        self.layout_at_position();
        self.dirty = true;
    }

    /// Line the menu up with its anchor's trailing edge (a menu from a
    /// button at the end of a header row) instead of its leading edge.
    pub fn set_align(&mut self, align: PanelAlign) {
        if self.align != align {
            self.align = align;
            self.dirty = true;
        }
        self.layout_at_position();
    }

    /// Mark the row (a message, a feed item, a file) this menu acts on, so
    /// it stays visible as the target while the menu is open. Cleared on
    /// close.
    pub fn anchor_row(&mut self, row: Rect) {
        if self.anchor_row != Some(row) {
            self.anchor_row = Some(row);
            self.dirty = true;
        }
    }

    pub fn push(&mut self, item: MenuItem) {
        self.items.push(item);
        self.refilter();
        self.dirty = true;
    }

    /// Say what the menu acts on (a selection, the item it was opened on,
    /// drawn where it is), so a question typed into it is about that.
    /// Without it the app attaches what is under the pointer, else its
    /// window.
    pub fn set_about(&mut self, about: Option<tilo_ui_base::shared_subject::SharedSelection>) {
        self.send_to.set_about(about.clone());
        self.about = about;
    }

    pub fn items(&self) -> &[MenuItem] {
        &self.items
    }

    pub fn set_item_enabled(&mut self, index: usize, enabled: bool) {
        let Some(MenuItem::Action {
            enabled: item_enabled,
            ..
        }) = self.items.get_mut(index)
        else {
            return;
        };
        if *item_enabled != enabled {
            *item_enabled = enabled;
            self.sync_rows();
            self.dirty = true;
        }
        if !enabled && self.hovered == Some(index) {
            self.set_hover(None);
            self.move_hover(1);
        }
    }

    /// Also computes `rect` immediately (via
    /// [`layout_at_position`](Self::layout_at_position)) so
    /// [`item_at`](Self::item_at) / [`contains`](Self::contains) are usable
    /// without first running a `draw`/`apply_layout` pass — needed by
    /// consumers (like `TextInput`) that route a click to a row in the same
    /// input batch the menu opened in.
    ///
    /// A menu opened by the context-menu gesture (a secondary click, the
    /// Menu key, Shift+F10: [`note_input`]) takes typing, and a sentence it
    /// has no entry for asks an agent about [`Self::set_about`]'s subject at
    /// `position`. Every app's context menus do so with no code of their
    /// own.
    pub fn open_at(&mut self, position: [f32; 2]) {
        let [x, y] = position;
        let at = Rect::new(f64::from(x), f64::from(y), f64::from(x), f64::from(y));
        self.open_over(at);
        // Opened at the pointer, the first row under it is armed unless that
        // row takes something away: a stray press should not be the one that
        // does.
        let first = self
            .items
            .iter()
            .take(self.own_rows.unwrap_or(self.items.len()))
            .find(|item| item.is_enabled())
            .filter(|item| item.is_destructive())
            .and_then(|_| {
                self.items
                    .iter()
                    .position(|item| item.is_enabled() && !item.is_destructive())
            });
        self.set_hover(first);
        self.dirty = true;
    }

    /// Open over `anchor`: a control, a row, or a point (a box of no size).
    /// The menu stands below it, or above it where below has less height, and
    /// inside the window it was opened in. Everything that opens a menu —
    /// the pointer, a control, the keyboard's row — names where it stands
    /// this way, so they are placed by one rule.
    pub fn open_over(&mut self, anchor: Rect) {
        self.send_to.close();
        self.alone = None;
        self.prepare_query();
        self.list.reset_scroll();
        self.open = true;
        self.appear.show(true);
        self.opened_under = is_point(anchor).then_some(anchor.origin());
        self.anchor = Some(anchor);
        self.set_hover(None);
        self.measured_width = MENU_WIDTH * self.text_scale;
        self.row_height = self.row_floor();
        self.list.set_row_height(Some(self.row_height));
        self.layout_at_position();
        self.dirty = true;
    }

    /// Open at `position` inside `viewport`: a menu a widget owns as a field
    /// rather than a tree node, so no frame gives it the window.
    pub fn open_at_clamped(&mut self, position: [f32; 2], viewport: Rect) {
        self.open_at(position);
        self.set_viewport(Some(viewport));
    }

    /// Update menu geometry for the system text scale.
    ///
    /// This is intentionally public for overlay-owning widgets: a menu can be
    /// opened and clicked between layout passes, so merely reading the theme in
    /// [`Widget::draw`] would leave its hit targets at the previous scale.
    pub fn set_text_scale(&mut self, text_scale: f32) {
        let scale = f64::from(text_scale.clamp(0.5, 4.0));
        if (self.text_scale - scale).abs() > f64::EPSILON {
            self.text_scale = scale;
            self.measured_width = MENU_WIDTH * scale;
            self.row_height = self.row_floor();
            self.list.set_row_height(Some(self.row_height));
            self.layout_at_position();
            self.dirty = true;
        }
    }

    /// The smallest a row may be: the text's line and its air, and on a
    /// coarse pointer the toolkit's minimum target, since the row itself is
    /// what a finger presses.
    fn row_floor(&self) -> f64 {
        (ROW_HEIGHT * self.text_scale).max(f64::from(self.interaction.minimum_target()))
    }

    /// Keep the menu inside an owner-provided viewport. It opens on the side
    /// of its anchor with more height, then wheel and keyboard navigation scroll
    /// rows internally when neither side can fit them. The accessibility tree
    /// still exposes every complete label.
    pub fn set_viewport(&mut self, viewport: Option<Rect>) {
        let viewport = viewport.filter(|rect| rect.width() > 0.0 && rect.height() > 0.0);
        if self.viewport != viewport {
            self.viewport = viewport;
            self.send_to.set_viewport(viewport);
            self.dirty = true;
        }
        self.layout_at_position();
        if let Some(alone) = self.alone {
            self.send_to.place(alone, viewport);
        }
    }

    /// Open a shared menu inside a host viewport. Geometry is immediately usable;
    /// call `layout_within` with the renderer before painting to settle text metrics.
    ///
    /// Opened by the context-menu gesture, it takes typing as
    /// [`Self::open_at`] does, a question pointed at the anchor.
    pub fn open_anchored(&mut self, anchor: Rect, viewport: Rect) {
        self.set_viewport(Some(viewport));
        self.open_over(anchor);
    }

    /// Open at `anchor` with the rows it has: a nested menu, whose parent
    /// already offers what the menu adds itself.
    fn show_anchored(&mut self, anchor: Rect, viewport: Rect) {
        self.open = true;
        self.appear.show(true);
        self.opened_under = None;
        self.set_hover(None);
        self.list.reset_scroll();
        self.reanchor(anchor, viewport);
        self.dirty = true;
    }

    /// Stand an open menu against `anchor`, placed inside `viewport`.
    pub fn reanchor(&mut self, anchor: Rect, viewport: Rect) {
        if self.anchor != Some(anchor) || self.viewport != Some(viewport) {
            self.anchor = Some(anchor);
            self.viewport = Some(viewport);
            self.send_to.set_viewport(Some(viewport));
            self.dirty = true;
        }
        self.layout_at_position();
        if let Some(alone) = self.alone {
            self.send_to.place(alone, Some(viewport));
        }
    }

    fn has_leading_gutter(&self) -> bool {
        self.items.iter().any(|item| {
            matches!(
                item,
                MenuItem::Action {
                    checked: Some(_),
                    ..
                } | MenuItem::Action { icon: Some(_), .. }
            )
        })
    }

    pub fn layout_within(&mut self, text: &mut TextRenderer, theme: &Theme) {
        self.set_text_scale(theme.text_scale);
        self.cache_theme(theme);
        let measured = (self.measured_width, self.row_height, self.trailing_width);
        let style = theme.text(TextRole::Body);
        let mut width: f64 = 0.0;
        let mut height: f64 = 0.0;
        let has_leading_gutter = self.has_leading_gutter();
        let mut trailing: f64 = 0.0;
        // The row asking what is typed elides its sentence: the menu keeps
        // the width its own rows give it while the person types.
        for item in self.owner_items() {
            if let Some(label) = item.label() {
                // Measured as it asks, the wider of its two forms.
                let asking;
                let label = if matches!(
                    item,
                    MenuItem::Action {
                        asks_first: true,
                        ..
                    }
                ) {
                    asking = format!("{label}\u{2026}");
                    asking.as_str()
                } else {
                    label
                };
                // A row that previews a face is measured in it, so the panel holds the label
                // whole.
                let row_style = match item.sample() {
                    Some(face) => style.clone().with_family(face.clone()),
                    None => style.clone(),
                };
                let (w, h) = text.measure_style(label, None, &row_style);
                let check_width = if has_leading_gutter {
                    f64::from(theme.tokens.metrics.icon_sm + theme.tokens.spacing.s2)
                } else {
                    0.0
                };
                // The row's trailing text stands in the column measured below,
                // once for the whole menu: counting it here as well made a menu
                // as wide as its longest label with that row's own shortcut
                // added twice, which is how a row with a sentence beside it
                // grew the panel across half the strip.
                width = width.max(f64::from(w) + check_width);
                height = height.max(f64::from(h));
            }
            if let Some(label) = item.trailing_text(self.platform) {
                let (w, _) = text.measure_style(&label, None, &style);
                trailing = trailing.max(f64::from(w));
            }
            if item.submenu_items().is_some() {
                trailing = trailing.max(f64::from(theme.tokens.metrics.icon_sm));
            }
        }
        // The shortcut column stands `s6` clear of the longest label.
        self.trailing_width = if trailing > 0.0 {
            trailing + f64::from(theme.tokens.spacing.s6)
        } else {
            0.0
        };
        // The field that asks what is typed shows its hint whole or not at
        // all: a menu narrower than the hint cuts it mid-word, so the hint is
        // measured with the rows.
        if self.query.is_some() {
            let hint = tilo_ui_base::i18n::I18nManager::lookup_plain("menu-query-placeholder");
            let (w, _) = text.measure_style(&hint, None, &style);
            width = width.max(f64::from(w));
        }
        // The content's inset on both sides; the row is the menu row height,
        // grown with `s2` of air round the line at large text.
        // However short its labels, a menu stays a menu's width.
        self.measured_width =
            (width + self.trailing_width + 2.0 * label_inset(theme)).max(f64::from(
                theme.tokens.metrics.menu_min_width
                    * tilo_ui_base::tokens::Metrics::text_growth(theme.text_scale),
            ));
        self.row_height = (height + f64::from(theme.tokens.spacing.s2))
            .max(f64::from(theme.tokens.metrics.menu_row_height))
            .max(self.row_floor());
        if measured != (self.measured_width, self.row_height, self.trailing_width) {
            self.dirty = true;
        }
        self.sync_rows();
        self.layout_at_position();
        self.set_hover(self.hovered);
    }

    /// Feed the rows to the list: the items shown, in the order shown, with
    /// the trailing text this platform gives them.
    fn sync_rows(&mut self) {
        let rows: Vec<MenuRow> = self
            .shown
            .iter()
            .filter_map(|&index| {
                let item = self.items.get(index)?.clone();
                let trailing = item.trailing_text(self.platform);
                Some(MenuRow {
                    item,
                    trailing,
                    trailing_width: self.trailing_width,
                })
            })
            .collect();
        self.list.set_rows(rows);
        self.list.set_header(self.query_height());
        self.list.set_row_height(Some(self.row_height));
        self.list
            .set_marked(self.marked_row().and_then(|row| self.slot(row)));
        self.list
            .set_highlight(self.hovered.and_then(|index| self.slot(index)));
        self.place_scroll();
    }

    /// The row slot of item `index` among the rows shown.
    fn slot(&self, index: usize) -> Option<usize> {
        self.shown.iter().position(|&shown| shown == index)
    }

    /// Arm item `index` (or nothing), which the list draws armed, and keep it
    /// in view.
    fn set_hover(&mut self, index: Option<usize>) {
        self.hovered = index;
        self.list
            .set_highlight(index.and_then(|index| self.slot(index)));
    }

    /// Height of one row.
    fn item_height(&self, index: usize) -> f64 {
        self.slot(index)
            .map_or(0.0, |slot| self.list.row_height_at(slot))
    }

    /// Width of a point-placed (`open_at`) menu: the fixed reference width,
    /// grown to fit the measured labels so nothing is clipped at a large
    /// text setting. Anchored menus hug their labels instead.
    fn legacy_width(&self) -> f64 {
        self.measured_width.max(MENU_WIDTH)
    }

    fn content_height(&self) -> f64 {
        self.list.content_height()
    }

    /// How far the rows are scrolled, in pixels.
    #[cfg(test)]
    fn scroll_offset(&self) -> f64 {
        self.list.scroll_offset()
    }

    /// Painted (clipped) geometry of an action row, in host coordinates.
    pub fn item_rect(&self, index: usize) -> Option<Rect> {
        if !matches!(self.items.get(index), Some(MenuItem::Action { .. })) {
            return None;
        }
        self.list.row_rect(self.slot(index)?)
    }

    /// The box a row occupies in the rows' scrolling, on screen or scrolled past the panel's
    /// edge: where the row is, so a reader of the menu finds it there rather than nowhere.
    pub fn row_bounds(&self, index: usize) -> Option<Rect> {
        if !self.open {
            return None;
        }
        self.list.row_bounds(self.slot(index)?)
    }

    pub fn focus_item(&mut self, index: usize) -> bool {
        let Some(slot) = self.slot(index) else {
            return false;
        };
        if !matches!(
            self.items.get(index),
            Some(MenuItem::Action { .. } | MenuItem::Emoji(_))
        ) || !self.list.rows()[slot].pickable()
        {
            return false;
        }
        self.set_hover(Some(index));
        self.dirty = true;
        true
    }

    pub fn focus_first_action(&mut self) {
        self.set_hover(None);
        self.dirty = true;
        self.move_hover(1);
    }

    /// Remember the theme the geometry was taken from; the rows' bar needs
    /// it between paints (a wheel, a bar's hover).
    fn cache_theme(&mut self, theme: &Theme) {
        if self.list.theme().tokens != theme.tokens {
            self.list.set_theme(theme);
            self.dirty = true;
        }
    }

    /// Hand `event` to the rows' bar: a press or drag on it, a wheel over
    /// it, the pointer's hover on it. `ctx.handled` says whether a bar
    /// gesture took it; the return value is whether the scroll moved.
    fn scroll_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) -> bool {
        self.place_scroll();
        let moved = self.list.scroll_event(event, ctx);
        if moved {
            self.dirty = true;
        }
        moved
    }

    /// Safe to call when already closed.
    pub fn close(&mut self) {
        tracing::debug!(
            menu = self as *const Self as usize,
            box_open = self.send_to.is_open(),
            "a menu closed"
        );
        self.send_to.close();
        self.alone = None;
        self.close_rows();
    }

    /// Put the rows away, and whatever they opened beside them, leaving the
    /// box as it stands.
    fn close_rows(&mut self) {
        self.pinned_top = None;
        self.open = false;
        self.appear.show(false);
        self.opened_under = None;
        self.active_touch = None;
        self.held = None;
        self.anchor_row = None;
        self.submenu = None;
        self.submenu_focused = false;
        self.dirty = true;
    }

    /// Close with no fade, for a choice that opens something over where the
    /// menu was (a sheet under its scrim): a menu fading out there shows
    /// through it. Safe to call when already closed.
    pub fn close_at_once(&mut self) {
        self.close();
        self.appear.snap(false);
    }

    /// Whether the menu still paints anything: open, or fading out, its rows
    /// or the box that took their place.
    pub fn is_drawn(&self) -> bool {
        self.appear.is_drawn() || self.send_to_alone_drawn()
    }

    /// Whether it has anything to show: its rows, or the send-to box that
    /// took their place. A host shows and routes the menu while this holds.
    pub fn is_open(&self) -> bool {
        self.open || self.send_to.is_open()
    }

    /// Every pixel the open menu painted in its last
    /// [`Widget::draw_overlay`], in the coordinates it was drawn in: its
    /// Panel with the floating shadow, the ring on its anchor row and its
    /// open nested menu. Nothing while it is closed.
    pub fn painted_bounds(&self, theme: &Theme) -> Vec<Rect> {
        // A closing menu still paints until it has gone.
        if !self.appear.is_drawn() {
            return Vec::new();
        }
        // `floating_surface::paint_rounded` clips the shadow to this reach;
        // two more pixels cover the panel's edge stroke and antialiasing.
        let elevation = &theme.tokens.elevation.floating;
        let reach = f64::from(elevation.blur) * 2.0 + f64::from(elevation.offset_y).abs() + 2.0;
        let mut bounds = vec![self.rect.inflate(reach, reach)];
        if let Some(row) = self.anchor_row {
            // A 2 px stroke on the row's outline.
            bounds.push(row.inflate(2.0, 2.0));
        }
        if let Some((_, nested)) = &self.submenu {
            bounds.extend(nested.painted_bounds(theme));
        }
        bounds
    }

    /// The point a pointer opened the menu at, preserved across `close()`
    /// so callers can re-open at the same spot. `None` before the menu has
    /// ever been opened, and for a menu a control or a row opened.
    pub fn position(&self) -> Option<[f32; 2]> {
        #[allow(
            clippy::cast_possible_truncation,
            reason = "a menu anchor is a logical pixel position on screen"
        )]
        self.anchor
            .filter(|anchor| is_point(*anchor))
            .map(|anchor| [anchor.x0 as f32, anchor.y0 as f32])
    }

    /// Lets a consumer route a left-click to
    /// [`click_item`](Self::click_item) without re-deriving the row layout.
    pub fn item_at(&self, point: [f64; 2]) -> Option<usize> {
        if !self.open {
            return None;
        }
        let point = Point::new(point[0], point[1]);
        self.shown
            .iter()
            .copied()
            .find(|&idx| self.item_rect(idx).is_some_and(|rect| rect.contains(point)))
    }

    /// For dismiss-on-click-outside routing.
    pub fn contains(&self, point: [f64; 2]) -> bool {
        if self.send_to.contains(Point::new(point[0], point[1])) {
            return true;
        }
        self.open
            && self
                .rect
                .contains(vello::kurbo::Point::new(point[0], point[1]))
    }

    /// No-op for separators / out-of-range indices. Records the index in
    /// `Self::selection` so a `&mut self` / CRDT host can react via
    /// [`Self::take_selection`].
    pub fn click_item(&mut self, idx: usize) {
        let Some(item) = self.items.get(idx) else {
            return;
        };
        match item {
            MenuItem::Action {
                submenu: Some(_),
                enabled: true,
                ..
            } => self.open_submenu(idx, false),
            MenuItem::Action {
                action,
                enabled: true,
                ..
            } if self.ask_row() == Some(idx) => self.ask_typed(),
            // A row the person may not run is chosen all the same: its press is sent, to be
            // refused with why ([`MenuItem::refused_unless`]).
            MenuItem::Action {
                action,
                press,
                enabled,
                refused,
                submenu: None,
                ..
            } if *enabled || *refused => {
                // A bound row presses its action; a row built around a callback runs it.
                if press.is_none() {
                    let cb = action.clone();
                    cb();
                } else {
                    press.clone().run();
                }
                if self.own_rows.is_none_or(|own| idx < own) {
                    self.selection = Some(idx);
                }
                self.close();
            }
            MenuItem::Emoji(row) => {
                let picked = row.choose(self.list.cell());
                if picked {
                    if self.own_rows.is_none_or(|own| idx < own) {
                        self.selection = Some(idx);
                    }
                    self.close();
                }
            }
            MenuItem::Action { .. } | MenuItem::Separator | MenuItem::Caption(_) => {}
        }
    }

    /// Open the nested menu of row `idx` beside it. `keyboard` puts focus
    /// on its first row.
    pub fn open_submenu(&mut self, idx: usize, keyboard: bool) {
        if self.submenu.as_ref().is_some_and(|(row, _)| *row == idx) {
            if keyboard && let Some((_, menu)) = self.submenu.as_mut() {
                menu.focus_first_action();
                self.submenu_focused = true;
            }
            return;
        }
        let Some(items) = self.items.get(idx).and_then(MenuItem::submenu_items) else {
            self.close_submenu();
            return;
        };
        let Some(row) = self.item_rect(idx) else {
            return;
        };
        let viewport = self
            .viewport
            .unwrap_or(Rect::new(-1.0e6, -1.0e6, 1.0e6, 1.0e6));
        let mut menu = ContextMenu::with_items(items.to_vec());
        menu.side = if self.side == PopoverSide::Left {
            PopoverSide::Left
        } else {
            PopoverSide::Right
        };
        menu.platform = self.platform;
        menu.text_scale = self.text_scale;
        menu.interaction = self.interaction;
        menu.row_height = self.row_height;
        menu.show_anchored(
            Rect::new(self.rect.x0, row.y0, self.rect.x1, row.y1),
            viewport,
        );
        // A nested menu follows the pointer across rows, which is too
        // frequent to animate (hover over menus is instant): it is there.
        menu.appear = tilo_ui_base::motion::Appearing::opened();
        if keyboard {
            menu.focus_first_action();
        }
        self.submenu = Some((idx, Box::new(menu)));
        self.set_hover(Some(idx));
        self.list.set_marked(self.slot(idx));
        self.submenu_focused = keyboard;
        self.dirty = true;
        // Hover opens it without marking the move handled.
        tilo_ui_base::widget::note_resource_presentation_changed();
    }

    /// Close the nested menu, keeping this one open.
    pub fn close_submenu(&mut self) {
        if self.submenu.take().is_some() {
            self.list.set_marked(None);
            self.dirty = true;
            // Hover closes it without marking the move handled.
            tilo_ui_base::widget::note_resource_presentation_changed();
        }
        self.submenu_focused = false;
    }

    pub fn is_submenu_open(&self) -> bool {
        self.submenu.is_some()
    }

    /// A pick in a nested menu as `(row, nested row)`, exactly once. The
    /// nested row's action has already run. For a pick in a menu nested
    /// deeper still, the nested row is the one whose menu held it.
    pub fn take_submenu_selection(&mut self) -> Option<(usize, usize)> {
        self.submenu_selection.take()
    }

    /// Give `event` to the open nested menu when it is the pointer's or the
    /// keyboard's: the pointer while over it, keys while it has focus.
    /// Returns whether it took the event.
    fn route_to_submenu(
        &mut self,
        event: &WindowEvent,
        ctx: &mut tilo_ui_base::widget::EventCtx<'_>,
    ) -> bool {
        if self.submenu.is_none() {
            return false;
        }
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = self.drawn.to_local(ctx.logical(position));
        }
        let p = self.last_cursor;
        let over = self
            .submenu
            .as_ref()
            .is_some_and(|(_, menu)| menu.contains_nested([p.x, p.y]));
        let key = match event {
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => Some(logical_key),
            _ => None,
        };
        let pointer = matches!(
            event,
            WindowEvent::CursorMoved { .. }
                | WindowEvent::MouseInput { .. }
                | WindowEvent::MouseWheel { .. }
        );
        if let Some(key) = key {
            // The keys go where the person is: the nested menu once the keyboard moved into it,
            // or once the pointer went over it, as a row it hovers there is the one they act on.
            if !self.submenu_focused && !over {
                return false;
            }
            // Left or Escape steps back out to this menu's row.
            if matches!(key, Key::Named(NamedKey::ArrowLeft | NamedKey::Escape)) {
                self.close_submenu();
                *ctx.handled = true;
                return true;
            }
        } else if !(pointer && over) {
            if pointer {
                self.submenu_focused = false;
            }
            return false;
        }
        if let Some((_, menu)) = self.submenu.as_mut() {
            menu.handle_event(event, ctx);
        }
        self.drain_submenu();
        *ctx.handled |= !matches!(event, WindowEvent::CursorMoved { .. });
        true
    }

    /// Drain a pick the nested menu just made, or one in a menu nested in
    /// it: the whole menu closes.
    fn drain_submenu(&mut self) {
        let Some((row, menu)) = self.submenu.as_mut() else {
            return;
        };
        let row = *row;
        if let Some(pick) = menu.take_selection() {
            self.submenu_selection = Some((row, pick));
            self.close();
        } else if let Some((deeper, _)) = menu.take_submenu_selection() {
            self.submenu_selection = Some((row, deeper));
            self.close();
        } else if !menu.is_open() {
            self.close_submenu();
        }
    }

    /// Whether `point` is over this menu or a menu nested in it.
    fn contains_nested(&self, point: [f64; 2]) -> bool {
        self.contains(point)
            || self
                .submenu
                .as_ref()
                .is_some_and(|(_, menu)| menu.contains_nested(point))
    }

    /// Returns `Some(idx)` exactly once per selection, then `None`. The
    /// CRDT-friendly action channel — poll it after dispatching an event to
    /// perform a `&mut self` mutation the `Arc<dyn Fn()>` model can't express.
    pub fn take_selection(&mut self) -> Option<usize> {
        self.selection.take()
    }

    /// The box the menu stands against as it opened: a control's or a row's, or the point a
    /// press opened it at, which has no area. For a test that a menu opened where it was asked.
    #[cfg(any(test, feature = "test-support"))]
    pub fn anchor_box(&self) -> Option<Rect> {
        self.anchor
    }

    pub fn hovered(&self) -> Option<usize> {
        self.hovered
    }

    fn row_height(&self) -> f64 {
        self.row_height
    }
    #[cfg(test)]
    fn padding(&self) -> f64 {
        PADDING
    }
    #[cfg(test)]
    fn menu_width(&self) -> f64 {
        self.legacy_width()
    }

    /// Skips separators. `dir > 0` moves down, `dir < 0` up, through the
    /// rows shown in the order shown, wrapping round.
    fn move_hover(&mut self, dir: i32) {
        self.list.move_highlight(dir > 0);
        if let Some(slot) = self.list.highlight() {
            self.hovered = self.shown.get(slot).copied();
            self.dirty = true;
        }
    }

    /// The row under `point` a click is taken by ([`MenuItem::takes_click`]).
    fn enabled_item_at(&self, point: Point) -> Option<usize> {
        self.item_at([point.x, point.y])
            .filter(|index| self.items[*index].takes_click())
    }
}

/// The reference row height at 1× (`metrics.menu_row_height`), used until a
/// paint has measured the labels against the theme.
const ROW_HEIGHT: f64 = tilo_ui_base::tokens::Metrics::BASE.menu_row_height as f64;
const SEPARATOR_HEIGHT: f64 = 9.0;
/// The air between a control and the menu it opened: the `s1` step.
const ANCHOR_GAP: f64 = tilo_ui_base::tokens::Spacing::BASE.s1 as f64;
const MENU_WIDTH: f64 = 220.0;
/// A caption line's share of a row's height.
const CAPTION_ROW_SHARE: f64 = 0.75;
/// The air between the query field and the first row: the `s1` step.
const QUERY_GAP: f64 = tilo_ui_base::tokens::Spacing::BASE.s1 as f64;
/// The rows a menu needs before it takes typing: a short menu is read at a
/// glance, and a field over it only pushes the commands away. At and below
/// this many, the menu opens as a plain list.
pub(super) const QUERY_MIN_ROWS: usize = 12;
/// How far, in logical pixels, the pointer may be from where the menu opened
/// and still count as not having moved: the opening position went through
/// `f32`, the pointer's did not.
const STILL_POINTER: f64 = 0.5;
/// The panel's vertical inset around its rows: the `s2` menu inset
/// (`floating_surface::menu_inset`), so the armed row runs concentric with
/// the panel at the top and bottom too.
const PADDING: f64 = tilo_ui_base::tokens::Spacing::BASE.s2 as f64;

/// Distance from the panel's edge to a row's content, on both sides.
pub(crate) fn label_inset(theme: &Theme) -> f64 {
    f64::from(theme.tokens.spacing.s3)
}

/// Whether item `index` is painted: every action is, and a separator only
/// between two actions and never next to another separator, so a menu built
/// from optional sections has no leading, trailing or doubled rule.
fn item_shown(items: &[MenuItem], index: usize) -> bool {
    let content = |item: &&MenuItem| !matches!(item, MenuItem::Separator);
    match items.get(index) {
        Some(MenuItem::Action { .. } | MenuItem::Caption(_) | MenuItem::Emoji(_)) => true,
        Some(MenuItem::Separator) => {
            let before = items.get(..index).unwrap_or_default();
            let after = items.get(index.saturating_add(1)..).unwrap_or_default();
            let action_before = before.iter().rev().take_while(content).count() > 0;
            let action_after = after.iter().any(|item| content(&item));
            action_before && action_after
        }
        None => false,
    }
}

impl ContextMenu {
    /// The menu's panel and rows, in the pose's transform.
    fn paint_menu(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let rows = self.appear.contents();
        self.list
            .paint_holding(scene, transform, text_renderer, theme, rows);
        let radius = self.list.panel_radius(theme);
        self.paint_query(scene, transform, text_renderer, theme, radius);
    }

    /// The send-to box, where the menu was; its arriving and leaving is its own.
    fn paint_send_to(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        if self.send_to.is_open() || self.send_to.is_drawn() {
            self.send_to.drain_answer();
            if let Some(alone) = self.alone {
                self.send_to.place(alone, self.viewport);
            }
            self.send_to.paint(scene, transform, text, theme);
        }
    }

    /// Where the menu comes from: the control, row or point it stands
    /// against.
    fn appear_anchor(&self) -> Point {
        self.anchor
            .map_or_else(|| self.rect.center(), |anchor| anchor.center())
    }
}

impl Widget for ContextMenu {
    tilo_ui_base::provides!(dyn tilo_ui_base::widget::controls::ActionControls,);

    /// A menu paints its panel in the window's top layer and nothing in
    /// place: it asks for a frame only when what it paints — its own panel
    /// while it is on screen, or a box without it — has changed.
    /// A closed menu is no child of its area's tree, so no frame's
    /// [`Self::mark_drawn`] reaches its flag: what it would paint next is
    /// not a frame the window owes while it paints nothing.
    fn needs_redraw(&self) -> bool {
        (self.is_drawn() && self.dirty)
            || self.send_to.needs_redraw()
            || self
                .submenu
                .as_ref()
                .is_some_and(|(_, menu)| menu.needs_redraw() || menu.needs_animation_frame())
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
        if let Some((_, menu)) = self.submenu.as_mut() {
            menu.mark_drawn();
        }
    }

    fn set_platform_conventions(&mut self, conventions: PlatformConventions) {
        if self.platform != conventions {
            self.platform = conventions;
            self.dirty = true;
        }
    }

    /// A row is the target a person presses, so on a coarse pointer it grows
    /// to the toolkit's minimum and the menu is placed again around it.
    fn set_interaction_environment(
        &mut self,
        environment: tilo_ui_base::interaction::InteractionEnvironment,
    ) {
        if self.interaction != environment {
            self.interaction = environment;
            self.row_height = self.row_height.max(self.row_floor());
            self.list.set_min_row_height(self.row_floor());
            self.list.set_row_height(Some(self.row_height));
            if let Some((_, menu)) = self.submenu.as_mut() {
                menu.set_interaction_environment(environment);
            }
            self.dirty = true;
            self.layout_at_position();
        }
    }

    fn composition_follows_allocation(&self) -> bool {
        true
    }

    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        theme: &Theme,
        node_map: &mut std::collections::HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        self.set_text_scale(theme.text_scale);
        let height = if self.open { self.rect.height() } else { 0.0 };
        // Both are small logical-pixel menu dimensions: MENU_WIDTH is a
        // compile-time constant and `height` is a row-count multiple of
        // ROW_HEIGHT, so neither comes close to f32's range.
        #[allow(
            clippy::cast_possible_truncation,
            reason = "bounded logical-pixel menu dimensions; f32 precision is sufficient"
        )]
        let style = Style {
            size: Size::from_lengths(self.legacy_width() as f32, height as f32),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    /// Both lengths are fixed when the node is built and it measures
    /// nothing: there is never a size for the layout solver to re-solve,
    /// open or closed.
    fn layout_needs_measure(&self) -> bool {
        false
    }

    /// It measures nothing and places itself against what opened it, so it
    /// asks layout only until its box has been given.
    fn is_layout_dirty(&self) -> bool {
        self.rect == Rect::ZERO
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        // Where it stands against what opened it wins over the node's own
        // place; a menu that never opened keeps the position layout gave it.
        if self.anchor.is_some() {
            self.layout_at_position();
            return;
        }
        let l = taffy.layout(node).copied().unwrap_or_default();
        let (x0, y0) = (
            origin.x + f64::from(l.location.x),
            origin.y + f64::from(l.location.y),
        );
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(l.size.width),
            y0 + f64::from(l.size.height),
        );
    }

    /// A menu paints nothing in place: it is a floating Panel in the
    /// window's top layer ([`Widget::draw_overlay`]), so no later sibling,
    /// pane or chrome covers it.
    fn draw(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (_scene, _transform, _text_renderer, theme, mut env) = ctx.parts();
        self.set_text_scale(theme.text_scale);
    }

    fn draw_overlay(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        #[allow(
            unused_variables,
            unused_mut,
            reason = "a draw binds only the parts it uses"
        )]
        let (scene, transform, text_renderer, theme, mut env) = ctx.parts();
        self.set_text_scale(theme.text_scale);
        self.cache_theme(theme);
        // A menu without a viewport of its own flips and shifts inside the
        // window it is presented in, and so does everything it opens beside
        // it.
        if self.viewport.is_none()
            && let Some(window) = env.viewport
        {
            self.set_viewport(Some(window));
        }
        // It comes from where it was opened and goes back
        // (`motion::Appear`); a closing menu is drawn until it has gone but
        // takes no input.
        self.appear.follow(self.open, theme.motion());
        if !self.appear.is_drawn() {
            self.drawn = tilo_ui_base::motion::Pose::REST;
            self.paint_send_to(scene, transform, text_renderer, theme);
            return;
        }

        // Measure against its labels: a menu hugs them, and its rows grow
        // with the text and to the minimum target on a coarse pointer.
        self.layout_within(text_renderer, theme);
        // The row the menu acts on stays marked while it is open.
        if let Some(row) = self.anchor_row {
            scene.stroke(
                &Stroke::new(2.0),
                transform,
                theme.tokens.colors.intent.accent_ink,
                None,
                &tilo_ui_base::surface::edge_outline(row, f64::from(theme.tokens.radii.item)),
            );
        }
        let pose = self.appear.pose(self.rect, self.appear_anchor());
        self.drawn = pose;
        let elevation = &theme.tokens.elevation.floating;
        let reach = f64::from(elevation.blur) * 2.0 + f64::from(elevation.offset_y).abs() + 2.0;
        let bounds = self.rect.inflate(reach, reach);
        pose.paint(scene, transform, bounds, |scene, transform| {
            self.paint_menu(scene, transform, text_renderer, theme);
        });
        if let Some((_, menu)) = self.submenu.as_mut() {
            menu.draw_overlay(&mut tilo_ui_base::draw::DrawCtx::new(
                scene,
                transform,
                text_renderer,
                theme,
                env.clone(),
            ));
        }
        self.paint_send_to(scene, transform, text_renderer, theme);
    }

    fn needs_animation_frame(&self) -> bool {
        self.appear.is_moving()
            || self.send_to.is_moving()
            || self.list.needs_animation_frame()
            || self
                .submenu
                .as_ref()
                .is_some_and(|(_, menu)| menu.needs_animation_frame())
    }

    /// A glide after a flick scrolls the rows as the wheel does.
    fn update(&mut self, _state: &AppModel, _path: &ModelPath) -> bool {
        if self.list.advance() {
            self.dirty = true;
        }
        false
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut tilo_ui_base::widget::EventCtx<'_>) {
        if !self.open {
            if self.send_to.is_open() {
                self.handle_alone(event, ctx);
            }
            return;
        }
        // The window lost the keyboard to another (a press there, a menu
        // there): a menu belongs to what the person does now, so it closes,
        // nested menus and the send box with it.
        if matches!(event, WindowEvent::Focused(false)) {
            self.close();
            return;
        }
        if self.route_to_submenu(event, ctx) {
            return;
        }
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                // A held bar thumb follows the pointer; the bar's hover and
                // the pointer's area are the region's to track.
                self.scroll_event(event, ctx);
                let at = ctx.logical(position);
                // Rows are hit where they are drawn while the menu arrives.
                self.last_cursor = self.drawn.to_local(at);
                // Hover follows the pointer once it moves; until then the
                // row the menu opened on (or the keyboard moved to) stays.
                // The pointer's own place is what is compared: the menu still
                // arriving moves what it draws under a still pointer, and a
                // host repeating that pointer's place is not the person
                // pointing at a row.
                if self
                    .opened_under
                    .is_some_and(|opened| opened.distance(at) <= STILL_POINTER)
                {
                    return;
                }
                self.opened_under = None;
                let hovered = self.enabled_item_at(self.last_cursor);
                if self.hovered != hovered {
                    self.set_hover(hovered);
                    self.dirty = true;
                }
                self.follow_emoji_cell();
                // Hovering a row with a nested menu opens it; hovering
                // another row closes it.
                if let Some(row) = self.hovered {
                    if self
                        .items
                        .get(row)
                        .and_then(MenuItem::submenu_items)
                        .is_some()
                    {
                        self.open_submenu(row, false);
                    } else if self.submenu.is_some() {
                        self.close_submenu();
                    }
                }
                // Not marked handled (so cursor presence still flows) —
                // only presses are.
            }
            // Over the menu, or anywhere while it is open.
            WindowEvent::MouseWheel { delta, .. }
                if self.anchor.is_some()
                    || self.contains([self.last_cursor.x, self.last_cursor.y]) =>
            {
                if !self.scroll_event(event, ctx) {
                    let amount = match delta {
                        MouseScrollDelta::LineDelta(_, y) => -f64::from(*y) * self.row_height(),
                        MouseScrollDelta::PixelDelta(position) => -position.y / ctx.scale_factor,
                    };
                    self.place_scroll();
                    if self.list.scroll_delta(Vec2::new(0.0, amount)) {
                        self.dirty = true;
                    }
                }
                let hovered = self.enabled_item_at(self.last_cursor);
                if self.hovered != hovered {
                    self.set_hover(hovered);
                    self.dirty = true;
                }
                *ctx.handled = true;
            }
            WindowEvent::KeyboardInput {
                event:
                    KeyEvent {
                        logical_key: key,
                        state: ElementState::Pressed,
                        ..
                    },
                ..
            } => match key {
                Key::Named(NamedKey::Escape) => {
                    self.close();
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowDown) => {
                    self.move_hover(1);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowUp) => {
                    self.move_hover(-1);
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::ArrowLeft | NamedKey::ArrowRight)
                    if self.step_emoji_cell(matches!(key, Key::Named(NamedKey::ArrowRight))) =>
                {
                    *ctx.handled = true;
                }
                Key::Named(NamedKey::Enter | NamedKey::ArrowRight)
                    if !matches!(key, Key::Named(NamedKey::ArrowRight))
                        || self.query.is_none()
                        || self.hovered_submenu().is_some() =>
                {
                    if let Some(idx) = self.hovered_submenu() {
                        self.open_submenu(idx, true);
                    } else if let (Key::Named(NamedKey::Enter), Some(idx)) = (key, self.hovered) {
                        self.click_item(idx);
                    } else if matches!(key, Key::Named(NamedKey::Enter)) {
                        // Nothing matches what is typed: Enter asks it.
                        self.ask_typed();
                    }
                    *ctx.handled = true;
                }
                _ => {
                    if self.edit_query(event, *ctx.modifiers) {
                        *ctx.handled = true;
                    }
                }
            },
            WindowEvent::Ime(_) if self.query.is_some() => {
                self.edit_query(event, *ctx.modifiers);
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                // A press starts afresh: one whose release never came (the
                // window lost the pointer) is forgotten, as the host forgets
                // every press it handed out.
                self.held = None;
                // A press on the bar grabs or pages it and takes no row. The
                // region answers in its own flag: the host that routed the
                // press here may have marked it handled already.
                let mut on_bar = false;
                self.place_scroll();
                self.list.press_bar(event, ctx, &mut on_bar);
                if on_bar {
                    // The bar under the press draws differently, held.
                    self.dirty = true;
                    *ctx.handled = true;
                    return;
                }
                if self.place_query_caret(self.last_cursor) {
                    // A press in the field puts the caret there.
                } else if let Some(idx) = self.item_at([self.last_cursor.x, self.last_cursor.y]) {
                    // The row the press took stays lit, and the menu stays open
                    // until the button comes up: the release on this row runs
                    // it, and one anywhere else leaves the menu as it was.
                    self.held = Some(idx);
                    if self.hovered != Some(idx) {
                        self.set_hover(Some(idx));
                        self.dirty = true;
                    }
                } else {
                    // An open menu is modal-ish: swallow the dismiss click
                    // so it doesn't also activate whatever sits underneath.
                    self.close();
                }
                *ctx.handled = true;
            }
            WindowEvent::MouseInput {
                state: ElementState::Released,
                button: MouseButton::Left,
                ..
            } => {
                self.scroll_event(event, ctx);
                if let Some(idx) = self.held.take() {
                    // The press is the row's until this release: a pointer
                    // still over it runs the row, one that left it first runs
                    // nothing and the menu stays open.
                    let over = self.item_at([self.last_cursor.x, self.last_cursor.y]);
                    if over == Some(idx) {
                        self.click_item(idx);
                    }
                    *ctx.handled = true;
                }
            }
            WindowEvent::Touch(touch) => {
                let point = self.drawn.to_local(ctx.logical(touch.location));
                match touch.phase {
                    winit::event::TouchPhase::Started if self.active_touch.is_none() => {
                        self.active_touch = Some(touch.id);
                        self.last_cursor = point;
                        self.set_hover(self.enabled_item_at(point));
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                    winit::event::TouchPhase::Moved if self.active_touch == Some(touch.id) => {
                        self.last_cursor = point;
                        self.set_hover(self.enabled_item_at(point));
                        self.dirty = true;
                        *ctx.handled = true;
                    }
                    winit::event::TouchPhase::Ended if self.active_touch == Some(touch.id) => {
                        self.active_touch = None;
                        self.last_cursor = point;
                        self.set_hover(self.item_at([point.x, point.y]));
                        self.dirty = true;
                        self.follow_emoji_cell();
                        if self.place_query_caret(point) {
                            // A tap in the field types there.
                        } else if let Some(idx) = self.item_at([point.x, point.y]) {
                            self.click_item(idx);
                        } else {
                            self.close();
                        }
                        *ctx.handled = true;
                    }
                    winit::event::TouchPhase::Cancelled if self.active_touch == Some(touch.id) => {
                        self.active_touch = None;
                        self.close();
                        *ctx.handled = true;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "ContextMenu"
    }

    fn traps_focus(&self) -> bool {
        self.is_open()
    }

    /// An open menu that takes typing is where typed text goes: its caret
    /// is the input method's.
    fn ime_cursor_rect(&self) -> Option<Rect> {
        self.query_caret()
    }

    fn ime_content(&self) -> Option<tilo_ui_base::ime::InputContent> {
        self.query_caret()
            .map(|_| tilo_ui_base::ime::InputContent::TEXT)
    }

    fn ime_edit_fingerprint(&self, key: &[u8; 32]) -> Option<[u8; 32]> {
        self.query_fingerprint(key)
    }

    /// Participates in the App's modal-capture contract — suppressing focus
    /// shortcuts while up.
    fn modal_capture(&self) -> Option<Rect> {
        if self.send_to.is_open() {
            return Some(self.send_to.rect());
        }
        self.open.then_some(self.rect)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn appears_from(&self) -> Option<Point> {
        Some(self.appear_anchor())
    }

    fn accessibility_hidden(&self) -> bool {
        // A box opened with no menu around it is on screen with no menu.
        !self.open && !self.send_to_alone_drawn()
    }

    fn accessibility(&self) -> Option<tilo_ui_base::a11y::AccessNode> {
        use tilo_ui_base::{a11y::AccessNode, a11y::AccessState, a11y::Role};
        // Menu items are painted rows rather than child widgets, so publish
        // virtual children. A container-only node made the menu visible to an
        // assistive technology but hid every action inside it.
        let action_count = self
            .shown
            .iter()
            .filter(|&&index| matches!(self.items.get(index), Some(MenuItem::Action { .. })))
            .count();
        let mut action_position: usize = 0;
        let gutter = self.has_leading_gutter();
        let field = self.query_access_node();
        let rows = self
            .shown
            .iter()
            .filter_map(|&index| self.items.get(index).map(|item| (index, item)))
            .filter_map(|(index, item)| {
                if let MenuItem::Emoji(row) = item {
                    return Some(self.emoji_row_node(index, row));
                }
                if let MenuItem::Caption(text) = item {
                    // Read in place among the rows, never focused.
                    return Some(
                        AccessNode::new(Role::StaticText)
                            .with_name(text.clone())
                            .with_bounds(self.row_bounds(index).unwrap_or(Rect::ZERO))
                            .with_state(AccessState {
                                hidden: !self.open,
                                ..Default::default()
                            }),
                    );
                }
                if item.is_separator() {
                    // The rule the menu draws between its sections, so a page draws it too.
                    return Some(
                        AccessNode::new(Role::Separator)
                            .with_bounds(self.row_bounds(index).unwrap_or(Rect::ZERO))
                            .with_state(AccessState {
                                hidden: !self.open,
                                ..Default::default()
                            }),
                    );
                }
                let MenuItem::Action {
                    label,
                    icon,
                    checked,
                    enabled,
                    submenu,
                    description,
                    destructive,
                    ..
                } = item
                else {
                    return None;
                };
                action_position = action_position.saturating_add(1);
                let open_here = self.submenu.as_ref().filter(|(row, _)| *row == index);
                let mut node = AccessNode::new(Role::MenuItem)
                    .with_name(label.clone())
                    .with_icon(*icon)
                    .with_action_target(format!("context-menu:item:{index}"))
                    .with_bounds(self.row_bounds(index).unwrap_or(Rect::ZERO))
                    .with_position(action_position, action_count)
                    .with_state(AccessState {
                        focusable: *enabled,
                        focused: self.open && self.hovered == Some(index) && !self.submenu_focused,
                        hidden: !self.open,
                        checked: *checked,
                        disabled: !*enabled,
                        expanded: submenu.as_ref().map(|_| open_here.is_some()),
                        ..Default::default()
                    });
                // The command names the action it runs, and the one record of the app's that it
                // runs it on: a row's menu and the row's own box press the one operation, so an
                // agent that names what the row does runs it on that row.
                if let Some(identity) = item.identity() {
                    node.action = Some(identity);
                }
                // What the row draws, apart from what it is called and says: an ask-first row's
                // label ends in its marker, and its shortcut stands at its trailing edge.
                node.drawn = Some(tilo_ui_base::a11y::Drawn {
                    label: item
                        .shown_label()
                        .map_or_else(|| label.clone(), std::borrow::Cow::into_owned),
                    trailing: item.trailing_text(self.platform),
                    gutter,
                    danger: *destructive,
                });
                if let Some(description) = description {
                    node.description = Some(description.clone());
                } else if let Some(trailing) = item.trailing_text(self.platform) {
                    node.description = Some(trailing);
                } else if matches!(
                    item,
                    MenuItem::Action {
                        asks_first: true,
                        ..
                    }
                ) {
                    node.description = Some(if shift_held() {
                        "Goes ahead at once; Undo takes it back".to_owned()
                    } else {
                        "Asks first; hold Shift to go ahead at once".to_owned()
                    });
                }
                if let Some((_, menu)) = open_here
                    && let Some(mut nested) = Widget::accessibility(menu.as_ref())
                {
                    // A nested row's address says which row it hangs off: the owner runs its
                    // own rows, and a row of a submenu is one the submenu stands open.
                    for child in &mut nested.children {
                        if let Some(inner) = child
                            .action_target
                            .as_deref()
                            .and_then(|target| target.strip_prefix("context-menu:item:"))
                        {
                            child.action_target =
                                Some(format!("context-menu:item:{index}.{inner}"));
                        }
                    }
                    node.children.push(nested);
                }
                Some(node)
            });
        let children = field
            .into_iter()
            .chain(rows)
            .chain(self.send_to.accessibility())
            .collect();
        Some(
            AccessNode::new(Role::Menu)
                .with_state(AccessState {
                    expanded: Some(self.open),
                    hidden: self.accessibility_hidden(),
                    ..Default::default()
                })
                .with_children(children),
        )
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &tilo_ui_base::a11y::Action,
        _state: &mut tilo_ui_base::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !self.open && !self.send_to.is_open() {
            return false;
        }
        if self.send_to.perform_semantic_action(target, action) {
            // A send pressed by assistive technology or an agent ends the box as a press does.
            if self.send_to.take_sent() {
                self.close();
            }
            return true;
        }
        if target == "context-menu:query" {
            let tilo_ui_base::a11y::Action::SetValue(text) = action else {
                return false;
            };
            self.set_query(text);
            return true;
        }
        if !matches!(action, tilo_ui_base::a11y::Action::Activate) {
            return false;
        }
        if self.activate_emoji_target(target) {
            return true;
        }
        let Some(index) = target
            .strip_prefix("context-menu:item:")
            .and_then(|index| index.parse::<usize>().ok())
        else {
            return false;
        };
        if !self.items.get(index).is_some_and(MenuItem::is_enabled) {
            return false;
        }
        // A row of the owner's is the owner's command: the menu reports the choice
        // ([`Self::take_selection`]) and the owner runs it, so a row asked for by its name is
        // pressed where it stands, instead of a command run here that the owner never hears.
        if self.rows_are_the_owners && index < self.own_rows.unwrap_or(self.items.len()) {
            return false;
        }
        self.click_item(index);
        true
    }
}

mod button;
pub use button::MenuButton;

mod component;
pub use component::{MenuButtonProps, MenuItemProps};

#[cfg(test)]
mod a11y_tests;
#[cfg(test)]
mod typing_tests;

#[cfg(test)]
mod tests;

#[cfg(test)]
mod bounded_tests;

#[cfg(test)]
mod placement_tests;

/// The nested menu and the drawing of a menu, for a test outside this crate.
#[cfg(any(test, feature = "test-support"))]
impl ContextMenu {
    /// Draws the menu as its host's overlay pass would.
    pub fn draw_overlay_for_tests(&mut self, ctx: &mut tilo_ui_base::draw::DrawCtx<'_>) {
        self.draw_overlay(ctx);
    }

    /// The rectangle the menu takes.
    pub fn rect_for_tests(&self) -> Rect {
        self.get_rect()
    }

    /// The nested menu its open row holds and that row's index, if one is open.
    pub fn submenu_for_tests(&self) -> Option<(usize, &ContextMenu)> {
        self.submenu.as_ref().map(|(row, menu)| (*row, &**menu))
    }

    /// The nested menu its open row holds, if one is open.
    pub fn nested_menu_for_tests(&self) -> Option<&ContextMenu> {
        self.submenu.as_ref().map(|(_, menu)| &**menu)
    }
}
