//! First-use workspace launchpad.
//!
//! An empty collaborative desktop should explain the useful things people can
//! do together, with a direct route to open any available app. Each card is a
//! declared action: [`LaunchpadAction`] names it once — its name, the sentence
//! describing it, its picture, and the app action it runs — and the host
//! publishes that same declaration as its own host actions, so the cards,
//! search's empty card and the catalogue agents read offer one set of verbs.
//! "Open an app" is not a verb of its own: it opens the desktop's search for
//! the apps this device has.

use std::cell::Cell;
use std::collections::HashMap;
use ui::widget::ModelPath;

use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::Fill;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::i18n::t;
use tilo_desktop_surface::HostAction;
use ui::TextRole;

/// The "Open an app" button in the desk's targets and the accessibility
/// tree; a card is [`LaunchpadAction::target`].
pub const LAUNCHPAD_OPEN_APP: &str = "launchpad:open-app";

/// An action an app publishes, as the desktop's chrome asks for it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LaunchpadAppAction {
    pub app_id: String,
    pub display_name: String,
    pub resource: String,
    pub action: String,
}

impl LaunchpadAppAction {
    /// The app action that opens (or starts) this person's conversation with
    /// `root` (hex): Chat's own `open-direct-conversation` on the workspace's
    /// conversation with them, its composer ready. Chat publishes the action
    /// on a row named for the person it is with, so the desktop asks for that
    /// row's resource.
    #[must_use]
    pub fn message_person(root: &str) -> Self {
        Self {
            app_id: "so.tilo.chat".to_owned(),
            display_name: "Chat".to_owned(),
            resource: format!("chat.conversation:{root}"),
            action: "open-direct-conversation".to_owned(),
        }
    }
}

/// A welcome verb of the empty desk, declared once: the card that shows it,
/// the host action that offers it to search and agents, and the app action it
/// runs all come from the row below.
#[derive(Clone, Copy, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum LaunchpadAction {
    Chat,
    Notes,
    Tasks,
    Meet,
    Invite,
    Activity,
}

impl LaunchpadAction {
    pub const ALL: [Self; 6] = [
        Self::Chat,
        Self::Notes,
        Self::Tasks,
        Self::Meet,
        Self::Invite,
        Self::Activity,
    ];

    /// A stable, untranslated name for the verb.
    pub const fn slug(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Notes => "notes",
            Self::Tasks => "tasks",
            Self::Meet => "meet",
            Self::Invite => "invite",
            Self::Activity => "activity",
        }
    }

    /// The verb's stable name among the desk's targets and in the
    /// accessibility tree (`launchpad:notes`), so a driver presses the card a
    /// person sees.
    #[must_use]
    pub fn target(self) -> String {
        format!("launchpad:{}", self.slug())
    }

    /// The host action that runs this verb (`launchpad-notes`): what the
    /// session publishes, search lists and an agent invokes.
    #[must_use]
    pub fn host_id(self) -> String {
        format!("launchpad-{}", self.slug())
    }

    /// The verb the host action `id` names, when it is one of the desk's.
    #[must_use]
    pub fn from_host_id(id: &str) -> Option<Self> {
        let slug = id.strip_prefix("launchpad-")?;
        Self::ALL.into_iter().find(|verb| verb.slug() == slug)
    }

    /// The app the verb happens in, with the name a person reads for it: the
    /// launchpad's Chat room, its Notes and Calendar. Inviting and catching up
    /// happen on the desk itself.
    pub const fn app_target(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Chat | Self::Meet => Some(("so.tilo.chat", "Chat")),
            Self::Notes => Some(("so.tilo.notes", "Notes")),
            Self::Tasks => Some(("so.tilo.calendar", "Calendar")),
            Self::Invite | Self::Activity => None,
        }
    }

    /// The typed action the verb invokes in its app: the app, the resource it
    /// publishes the action on, and the action. The host delivers it to the
    /// app in the workspace, launching the app first when it is closed, so
    /// the verb does what it names in one step.
    pub fn app_action(self) -> Option<LaunchpadAppAction> {
        match self {
            Self::Tasks => Some(LaunchpadAppAction {
                app_id: "so.tilo.calendar".to_owned(),
                display_name: "Calendar".to_owned(),
                resource: "calendar.calendar".to_owned(),
                action: "show-tasks".to_owned(),
            }),
            Self::Meet => Some(LaunchpadAppAction {
                app_id: "so.tilo.chat".to_owned(),
                display_name: "Chat".to_owned(),
                resource: "chat.workspace".to_owned(),
                action: "start-call".to_owned(),
            }),
            Self::Chat | Self::Notes | Self::Invite | Self::Activity => None,
        }
    }

    /// The verb's name, as its card and every list of the action read it.
    pub fn label(self) -> String {
        match self {
            Self::Chat => t("launchpad-discuss"),
            Self::Notes => t("launchpad-notes"),
            Self::Tasks => t("launchpad-tasks"),
            Self::Meet => t("launchpad-meet"),
            Self::Invite => t("launchpad-invite"),
            Self::Activity => t("launchpad-activity"),
        }
    }

    /// What the verb does, in the words the workspace's scope makes true:
    /// a private workspace has nobody to write or talk with yet.
    fn detail(self, scope: WorkspaceScope) -> String {
        let private = scope == WorkspaceScope::Private;
        match self {
            Self::Chat if private => t("launchpad-discuss-detail-private"),
            Self::Chat => t("launchpad-discuss-detail"),
            Self::Notes if private => t("launchpad-notes-detail-private"),
            Self::Notes => t("launchpad-notes-detail"),
            Self::Tasks => t("launchpad-tasks-detail"),
            Self::Meet if private => t("launchpad-meet-detail-private"),
            Self::Meet => t("launchpad-meet-detail"),
            Self::Invite if private => t("launchpad-invite-detail-private"),
            Self::Invite => t("launchpad-invite-detail"),
            Self::Activity => t("launchpad-activity-detail"),
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Chat => Icon::MessageCircle,
            Self::Notes => Icon::FileText,
            Self::Tasks => Icon::ListChecks,
            Self::Meet => Icon::Phone,
            Self::Invite => Icon::UserPlus,
            Self::Activity => Icon::History,
        }
    }

    /// Whether search offers the verb before anything is typed: what brings
    /// people in, calls them or shows what they did. A card that starts an app
    /// is looked for by name — search already lists that app by its own name,
    /// and two rows to the same app say less than one.
    pub const fn unasked(self) -> bool {
        matches!(self, Self::Meet | Self::Invite | Self::Activity)
    }

    /// The action the host publishes for this verb: its name, the sentence
    /// describing it, its own picture and whether search may offer it before
    /// anything is typed ([`HostAction`]). The desktops' cards, the palette
    /// and the catalogue agents read all read this.
    #[must_use]
    pub fn host_action(self, scope: WorkspaceScope) -> HostAction {
        HostAction {
            id: self.host_id(),
            title: self.label(),
            subtitle: Some(self.detail(scope)),
            icon: Some(self.icon().name().to_owned()),
            destructive: false,
            unasked: self.unasked(),
            source: None,
        }
    }

    /// Every verb's host action, in the order a person reads the cards.
    #[must_use]
    pub fn host_actions(scope: WorkspaceScope) -> Vec<HostAction> {
        Self::ALL
            .into_iter()
            .map(|verb| verb.host_action(scope))
            .collect()
    }
}

/// What a person chose on the launchpad, waiting for the desk to take it: a
/// card's verb, or the app opener. The launchpad decides nothing itself — the
/// desk carries the choice out through the door every other host action uses.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LaunchpadPress {
    Card(LaunchpadAction),
    OpenApp,
}

/// A tile's icon ink: neutral, secondary at rest and primary while the tile
/// is pointed at or chosen. The accent is never an icon at rest
/// (`docs/design-language.md`, "Colour roles").
fn icon_ink(theme: &Theme, active: bool) -> vello::peniko::Color {
    if active {
        theme.text_primary
    } else {
        theme.text_dim
    }
}

pub struct CollaborationLaunchpad {
    rect: Rect,
    /// The display the launchpad presents on when the canvas spans several.
    presentation: Option<Rect>,
    card_rects: [Rect; 6],
    last_cursor: Point,
    focused: bool,
    selected: usize,
    app_button_size: (f64, f64),
    /// Measured line height of the keyboard hint under the grid. The
    /// "Open an app" button is centred on the hint's line, so the two read
    /// as one footer row; measured during `draw`, defaulting to the
    /// toolkit's line metrics for the caption size before the first frame.
    hint_line_h: f64,
    /// What this widget painted last differs from what it would paint now.
    dirty: bool,
    copy: LaunchpadCopy,
    scope: WorkspaceScope,
    /// What the person chose, waiting for the desk to carry it out.
    press: Cell<Option<LaunchpadPress>>,
    targets: ui::widget::PaintedTargets,
    target_geometry: Option<([Rect; 6], Rect)>,
}

/// Who sees what is made in the workspace the launchpad is shown in, which
/// is what its words about sharing must say.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum WorkspaceScope {
    /// Only the person themselves.
    Private,
    /// The people invited to it.
    #[default]
    Shared,
    /// Anyone who joins: a public or community workspace.
    Public,
}

/// Words a host puts in place of the launchpad's own: a desktop in the
/// browser welcomes a new member and names its own keyboard chords.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct LaunchpadCopy {
    pub title: Option<String>,
    pub subtitle: Option<String>,
    pub keyboard_hint: Option<String>,
}

/// Top of the keyboard hint, from the bottom of the last card.
const HINT_TOP: f64 = 22.0;
/// Top of the footer band, from the bottom of the last card, and the band's
/// minimum height (the "Open an app" button's minimum height).
const FOOTER_TOP: f64 = 14.0;
const FOOTER_MIN_H: f64 = 36.0;
/// Height of the heading band (title + subtitle) above the first card row.
const HEADING_BAND: f64 = 90.0;

impl Default for CollaborationLaunchpad {
    fn default() -> Self {
        Self::new()
    }
}

impl CollaborationLaunchpad {
    /// A launchpad centred in its canvas.
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            presentation: None,
            card_rects: [Rect::ZERO; 6],
            last_cursor: Point::ZERO,
            focused: false,
            selected: 0,
            app_button_size: (160.0, 36.0),
            hint_line_h: f64::from(
                ui::Theme::dark()
                    .text(ui::TextRole::Caption)
                    .line_height_px(),
            ),
            dirty: true,
            copy: LaunchpadCopy::default(),
            scope: WorkspaceScope::default(),
            press: Cell::new(None),
            targets: ui::widget::PaintedTargets::new(),
            target_geometry: None,
        }
    }

    /// Show `copy` where it names text, the launchpad's own words elsewhere.
    #[must_use]
    pub fn with_copy(mut self, copy: LaunchpadCopy) -> Self {
        self.copy = copy;
        self.dirty = true;
        self
    }

    /// Another launchpad saying the same words, for a second display's empty
    /// desk: every empty display shows the start cards, and each takes its
    /// own pointer and keyboard.
    #[must_use]
    pub fn for_another_display(&self) -> Self {
        let mut pad = Self::new();
        pad.copy.clone_from(&self.copy);
        pad.scope = self.scope;
        pad
    }

    /// Say what the workspace shown in is: private, shared or public.
    pub fn set_scope(&mut self, scope: WorkspaceScope) {
        if scope != self.scope {
            self.scope = scope;
            self.dirty = true;
        }
    }

    fn title(&self) -> String {
        self.copy.title.clone().unwrap_or_else(|| {
            t(match self.scope {
                WorkspaceScope::Private => "launchpad-title-private",
                WorkspaceScope::Shared | WorkspaceScope::Public => "launchpad-title",
            })
        })
    }

    fn subtitle(&self) -> String {
        self.copy.subtitle.clone().unwrap_or_else(|| {
            t(match self.scope {
                WorkspaceScope::Private => "launchpad-subtitle-private",
                WorkspaceScope::Shared => "launchpad-subtitle",
                WorkspaceScope::Public => "launchpad-subtitle-public",
            })
        })
    }

    fn keyboard_hint(&self) -> String {
        self.copy
            .keyboard_hint
            .clone()
            .unwrap_or_else(|| t("launchpad-keyboard-hint"))
    }

    /// Centre on `display` instead of the whole canvas, as on a desktop
    /// spanning several displays. `None` returns to the whole canvas.
    pub fn set_presentation_area(&mut self, display: Option<Rect>) {
        let presentation = display;
        if presentation != self.presentation {
            self.targets.cancel_pointer_press();
            self.presentation = presentation;
            self.card_rects = self.cards();
            self.dirty = true;
        }
    }

    /// The part of the canvas the grid centres in.
    fn presentation_rect(&self) -> Rect {
        match self.presentation {
            Some(display) => {
                let area = display.intersect(self.rect);
                if area.width() > 0.0 && area.height() > 0.0 {
                    area
                } else {
                    self.rect
                }
            }
            None => self.rect,
        }
    }

    fn cards(&self) -> [Rect; 6] {
        self.card_layout().0
    }

    fn card_layout(&self) -> ([Rect; 6], usize) {
        Self::card_geometry(self.presentation_rect())
    }

    /// The card drawn at `p`.
    pub fn action_at(&self, p: Point) -> Option<LaunchpadAction> {
        LaunchpadAction::ALL
            .iter()
            .zip(self.card_rects)
            .find(|(_, rect)| rect.contains(p))
            .map(|(action, _)| *action)
    }

    /// The cards and the "Open an app" button by stable name
    /// (`launchpad:notes`, `launchpad:open-app`, ...), as laid out in the
    /// last frame, so a driver can press them like a person would.
    pub fn targets(&self) -> Vec<(String, Rect)> {
        LaunchpadAction::ALL
            .iter()
            .zip(self.card_rects)
            .map(|(action, rect)| (action.target(), rect))
            .chain(std::iter::once((
                LAUNCHPAD_OPEN_APP.to_owned(),
                self.app_button_rect(),
            )))
            .collect()
    }

    /// Take the person's choice, for the desk to carry out on its next pass.
    fn activate(&self, press: LaunchpadPress) -> bool {
        self.press.set(Some(press));
        // The compositor's session log keeps this target
        // (tilo_startup_metrics::INPUT_ROUTE_TARGET).
        tracing::info!(target: "tilo::input_route", ?press, "launchpad: a card was activated");
        true
    }

    /// What the person chose since the last call, cleared so it happens once.
    pub fn take_press(&self) -> Option<LaunchpadPress> {
        self.press.take()
    }

    /// The footer reservation under the grid: `FOOTER_TOP` below the last
    /// card, at least the button's minimum height tall. Card overflow is
    /// measured against this band, so the grid does not move when the
    /// button is repositioned inside it.
    fn footer_band(&self) -> Rect {
        let last = self.card_rects[5];
        Rect::new(
            last.x0,
            last.y1 + FOOTER_TOP,
            last.x1,
            last.y1 + FOOTER_TOP + FOOTER_MIN_H,
        )
    }

    /// What the launchpad presents — heading band, cards and footer — as
    /// opposed to its layout rect, which spans the whole canvas. The access
    /// tree reports this, so a geometry probe does not read chrome floating
    /// over the canvas as overlapping the launchpad.
    /// The layout rect until the first frame has placed the cards.
    fn content_bounds(&self) -> Rect {
        let Some(first) = self.card_rects.first().copied() else {
            return self.rect;
        };
        if first.area() <= 0.0 {
            return self.rect;
        }
        let last = self.card_rects.last().copied().unwrap_or(first);
        let heading = Rect::new(first.x0, first.y0 - HEADING_BAND, last.x1, first.y0);
        self.card_rects
            .iter()
            .fold(heading, |acc, card| acc.union(*card))
            .union(self.app_button_rect())
    }

    fn app_button_rect(&self) -> Rect {
        let last = self.card_rects[5];
        // Centred on the keyboard hint's line: it used to start 14 px below
        // the grid while the hint started 22 px below it, leaving their
        // centres ~4 px apart (HIG §7: controls in one row share a line).
        let centre = last.y1 + HINT_TOP + self.hint_line_h / 2.0;
        let top = centre - self.app_button_size.1 / 2.0;
        Rect::new(
            last.x1 - self.app_button_size.0,
            top,
            last.x1,
            top + self.app_button_size.1,
        )
    }

    /// What the pointer lights up: a card, or the "Open an app" button.
    fn hover_target(&self) -> (Option<usize>, bool) {
        (
            self.hovered(),
            self.app_button_rect().contains(self.last_cursor),
        )
    }

    fn hovered(&self) -> Option<usize> {
        self.card_rects
            .iter()
            .position(|rect| rect.contains(self.last_cursor))
    }

    #[allow(
        clippy::arithmetic_side_effects,
        clippy::cast_precision_loss,
        reason = "bounded six-card renderer geometry uses f64 logical pixels"
    )]
    fn card_geometry(rect: Rect) -> ([Rect; 6], usize) {
        let content_x0 = rect.x0;
        let content_width = rect.x1 - content_x0;
        let panel_w = (content_width - 48.0).clamp(280.0, 900.0);
        let panel_x = content_x0 + (content_width - panel_w) / 2.0;
        let columns: usize = if panel_w >= 760.0 {
            3
        } else if panel_w >= 440.0 {
            2
        } else {
            1
        };
        let gap = 12.0;
        let card_w = (panel_w - gap * (columns.saturating_sub(1) as f64)) / columns as f64;
        let rows = 6_usize.div_ceil(columns);
        let available_h = (rect.height() - 168.0 - gap * (rows.saturating_sub(1) as f64)).max(0.0);
        let card_h = (available_h / rows as f64).clamp(64.0, 96.0);
        let grid_h = (rows as f64).mul_add(card_h, (rows.saturating_sub(1) as f64) * gap);
        let top = rect.center().y - grid_h / 2.0 + 32.0;
        let mut cards = [Rect::ZERO; 6];
        for (index, card) in cards.iter_mut().enumerate() {
            let column = index % columns;
            let row = index / columns;
            let x = panel_x + column as f64 * (card_w + gap);
            let y = top + row as f64 * (card_h + gap);
            *card = Rect::new(x, y, x + card_w, y + card_h);
        }
        (cards, columns)
    }

    fn sync_targets(&mut self) {
        let geometry = (self.card_rects, self.app_button_rect());
        if self.target_geometry != Some(geometry) {
            self.targets.begin_frame();
            for (action, rect) in LaunchpadAction::ALL.into_iter().zip(self.card_rects) {
                self.targets.target(action.target(), rect, action.label());
            }
            self.targets
                .target(LAUNCHPAD_OPEN_APP, geometry.1, t("launchpad-open-app"));
            self.target_geometry = Some(geometry);
        }
        let id = LaunchpadAction::ALL
            .get(self.selected)
            .map_or_else(|| LAUNCHPAD_OPEN_APP.to_owned(), |action| action.target());
        self.targets.focus(&id);
        self.targets.set_host_focus(self.focused);
    }

    fn navigate(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) -> bool {
        let WindowEvent::KeyboardInput { event, .. } = event else {
            return false;
        };
        if !self.focused || event.state != ElementState::Pressed {
            return false;
        }
        let (_, columns) = self.card_layout();
        let last_index = LaunchpadAction::ALL.len();
        let next = match event.logical_key {
            Key::Named(NamedKey::ArrowRight) => {
                Some(self.selected.saturating_add(1).min(last_index))
            }
            Key::Named(NamedKey::ArrowLeft) => Some(self.selected.saturating_sub(1)),
            Key::Named(NamedKey::ArrowDown) => {
                Some(self.selected.saturating_add(columns).min(last_index))
            }
            Key::Named(NamedKey::ArrowUp) => Some(self.selected.saturating_sub(columns)),
            Key::Named(NamedKey::Tab) => {
                if ctx.modifiers.state().shift_key() {
                    Some(self.selected.checked_sub(1).unwrap_or(last_index))
                } else if self.selected >= last_index {
                    Some(0)
                } else {
                    Some(self.selected.saturating_add(1))
                }
            }
            _ => None,
        };
        if let Some(next) = next {
            self.selected = next;
            *ctx.handled = true;
            return true;
        }
        false
    }
}

impl Widget for CollaborationLaunchpad {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        let style = Style {
            flex_grow: 1.0,
            size: taffy::Size::from_percent(1.0_f32, 1.0_f32),
            min_size: taffy::Size::from_percent(0.0_f32, 0.0_f32),
            ..Default::default()
        };
        taffy.widget_leaf(style, node_map, self)
    }

    fn measure(
        &mut self,
        _known: taffy::Size<Option<f32>>,
        _available: taffy::Size<taffy::AvailableSpace>,
        _text_renderer: &mut TextRenderer,
    ) -> taffy::Size<f32> {
        taffy::Size {
            width: 680.0,
            height: 420.0,
        }
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let old_rect = self.rect;
        self.rect = ui::widget::layout_rect(taffy, node, origin);
        if self.rect != old_rect {
            self.targets.cancel_pointer_press();
        }
        let cards = self.cards();
        if cards != self.card_rects {
            self.card_rects = cards;
            self.dirty = true;
        }
    }

    ui::redraw_flag!();

    #[allow(
        clippy::arithmetic_side_effects,
        clippy::cast_possible_truncation,
        clippy::too_many_lines,
        reason = "immediate-mode renderer composes bounded logical-pixel geometry; text width API uses f32"
    )]
    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        // Theme tokens already include accessibility scaling; TextRenderer
        // also applies its configured scale to authored font sizes.
        let scale = text.text_scale();
        let base = theme.tokens.typography.size(TextRole::Body) / scale;
        let small = theme.tokens.typography.size(TextRole::Caption) / scale;
        let caption = theme.tokens.typography.size(TextRole::Caption) / scale;
        let heading = theme.tokens.typography.size(TextRole::Display) / scale;
        let (button_w, button_h) = text.measure(&t("launchpad-open-app"), base, None);
        self.app_button_size = (
            (f64::from(button_w) + 32.0).max(160.0),
            (f64::from(button_h) + 16.0).max(FOOTER_MIN_H),
        );
        let keyboard_hint = self.keyboard_hint();
        let (_, hint_h) = text.measure(&keyboard_hint, caption, None);
        self.hint_line_h = f64::from(hint_h);
        self.card_rects = self.cards();
        // Leave the persistent help affordance its bottom band on laptops.
        // The grid is pushed up by the footer *band* (a fixed reservation),
        // not by where the button happens to sit inside it, so centring the
        // button on the hint's line cannot move the hint.
        let footer_bottom = self.footer_band().y1.max(self.app_button_rect().y1);
        let overflow = (footer_bottom - (self.presentation_rect().y1 - 64.0)).max(0.0);
        for card in &mut self.card_rects {
            *card = *card + vello::kurbo::Vec2::new(0.0, -overflow);
        }
        self.sync_targets();
        let cards = self.card_rects;
        let hovered = LaunchpadAction::ALL
            .iter()
            .position(|action| self.targets.interaction(&action.target()).hovered);
        let content_left = cards.first().map_or(self.rect.x0, |card| card.x0);
        let heading_y = cards.first().map_or(self.rect.y0, |card| card.y0) - HEADING_BAND;
        let panel_width = cards[5].x1 - content_left;
        let title = text.elide_to_width(&self.title(), heading, panel_width as f32);
        text.draw_text(
            scene,
            &title,
            heading,
            theme.text_primary,
            transform * Affine::translate((content_left, heading_y)),
            None,
        );
        let (_, title_h) = text.measure(&title, heading, None);
        let subtitle = text.elide_to_width(&self.subtitle(), small, panel_width as f32);
        let (_, subtitle_h) = text.measure(&subtitle, small, None);
        if f64::from(title_h + subtitle_h) + 9.0 <= HEADING_BAND {
            // On the bare wallpaper, like the hint under the cards: the dim
            // tier's contrast holds only on the theme's own surfaces.
            text.draw_text(
                scene,
                &subtitle,
                small,
                theme.text_primary,
                transform * Affine::translate((content_left, heading_y + f64::from(title_h) + 3.0)),
                None,
            );
        }

        // The tiles are actions, so each is a secondary action's face: the
        // raised level with no edge, a neutral lift under the pointer and
        // the press, and the keyboard focus ring outside it
        // (`docs/design-language.md`, "Buttons" and "Lifts"). The icon is
        // neutral ink: secondary at rest, primary while the tile is the one
        // being pointed at or chosen; the accent is never an icon at rest.
        for (index, (action, rect)) in LaunchpadAction::ALL.into_iter().zip(cards).enumerate() {
            let interaction = self.targets.interaction(&action.target());
            let focused = self.focused && self.selected == index;
            let hovered_here = hovered == Some(index);
            let (fill, _, _) = ui::widget::face_colors(
                theme,
                ui::widget::ButtonRole::Secondary,
                ui::widget::Interaction {
                    hovered: hovered_here,
                    pressed: interaction.pressed,
                    focused,
                    disabled: false,
                },
            );
            let radius = ui::chrome::ChromeRole::Control.radius(theme, rect.height());
            scene.fill(
                Fill::NonZero,
                transform,
                fill,
                None,
                &RoundedRect::from_rect(rect, radius),
            );
            if focused {
                ui::widget::paint_focus_ring(scene, transform, rect, radius, theme);
            }
            let icon_rect = Rect::new(
                rect.x0 + 18.0,
                rect.y0 + 18.0,
                rect.x0 + 42.0,
                rect.y0 + 42.0,
            );
            action.icon().draw_with_stroke(
                scene,
                transform,
                icon_rect,
                icon_ink(theme, hovered_here || focused),
                1.6,
            );
            let label = text.elide_to_width(&action.label(), base, (rect.width() - 72.0) as f32);
            text.draw_text(
                scene,
                &label,
                base,
                theme.text_primary,
                transform * Affine::translate((rect.x0 + 56.0, rect.y0 + 16.0)),
                None,
            );
            let (_, label_h) = text.measure(&label, base, None);
            let (_, detail_h) = text.measure(&action.detail(self.scope), caption, None);
            let detail_y = (16.0 + f64::from(label_h) + 6.0).max(47.0);
            if detail_y + f64::from(detail_h) + 6.0 <= rect.height() {
                let detail = text.elide_to_width(
                    &action.detail(self.scope),
                    caption,
                    (rect.width() - 72.0) as f32,
                );
                text.draw_text(
                    scene,
                    &detail,
                    caption,
                    theme.text_dim,
                    transform * Affine::translate((rect.x0 + 56.0, rect.y0 + detail_y)),
                    Some((rect.width() - 72.0) as f32),
                );
            }
        }

        let button = self.app_button_rect();
        // The shared `Button` face (HIG §9.9) at the Control radius (§9.1):
        // fill, border, hover, focus ring and measured label centring come
        // from the toolkit rather than a local copy of them. `base` is
        // renderer-relative here (divided above), which is what the face
        // expects from a widget painted at the tree's scale.
        Button::paint(
            scene,
            transform,
            text,
            theme,
            &ButtonFace::new(button, &t("launchpad-open-app"))
                .control_size(ui::widget::ControlSize::Large)
                .focused(self.focused && self.selected == 6)
                .hovered(self.targets.interaction(LAUNCHPAD_OPEN_APP).hovered)
                .pressed(self.targets.interaction(LAUNCHPAD_OPEN_APP).pressed),
        );
        if let Some(last) = cards.last() {
            let hint_width = (button.x0 - content_left - 12.0).max(0.0) as f32;
            let hint = text.elide_to_width(&keyboard_hint, caption, hint_width);
            // The button is centred on this line (`app_button_rect`), so the
            // hint stays where it was and the pair share one centre.
            text.draw_text(
                scene,
                &hint,
                caption,
                theme.text_primary,
                transform * Affine::translate((content_left, last.y1 + HINT_TOP)),
                None,
            );
        }
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        let button = match event {
            WindowEvent::MouseInput { state, .. } => Some(*state),
            _ => None,
        };
        if *ctx.handled {
            if let Some(state) = button {
                // The compositor's session log keeps this target
                // (tilo_startup_metrics::INPUT_ROUTE_TARGET).
                tracing::info!(target: "tilo::input_route", ?state, "launchpad: a button another widget handled first");
            }
            return;
        }
        let before = (self.hover_target(), self.selected, self.focused);
        self.sync_targets();
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor = ctx.logical(position);
        }
        if !self.navigate(event, ctx) {
            let before = button.map(|_| {
                (
                    self.targets.hovered().map(str::to_owned),
                    self.targets.pressed().map(str::to_owned),
                )
            });
            self.dirty |= self.targets.handle_event(event, ctx);
            if let (Some(state), Some((hovered, pressed))) = (button, before) {
                tracing::info!(
                    target: "tilo::input_route",
                    ?state,
                    x = self.last_cursor.x,
                    y = self.last_cursor.y,
                    ?hovered,
                    pressed_before = ?pressed,
                    pressed_after = ?self.targets.pressed(),
                    "launchpad: a button reached the cards"
                );
            }
            if let Some(id) = self.targets.focused() {
                self.selected = LaunchpadAction::ALL
                    .iter()
                    .position(|action| action.target() == id)
                    .unwrap_or(6);
            }
            if matches!(event, WindowEvent::Focused(false)) {
                self.focused = false;
            }
            for id in self.targets.take_activated() {
                let choice = LaunchpadAction::ALL
                    .into_iter()
                    .find(|action| action.target() == id)
                    .map_or(LaunchpadPress::OpenApp, LaunchpadPress::Card);
                self.activate(choice);
            }
        }
        self.dirty |= (self.hover_target(), self.selected, self.focused) != before;
    }

    fn wants_pointer_capture(&self) -> bool {
        self.targets.wants_pointer_capture()
    }

    fn cursor_shape(&self, point: Point) -> Option<winit::window::CursorIcon> {
        (self.card_rects.iter().any(|rect| rect.contains(point))
            || self.app_button_rect().contains(point))
        .then_some(winit::window::CursorIcon::Pointer)
    }

    fn is_focusable(&self) -> bool {
        true
    }

    fn set_focus(&mut self, focused: bool) {
        self.dirty |= self.focused != focused;
        self.focused = focused;
        if !focused {
            self.dirty |= self.targets.wants_pointer_capture();
            self.targets.cancel_pointer_press();
        }
        self.targets.set_host_focus(focused);
    }

    fn is_focused(&self) -> bool {
        self.focused
    }

    fn name(&self) -> &'static str {
        "CollaborationLaunchpad"
    }

    fn as_any(&self) -> Option<&dyn std::any::Any> {
        Some(self)
    }

    fn as_any_mut(&mut self) -> Option<&mut dyn std::any::Any> {
        Some(self)
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    fn accessibility(&self) -> Option<AccessNode> {
        Some(
            AccessNode::new(Role::Group)
                .with_name(self.title())
                .with_bounds(self.content_bounds())
                .with_description(t("launchpad-description"))
                .with_children(
                    LaunchpadAction::ALL
                        .into_iter()
                        .enumerate()
                        .zip(self.card_rects)
                        .map(|((index, action), bounds)| {
                            AccessNode::new(Role::Button)
                                .with_name(action.label())
                                .with_bounds(bounds)
                                .with_description(action.detail(self.scope))
                                .with_action_target(action.target())
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(self.focused && self.selected == index),
                                )
                        })
                        .chain(std::iter::once(
                            AccessNode::new(Role::Button)
                                .with_name(t("launchpad-open-app"))
                                .with_action_target(LAUNCHPAD_OPEN_APP)
                                .with_bounds(self.app_button_rect())
                                .with_state(
                                    AccessState::new()
                                        .focusable(true)
                                        .focused(self.focused && self.selected == 6),
                                ),
                        ))
                        .collect(),
                ),
        )
    }

    fn synthetic_accessibility_rect(&self, path: &[usize]) -> Option<Rect> {
        let [index] = path else { return None };
        if *index == 6 {
            Some(self.app_button_rect())
        } else {
            self.card_rects.get(*index).copied()
        }
    }

    fn focus_synthetic_accessibility(&mut self, path: &[usize]) -> bool {
        let [index] = path else { return false };
        if *index > LaunchpadAction::ALL.len() {
            return false;
        }
        self.focused = true;
        self.selected = *index;
        self.dirty = true;
        true
    }

    fn perform_semantic_action(
        &mut self,
        target: &str,
        action: &Action,
        _state: &mut ui::widget::Edits<'_>,
        _path: &ModelPath,
    ) -> bool {
        if !matches!(action, Action::Activate) {
            return false;
        }
        if target == LAUNCHPAD_OPEN_APP {
            return self.activate(LaunchpadPress::OpenApp);
        }
        LaunchpadAction::ALL
            .into_iter()
            .find(|verb| verb.target() == target)
            .is_some_and(|verb| self.activate(LaunchpadPress::Card(verb)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;

    fn deliver(
        pad: &mut CollaborationLaunchpad,
        doc: &mut AutoCommit,
        event: &WindowEvent,
    ) -> bool {
        ui::testing::Dispatch::new()
            .doc(doc)
            .send(pad, event)
            .handled
    }

    use ui::synthetic_input::mouse_release;

    #[test]
    fn the_sharing_copy_follows_the_workspace_scope() {
        let mut pad = CollaborationLaunchpad::new();
        pad.set_scope(WorkspaceScope::Private);
        assert_eq!(pad.subtitle(), t("launchpad-subtitle-private"));
        assert_ne!(
            pad.subtitle(),
            t("launchpad-subtitle"),
            "a private workspace is never said to be shared with everyone"
        );
        assert_eq!(
            LaunchpadAction::Notes.detail(WorkspaceScope::Private),
            t("launchpad-notes-detail-private")
        );
        pad.set_scope(WorkspaceScope::Shared);
        assert_eq!(pad.subtitle(), t("launchpad-subtitle"));
        assert_eq!(pad.title(), t("launchpad-title"));
        pad.set_scope(WorkspaceScope::Public);
        assert_eq!(pad.subtitle(), t("launchpad-subtitle-public"));
        let copy = LaunchpadCopy {
            subtitle: Some("Welcome".into()),
            ..LaunchpadCopy::default()
        };
        let hosted = CollaborationLaunchpad::new().with_copy(copy);
        assert_eq!(hosted.subtitle(), "Welcome", "a host's own words still win");
    }

    /// The cards, the host actions search lists and the verbs agents read are
    /// one declaration: every card publishes a row of its own, with the name,
    /// the picture and the flags the card shows.
    #[test]
    fn every_card_publishes_one_host_action_of_its_own() {
        let published = LaunchpadAction::host_actions(WorkspaceScope::Shared);
        assert_eq!(published.len(), LaunchpadAction::ALL.len());
        for (action, card) in published.iter().zip(LaunchpadAction::ALL) {
            assert_eq!(action.id, card.host_id());
            assert_eq!(action.title, card.label());
            assert_eq!(action.subtitle, Some(card.detail(WorkspaceScope::Shared)));
            assert_eq!(action.icon, Some(card.icon().name().to_owned()));
            assert!(!action.destructive, "no card takes anything away");
            assert_eq!(action.unasked, card.unasked());
            assert_eq!(action.source, None, "a card is no menu's row");
            assert_eq!(LaunchpadAction::from_host_id(&action.id), Some(card));
        }
        assert_eq!(
            LaunchpadAction::from_host_id("launchpad-open-app"),
            None,
            "opening an app is no card's verb"
        );
    }

    #[test]
    fn app_opener_waits_for_pointer_release_and_cancelled_drag_does_not_open() {
        let mut pad = CollaborationLaunchpad::new();
        pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        pad.card_rects = pad.cards();
        let button = pad.app_button_rect().center();
        let mut doc = AutoCommit::new();
        deliver(&mut pad, &mut doc, &pointer_at(button));
        assert!(deliver(&mut pad, &mut doc, &mouse_press()));
        assert!(
            pad.take_press().is_none(),
            "a pointer press must not open Search before the person completes their click"
        );
        deliver(&mut pad, &mut doc, &pointer_at(Point::new(0.0, 0.0)));
        assert!(deliver(&mut pad, &mut doc, &mouse_release()));
        assert!(
            pad.take_press().is_none(),
            "dragging off the app opener cancels activation"
        );
        deliver(&mut pad, &mut doc, &pointer_at(button));
        deliver(&mut pad, &mut doc, &mouse_press());
        assert!(deliver(&mut pad, &mut doc, &mouse_release()));
        assert_eq!(pad.take_press(), Some(LaunchpadPress::OpenApp));
        assert!(
            pad.take_press().is_none(),
            "a chosen card happens once, not every pass"
        );
    }

    #[test]
    fn interrupted_app_opener_gestures_consume_release_and_can_be_retried() {
        for interruption in [named_key(NamedKey::Escape), WindowEvent::Focused(false)] {
            let mut pad = CollaborationLaunchpad::new();
            pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            pad.card_rects = pad.cards();
            pad.set_focus(true);
            let mut doc = AutoCommit::new();
            let button = pad.app_button_rect().center();
            deliver(&mut pad, &mut doc, &pointer_at(button));
            deliver(&mut pad, &mut doc, &mouse_press());
            assert!(pad.wants_pointer_capture());
            deliver(&mut pad, &mut doc, &interruption);
            assert!(deliver(&mut pad, &mut doc, &mouse_release()));
            assert!(!pad.wants_pointer_capture());
            assert!(pad.take_press().is_none());
            deliver(&mut pad, &mut doc, &mouse_press());
            assert!(deliver(&mut pad, &mut doc, &mouse_release()));
            assert_eq!(pad.take_press(), Some(LaunchpadPress::OpenApp));
        }
    }

    #[test]
    fn moving_the_launchpad_cancels_the_old_press_before_new_geometry_is_clicked() {
        let mut pad = CollaborationLaunchpad::new();
        pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        pad.card_rects = pad.cards();
        let mut doc = AutoCommit::new();
        let button = pad.app_button_rect().center();
        deliver(&mut pad, &mut doc, &pointer_at(button));
        deliver(&mut pad, &mut doc, &mouse_press());
        pad.set_presentation_area(Some(Rect::new(0.0, 0.0, 1000.0, 700.0)));
        let moved_button = pad.app_button_rect().center();
        deliver(&mut pad, &mut doc, &pointer_at(moved_button));
        assert!(deliver(&mut pad, &mut doc, &mouse_release()));
        assert!(pad.take_press().is_none());
        deliver(&mut pad, &mut doc, &mouse_press());
        assert!(deliver(&mut pad, &mut doc, &mouse_release()));
        assert_eq!(pad.take_press(), Some(LaunchpadPress::OpenApp));
    }

    #[test]
    fn app_opener_keyboard_repeat_is_consumed_without_a_second_request() {
        let mut pad = CollaborationLaunchpad::new();
        pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        pad.card_rects = pad.cards();
        pad.focus_synthetic_accessibility(&[6]);
        let mut doc = AutoCommit::new();
        assert!(deliver(&mut pad, &mut doc, &named_key(NamedKey::Enter)));
        assert_eq!(pad.take_press(), Some(LaunchpadPress::OpenApp));
        let mut repeated = named_key(NamedKey::Enter);
        assert!(matches!(repeated, WindowEvent::KeyboardInput { .. }));
        if let WindowEvent::KeyboardInput { event, .. } = &mut repeated {
            event.repeat = true;
        }
        assert!(deliver(&mut pad, &mut doc, &repeated));
        assert!(pad.take_press().is_none());
        assert!(deliver(&mut pad, &mut doc, &named_key(NamedKey::Space)));
        assert_eq!(pad.take_press(), Some(LaunchpadPress::OpenApp));
    }

    fn pointer_at(point: Point) -> WindowEvent {
        ui::synthetic_input::cursor_moved(point.x, point.y)
    }

    use ui::synthetic_input::mouse_press;

    use ui::synthetic_input::named_key;

    /// The tiles are actions: a secondary action's face, the raised
    /// level with no edge that lifts under the pointer, with neutral
    /// icons, never the accent at rest.
    #[test]
    fn launchpad_tiles_are_raised_actions_with_neutral_icons() {
        for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
            let face = |hovered| {
                ui::widget::face_colors(
                    &theme,
                    ui::widget::ButtonRole::Secondary,
                    ui::widget::Interaction {
                        hovered,
                        ..ui::widget::Interaction::default()
                    },
                )
            };
            let (rest, edge, _) = face(false);
            assert_eq!(rest, theme.tokens.colors.surfaces.raised, "{}", theme.name);
            assert!(edge.is_none(), "{}: a raised tile has no edge", theme.name);
            assert_ne!(face(true).0, rest, "{}: a hovered tile lifts", theme.name);
            // Icons are the text inks, never the accent. High contrast's
            // accent is inverted, its primary ink itself, so there the
            // colours cannot tell the two apart; elsewhere they must differ.
            let intent = &theme.tokens.colors.intent;
            let neutral = [theme.text_dim, theme.text_primary];
            for active in [false, true] {
                let ink = icon_ink(&theme, active);
                for accent in [intent.accent, intent.accent_ink] {
                    assert!(
                        neutral.contains(&accent) || ink != accent,
                        "{}: icons are neutral ink",
                        theme.name
                    );
                }
            }
            assert_eq!(icon_ink(&theme, false), theme.text_dim);
            assert_eq!(icon_ink(&theme, true), theme.text_primary);
        }
    }

    #[test]
    fn launchpad_layout_keeps_edges_bounds_and_clears_chrome_at_every_size() {
        fn compact_large_text_heading_stays_below_top_chrome() {
            let mut pad = CollaborationLaunchpad::new();
            pad.rect = Rect::new(0.0, 48.0, 800.0, 700.0);
            let theme = Theme::dark().with_text_scale(2.0);
            let mut text = TextRenderer::new();
            text.set_text_scale(2.0);
            pad.draw(
                &mut Scene::new(),
                Affine::IDENTITY,
                &mut text,
                &theme,
                &PresenceCtx::default(),
            );
            assert!(
                pad.card_rects[0].y0 - 90.0 >= pad.rect.y0 + 12.0,
                "heading must clear chrome: {:?}",
                pad.card_rects[0]
            );
        }

        fn desktop_app_button_has_visible_bounds_in_compact_layouts() {
            for (width, height) in [(1280.0, 752.0), (960.0, 492.0), (460.0, 712.0)] {
                let mut pad = CollaborationLaunchpad::new();
                pad.rect = Rect::new(0.0, 48.0, width, height + 48.0);
                for scale in [1.0, 1.5, 2.0] {
                    let theme = Theme::dark().with_text_scale(scale);
                    let mut text = TextRenderer::new();
                    text.set_text_scale(scale);
                    pad.draw(
                        &mut Scene::new(),
                        Affine::IDENTITY,
                        &mut text,
                        &theme,
                        &PresenceCtx::default(),
                    );
                    let button = pad.app_button_rect();
                    let (label_w, label_h) = text.measure(
                        "Open an app",
                        theme.tokens.typography.size(TextRole::Body) / scale,
                        None,
                    );
                    assert!(
                        f64::from(label_w) + 32.0 <= button.width()
                            && f64::from(label_h) + 16.0 <= button.height()
                    );
                    assert!(button.width() >= 24.0 && button.height() >= 24.0);
                    assert!(
                        pad.rect.contains(button.origin())
                            && button.x1 <= pad.rect.x1
                            && button.y1 <= pad.rect.y1,
                        "{button:?} outside {:?}",
                        pad.rect
                    );
                }
            }
        }

        fn wide_and_narrow_layouts_keep_all_cards_actionable() {
            for rect in [
                Rect::new(0.0, 0.0, 1200.0, 720.0),
                Rect::new(0.0, 0.0, 460.0, 760.0),
            ] {
                let (cards, _) = CollaborationLaunchpad::card_geometry(rect);
                assert!(
                    cards
                        .iter()
                        .all(|card| card.width() >= 280.0 && card.height() >= 64.0)
                );
                assert!(cards.windows(2).all(|pair| pair[0] != pair[1]));
            }
        }

        compact_large_text_heading_stays_below_top_chrome();
        desktop_app_button_has_visible_bounds_in_compact_layouts();
        wide_and_narrow_layouts_keep_all_cards_actionable();
    }

    #[test]
    fn a_canvas_across_two_displays_centres_on_the_presenting_one() {
        let mut pad = CollaborationLaunchpad::new();
        pad.rect = Rect::new(0.0, 48.0, 2048.0, 768.0);
        let right = Rect::new(1024.0, 0.0, 2048.0, 768.0);
        pad.set_presentation_area(Some(right));
        let cards = pad.cards();
        let x0 = cards.iter().map(|c| c.x0).fold(f64::INFINITY, f64::min);
        let x1 = cards.iter().map(|c| c.x1).fold(f64::NEG_INFINITY, f64::max);
        assert!(
            x0 >= right.x0 && x1 <= right.x1,
            "no card may straddle the seam: {cards:?}"
        );
        assert!(
            (f64::midpoint(x0, x1) - right.center().x).abs() < 1.0,
            "the grid centres on its display: {x0}..{x1}"
        );
        let left = Rect::new(0.0, 0.0, 1024.0, 768.0);
        pad.set_presentation_area(Some(left));
        assert!(
            pad.cards()
                .iter()
                .all(|c| c.x0 >= left.x0 && c.x1 <= left.x1),
            "moving to the other display moves the grid there: {:?}",
            pad.cards()
        );
    }

    #[test]
    fn a_persons_card_asks_chat_for_the_conversation_between_them() {
        let root = "a6d40c19e7b25f83c1d9e0a4b7f26c58d3e1a9b0c7f4e2d6a8b5c3e1f0d9a7c2";
        let ask = LaunchpadAppAction::message_person(root);
        assert_eq!(ask.app_id, "so.tilo.chat");
        assert_eq!(ask.display_name, "Chat");
        assert_eq!(ask.action, "open-direct-conversation");
        assert_eq!(
            ask.resource,
            format!("chat.conversation:{root}"),
            "Chat publishes the action on its conversation with them"
        );
    }

    #[test]
    fn launchpad_cards_route_to_their_owning_apps_by_click_or_assistive_action() {
        fn desktop_launchpad_exposes_an_app_opener() {
            let pad = CollaborationLaunchpad::new();
            assert!(
                pad.accessibility()
                    .unwrap()
                    .children
                    .iter()
                    .any(|node| node.name.as_deref() == Some("Open an app"))
            );
        }

        fn every_card_has_a_unique_semantic_route() {
            let routes = LaunchpadAction::ALL.map(LaunchpadAction::target);
            let unique = routes.iter().collect::<std::collections::HashSet<_>>();
            assert_eq!(unique.len(), LaunchpadAction::ALL.len());
            assert!(routes.iter().all(|route| route.starts_with("launchpad:")));
        }

        fn collaboration_cards_target_the_apps_that_own_those_features() {
            assert_eq!(
                LaunchpadAction::Chat.app_target(),
                Some(("so.tilo.chat", "Chat"))
            );
            assert_eq!(
                LaunchpadAction::Meet.app_target(),
                Some(("so.tilo.chat", "Chat")),
                "calls are a Chat feature, not a fabricated Meet package"
            );
            assert_eq!(
                LaunchpadAction::Tasks.app_target(),
                Some(("so.tilo.calendar", "Calendar")),
                "Calendar is the repo's existing task manager"
            );
        }

        fn tasks_and_call_cards_name_the_action_they_perform() {
            let tasks = LaunchpadAction::Tasks.app_action().expect("tasks action");
            assert_eq!(
                (
                    tasks.app_id.as_str(),
                    tasks.resource.as_str(),
                    tasks.action.as_str()
                ),
                ("so.tilo.calendar", "calendar.calendar", "show-tasks")
            );
            let call = LaunchpadAction::Meet.app_action().expect("call action");
            assert_eq!(
                (
                    call.app_id.as_str(),
                    call.resource.as_str(),
                    call.action.as_str()
                ),
                ("so.tilo.chat", "chat.workspace", "start-call")
            );
            for card in LaunchpadAction::ALL {
                if let Some(action) = card.app_action() {
                    assert_eq!(
                        card.app_target(),
                        Some((action.app_id.as_str(), action.display_name.as_str())),
                        "{card:?} acts in the app it opens"
                    );
                }
            }
        }

        fn assistive_activation_takes_the_same_choice_as_a_card_click() {
            let mut launchpad = CollaborationLaunchpad::new();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            assert!(launchpad.perform_semantic_action(
                &LaunchpadAction::Activity.target(),
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
            assert_eq!(
                launchpad.take_press(),
                Some(LaunchpadPress::Card(LaunchpadAction::Activity))
            );
        }

        desktop_launchpad_exposes_an_app_opener();
        every_card_has_a_unique_semantic_route();
        collaboration_cards_target_the_apps_that_own_those_features();
        tasks_and_call_cards_name_the_action_they_perform();
        assistive_activation_takes_the_same_choice_as_a_card_click();
    }

    #[test]
    fn a_card_publishes_its_own_action_by_click_or_keyboard() {
        fn a_clicked_card_publishes_its_own_action_not_the_launcher() {
            let mut pad = CollaborationLaunchpad::new();
            pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            pad.card_rects = pad.cards();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let notes = pad.card_rects[1].center();
            let mut handled = false;
            for event in [pointer_at(notes), mouse_press(), mouse_release()] {
                handled = deliver(&mut pad, &mut doc, &event);
            }
            assert!(handled);
            assert_eq!(
                pad.take_press(),
                Some(LaunchpadPress::Card(LaunchpadAction::Notes))
            );
            let _ = &root;
        }

        fn arrow_keys_move_and_enter_opens_the_selected_card() {
            let mut pad = CollaborationLaunchpad::new();
            pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            pad.card_rects = pad.cards();
            pad.set_focus(true);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            for key in [NamedKey::ArrowRight, NamedKey::Enter] {
                assert!(deliver(&mut pad, &mut doc, &named_key(key)));
            }
            assert_eq!(
                pad.take_press(),
                Some(LaunchpadPress::Card(LaunchpadAction::Notes))
            );
            let _ = &root;
        }

        a_clicked_card_publishes_its_own_action_not_the_launcher();
        arrow_keys_move_and_enter_opens_the_selected_card();
    }
}
