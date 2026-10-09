//! First-use workspace launchpad.
//!
//! An empty collaborative desktop should explain the useful things people can
//! do together, with a direct route to open any available app. Every card
//! publishes a semantic action into the widget document; the host takes it
//! with [`LaunchpadAction::take`] (the "Open an app" button with
//! [`take_open_app`]) and routes it through the same app, invite, Activity and
//! launcher paths used everywhere else.

use std::collections::HashMap;
use ui::widget::ModelPath;

use automerge::transaction::Transactable as _;
use automerge::{ObjId, ReadDoc as _, ScalarValue, Value};
use taffy::style_helpers::percent;
use taffy::{NodeId, Style, TaffyTree};
use ui::a11y::{AccessNode, AccessState, Action, Role};
use ui::color_compat::ColorCompat;
use ui::icons::Icon;
use ui::presence::PresenceCtx;
use ui::text_render::TextRenderer;
use ui::theme::Theme;
use ui::widget::{Button, ButtonFace, TaffyTreeExt, Widget};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect, Stroke};
use vello::peniko::Fill;
use winit::event::{ElementState, WindowEvent};
use winit::keyboard::{Key, NamedKey};

use crate::i18n::t;
use ui::TextRole;

pub const EMPTY_WORKSPACE_OPEN_APP: &str = "shell.empty-workspace.open-app";

/// Whether "Open an app" was chosen, cleared from `doc` so it fires once.
pub fn take_open_app(doc: &mut ui::widget::Edits<'_>, root: &ObjId) -> bool {
    consume_launchpad(doc, root, LaunchpadChoice::OpenApp)
}

pub const EMPTY_WORKSPACE_CHAT: &str = "shell.empty-workspace.chat";
pub const EMPTY_WORKSPACE_NOTES: &str = "shell.empty-workspace.notes";
pub const EMPTY_WORKSPACE_TASKS: &str = "shell.empty-workspace.tasks";
pub const EMPTY_WORKSPACE_MEET: &str = "shell.empty-workspace.meet";
pub const EMPTY_WORKSPACE_INVITE: &str = "shell.empty-workspace.invite";
pub const EMPTY_WORKSPACE_ACTIVITY: &str = "shell.empty-workspace.activity";

/// An action an app publishes, as a launchpad card names it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct LaunchpadAppAction {
    pub app_id: &'static str,
    pub display_name: &'static str,
    pub resource: &'static str,
    pub action: &'static str,
}

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

    pub const fn path(self) -> &'static str {
        match self {
            Self::Chat => EMPTY_WORKSPACE_CHAT,
            Self::Notes => EMPTY_WORKSPACE_NOTES,
            Self::Tasks => EMPTY_WORKSPACE_TASKS,
            Self::Meet => EMPTY_WORKSPACE_MEET,
            Self::Invite => EMPTY_WORKSPACE_INVITE,
            Self::Activity => EMPTY_WORKSPACE_ACTIVITY,
        }
    }

    pub const fn app_target(self) -> Option<(&'static str, &'static str)> {
        match self {
            Self::Chat | Self::Meet => Some(("so.tilo.chat", "Chat")),
            Self::Notes => Some(("so.tilo.notes", "Notes")),
            Self::Tasks => Some(("so.tilo.calendar", "Calendar")),
            Self::Invite | Self::Activity => None,
        }
    }

    /// The typed action a card invokes in its app: the app, the resource it
    /// publishes the action on, and the action. The host delivers it to the
    /// app in the workspace, launching the app first when it is closed, so
    /// the card does what it names in one step.
    pub const fn app_action(self) -> Option<LaunchpadAppAction> {
        match self {
            Self::Meet => Some(LaunchpadAppAction {
                app_id: "so.tilo.chat",
                display_name: "Chat",
                resource: "chat.workspace",
                action: "start-call",
            }),
            Self::Chat | Self::Notes | Self::Tasks | Self::Invite | Self::Activity => None,
        }
    }

    /// The card the person chose, cleared from `doc` so it fires once.
    pub fn take(doc: &mut ui::widget::Edits<'_>, root: &ObjId) -> Option<Self> {
        let action = Self::ALL.into_iter().find(|action| {
            matches!(
                doc.get(root, action.path()),
                Ok(Some((Value::Scalar(value), _)))
                    if matches!(value.as_ref(), ScalarValue::Boolean(true))
            )
        })?;
        consume_launchpad(doc, root, LaunchpadChoice::Card(action)).then_some(action)
    }

    /// A stable, untranslated name for the card.
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

    fn detail(self) -> String {
        match self {
            Self::Chat => t("launchpad-discuss-detail"),
            Self::Notes => t("launchpad-notes-detail"),
            Self::Tasks => t("launchpad-tasks-detail"),
            Self::Meet => t("launchpad-meet-detail"),
            Self::Invite => t("launchpad-invite-detail"),
            Self::Activity => t("launchpad-activity-detail"),
        }
    }

    const fn icon(self) -> Icon {
        match self {
            Self::Chat => Icon::Mail,
            Self::Notes => Icon::FileText,
            Self::Tasks => Icon::Check,
            Self::Meet => Icon::Camera,
            Self::Invite => Icon::Users,
            Self::Activity => Icon::History,
        }
    }
}

const LAUNCHPAD_ACTION_TYPE: &str = "tilo.desktop.launchpad";

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
enum LaunchpadChoice {
    OpenApp,
    Card(LaunchpadAction),
}

impl LaunchpadChoice {
    fn path(&self) -> &'static str {
        match self {
            Self::OpenApp => EMPTY_WORKSPACE_OPEN_APP,
            Self::Card(action) => action.path(),
        }
    }
}

/// A native launchpad request the desktop consumes through its public route.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ActivateLaunchpad {
    root: String,
    choice: LaunchpadChoice,
}

impl tilo_actions::Action for ActivateLaunchpad {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        let root = doc.import_obj(&self.root)?;
        doc.put(&root, self.choice.path(), true)?;
        Ok(())
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

/// A host acknowledges exactly one pending launchpad request.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct ConsumeLaunchpad {
    root: String,
    choice: LaunchpadChoice,
}

impl tilo_actions::Action for ConsumeLaunchpad {
    fn apply(&self, doc: &mut tilo_actions::Doc) -> Result<(), tilo_actions::ActionError> {
        let root = doc.import_obj(&self.root)?;
        if !matches!(
            doc.get(&root, self.choice.path())?,
            Some((Value::Scalar(value), _))
                if matches!(value.as_ref(), ScalarValue::Boolean(true))
        ) {
            return Err(tilo_actions::ActionError::Refused(
                "the launchpad request is no longer pending".into(),
            ));
        }
        doc.put(&root, self.choice.path(), false)?;
        Ok(())
    }

    fn locus(&self) -> tilo_actions::ExecutionLocus {
        tilo_actions::ExecutionLocus::ClientLocal
    }
}

fn consume_launchpad(
    edits: &mut ui::widget::Edits<'_>,
    root: &ObjId,
    choice: LaunchpadChoice,
) -> bool {
    if !matches!(
        edits.doc().get(root, choice.path()),
        Ok(Some((Value::Scalar(value), _)))
            if matches!(value.as_ref(), ScalarValue::Boolean(true))
    ) {
        return false;
    }
    let action = ConsumeLaunchpad {
        root: root.to_string(),
        choice,
    };
    let context =
        tilo_actions::ActionContext::for_action("tilo.desktop", action.choice.path(), &action);
    match edits.apply(
        &mut ui::action_host::ActionHost::new(),
        "tilo.desktop.launchpad.consume",
        &action,
        context,
    ) {
        Ok(()) => true,
        Err(error) => {
            tracing::warn!(%error, "launchpad request acknowledgement refused");
            false
        }
    }
}

pub struct CollaborationLaunchpad {
    /// Horizontal room the host's floating workspace rail takes over a
    /// canvas of the given height.
    rail_clearance: fn(f64) -> f64,
    rect: Rect,
    /// The display the launchpad presents on when the canvas spans several,
    /// and whether the rail floats over that display.
    presentation: Option<(Rect, bool)>,
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
    actions: ui::action_host::ActionHost,
    targets: ui::widget::PaintedTargets,
    target_geometry: Option<([Rect; 6], Rect)>,
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

impl CollaborationLaunchpad {
    /// A launchpad centred beside a rail that takes `rail_clearance` of a
    /// canvas that tall.
    pub fn new(rail_clearance: fn(f64) -> f64) -> Self {
        Self {
            rail_clearance,
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
            actions: ui::action_host::ActionHost::new(),
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

    fn title(&self) -> String {
        self.copy
            .title
            .clone()
            .unwrap_or_else(|| t("launchpad-title"))
    }

    fn subtitle(&self) -> String {
        self.copy
            .subtitle
            .clone()
            .unwrap_or_else(|| t("launchpad-subtitle"))
    }

    fn keyboard_hint(&self) -> String {
        self.copy
            .keyboard_hint
            .clone()
            .unwrap_or_else(|| t("launchpad-keyboard-hint"))
    }

    /// Centre on `display` (with the rail's clearance when `beside_rail`)
    /// instead of the whole canvas, as on a desktop spanning several
    /// displays. `None` returns to the whole canvas.
    pub fn set_presentation_area(&mut self, display: Option<Rect>, beside_rail: bool) {
        let presentation = display.map(|rect| (rect, beside_rail));
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
            Some((display, _)) => {
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
        let no_rail: fn(f64) -> f64 = |_| 0.0;
        let clearance = match self.presentation {
            Some((_, false)) => no_rail,
            _ => self.rail_clearance,
        };
        Self::card_geometry(self.presentation_rect(), clearance)
    }

    /// The cards and the "Open an app" button by stable name
    /// (`launchpad:notes`, `launchpad:open-app`, ...), as laid out in the
    /// last frame, so a driver can press them like a person would.
    pub fn targets(&self) -> Vec<(String, Rect)> {
        LaunchpadAction::ALL
            .iter()
            .zip(self.card_rects)
            .map(|(action, rect)| (format!("launchpad:{}", action.slug()), rect))
            .chain(std::iter::once((
                "launchpad:open-app".to_owned(),
                self.app_button_rect(),
            )))
            .collect()
    }

    fn activate(
        &mut self,
        choice: LaunchpadChoice,
        state: &mut ui::widget::Edits<'_>,
        path: &ObjId,
    ) -> bool {
        let action = ActivateLaunchpad {
            root: path.to_string(),
            choice,
        };
        let context =
            tilo_actions::ActionContext::for_action("tilo.desktop", action.choice.path(), &action);
        match state.apply(&mut self.actions, LAUNCHPAD_ACTION_TYPE, &action, context) {
            Ok(()) => true,
            Err(error) => {
                tracing::warn!(%error, "launchpad action refused");
                false
            }
        }
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
    /// opposed to its layout rect, which spans the whole canvas under the
    /// floating workspace rail. The access tree reports this, so a geometry
    /// probe does not read the rail's controls as overlapping the launchpad.
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
    fn card_geometry(rect: Rect, rail_clearance: fn(f64) -> f64) -> ([Rect; 6], usize) {
        // The workspace rail floats over the launchpad rather than shrinking
        // its widget's layout rect. On desktop-sized outputs, centre the
        // launchpad in the remaining canvas so its first heading/card never
        // starts under that rail. Narrow layouts keep the full canvas: the
        // rail is then already collapsed by the surrounding chrome.
        let rail_clearance = rail_clearance(rect.height());
        let content_x0 = if rect.width() >= 720.0 {
            rect.x0 + rail_clearance
        } else {
            rect.x0
        };
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
                self.targets.target(action.path(), rect, action.label());
            }
            self.targets.target(
                EMPTY_WORKSPACE_OPEN_APP,
                geometry.1,
                t("launchpad-open-app"),
            );
            self.target_geometry = Some(geometry);
        }
        let id = LaunchpadAction::ALL
            .get(self.selected)
            .map_or(EMPTY_WORKSPACE_OPEN_APP, |action| action.path());
        self.targets.focus(id);
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
            size: taffy::Size {
                width: percent(1.0_f32),
                height: percent(1.0_f32),
            },
            min_size: taffy::Size {
                width: percent(0.0_f32),
                height: percent(0.0_f32),
            },
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
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let old_rect = self.rect;
        self.rect = ui::widget::placed(&layout, origin);
        if self.rect != old_rect {
            self.targets.cancel_pointer_press();
        }
        let cards = self.cards();
        if cards != self.card_rects {
            self.card_rects = cards;
            self.dirty = true;
        }
    }

    fn needs_redraw(&self) -> bool {
        self.dirty
    }

    fn mark_drawn(&mut self) {
        self.dirty = false;
    }

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
            .position(|action| self.targets.interaction(action.path()).hovered);
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

        // Control role (HIG §9.1/§9.3): an action tile takes `radii.control`. The
        // hovered or keyboard-selected tile is a lift with the tier's
        // hairline — never an accent wash, which is reserved for live state
        // (§9.4) — and an idle tile rests on its translucent floor with an
        // edge derived from that floor (§9.5), so the six cards read as cards
        // on the dark grid as well as the light one.
        let floor = theme.tokens.colors.surfaces.raised.with_alpha_factor(0.78);
        let idle_edge = ui::surface::edge_on(theme.tokens.colors.surfaces.raised);
        for (index, (action, rect)) in LaunchpadAction::ALL.into_iter().zip(cards).enumerate() {
            let active = hovered == Some(index) || (self.focused && self.selected == index);
            let tier = ui::chrome::selection_tier(
                theme,
                ui::chrome::SelectionState::new(
                    active,
                    hovered == Some(index),
                    self.targets.interaction(action.path()).pressed,
                ),
            );
            let surface = if active { tier.fill } else { floor };
            let shape = RoundedRect::from_rect(
                rect,
                ui::chrome::ChromeRole::Control.radius(theme, rect.height()),
            );
            scene.fill(Fill::NonZero, transform, surface, None, &shape);
            let focused = self.focused && self.selected == index;
            scene.stroke(
                &Stroke::new(if focused { 2.0 } else { 1.0 }),
                transform,
                if focused {
                    theme.border_focus
                } else {
                    tier.border.unwrap_or(idle_edge)
                },
                None,
                &shape,
            );
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
                theme.tokens.colors.intent.inverted,
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
            let (_, detail_h) = text.measure(&action.detail(), caption, None);
            let detail_y = (16.0 + f64::from(label_h) + 6.0).max(47.0);
            if detail_y + f64::from(detail_h) + 6.0 <= rect.height() {
                let detail =
                    text.elide_to_width(&action.detail(), caption, (rect.width() - 72.0) as f32);
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
                .hovered(self.targets.interaction(EMPTY_WORKSPACE_OPEN_APP).hovered)
                .pressed(self.targets.interaction(EMPTY_WORKSPACE_OPEN_APP).pressed),
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
        if *ctx.handled {
            return;
        }
        let before = (self.hover_target(), self.selected, self.focused);
        self.sync_targets();
        if let WindowEvent::CursorMoved { position, .. } = event {
            self.last_cursor =
                Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
        }
        if !self.navigate(event, ctx) {
            self.dirty |= self.targets.handle_event(event, ctx);
            if let Some(id) = self.targets.focused() {
                self.selected = LaunchpadAction::ALL
                    .iter()
                    .position(|action| action.path() == id)
                    .unwrap_or(6);
            }
            if matches!(event, WindowEvent::Focused(false)) {
                self.focused = false;
            }
            for id in self.targets.take_activated() {
                let choice = LaunchpadAction::ALL
                    .into_iter()
                    .find(|action| action.path() == id)
                    .map_or(LaunchpadChoice::OpenApp, LaunchpadChoice::Card);
                self.activate(choice, &mut ctx.state, ctx.path);
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
                                .with_description(action.detail())
                                .with_action_target(action.path())
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.focused && self.selected == index,
                                    ..Default::default()
                                })
                        })
                        .chain(std::iter::once(
                            AccessNode::new(Role::Button)
                                .with_name(t("launchpad-open-app"))
                                .with_action_target(EMPTY_WORKSPACE_OPEN_APP)
                                .with_bounds(self.app_button_rect())
                                .with_state(AccessState {
                                    focusable: true,
                                    focused: self.focused && self.selected == 6,
                                    ..Default::default()
                                }),
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
        state: &mut ui::widget::Edits<'_>,
        path: &ModelPath,
    ) -> bool {
        let (state, path) = ui::doc::document(state, path);
        if target == EMPTY_WORKSPACE_OPEN_APP && matches!(action, Action::Activate) {
            return self.activate(LaunchpadChoice::OpenApp, state, path);
        }
        let Some(item) = LaunchpadAction::ALL
            .into_iter()
            .find(|item| item.path() == target)
        else {
            return false;
        };
        if !matches!(action, Action::Activate) {
            return false;
        }
        self.activate(LaunchpadChoice::Card(item), state, path)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use automerge::AutoCommit;
    use winit::event::MouseButton;

    fn deliver(
        pad: &mut CollaborationLaunchpad,
        doc: &mut AutoCommit,
        event: &WindowEvent,
    ) -> bool {
        let mut taffy = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        let mut focus_target = None;
        pad.handle_event(
            event,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus_target,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    fn left_release() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Released,
            button: MouseButton::Left,
        }
    }

    #[test]
    fn app_opener_waits_for_pointer_release_and_cancelled_drag_does_not_open() {
        let mut pad = CollaborationLaunchpad::new(test_rail);
        pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        pad.card_rects = pad.cards();
        let button = pad.app_button_rect().center();
        let mut doc = AutoCommit::new();
        deliver(&mut pad, &mut doc, &pointer_at(button));
        assert!(deliver(&mut pad, &mut doc, &left_press()));
        assert!(
            !take_open_app(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT),
            "a pointer press must not open Search before the person completes their click"
        );
        deliver(&mut pad, &mut doc, &pointer_at(Point::new(0.0, 0.0)));
        assert!(deliver(&mut pad, &mut doc, &left_release()));
        assert!(
            !take_open_app(&mut ui::widget::Edits::lend(&mut doc), &automerge::ROOT),
            "dragging off the app opener cancels activation"
        );
        deliver(&mut pad, &mut doc, &pointer_at(button));
        deliver(&mut pad, &mut doc, &left_press());
        assert!(deliver(&mut pad, &mut doc, &left_release()));
        assert!(take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(!take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
    }

    #[test]
    fn interrupted_app_opener_gestures_consume_release_and_can_be_retried() {
        for interruption in [named_key(NamedKey::Escape), WindowEvent::Focused(false)] {
            let mut pad = CollaborationLaunchpad::new(test_rail);
            pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            pad.card_rects = pad.cards();
            pad.set_focus(true);
            let mut doc = AutoCommit::new();
            let button = pad.app_button_rect().center();
            deliver(&mut pad, &mut doc, &pointer_at(button));
            deliver(&mut pad, &mut doc, &left_press());
            assert!(pad.wants_pointer_capture());
            deliver(&mut pad, &mut doc, &interruption);
            assert!(deliver(&mut pad, &mut doc, &left_release()));
            assert!(!pad.wants_pointer_capture());
            assert!(!take_open_app(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
            deliver(&mut pad, &mut doc, &left_press());
            assert!(deliver(&mut pad, &mut doc, &left_release()));
            assert!(take_open_app(
                &mut ui::widget::Edits::lend(&mut doc),
                &automerge::ROOT
            ));
        }
    }

    #[test]
    fn moving_the_launchpad_cancels_the_old_press_before_new_geometry_is_clicked() {
        let mut pad = CollaborationLaunchpad::new(test_rail);
        pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        pad.card_rects = pad.cards();
        let mut doc = AutoCommit::new();
        let button = pad.app_button_rect().center();
        deliver(&mut pad, &mut doc, &pointer_at(button));
        deliver(&mut pad, &mut doc, &left_press());
        pad.set_presentation_area(Some(Rect::new(0.0, 0.0, 1000.0, 700.0)), false);
        let moved_button = pad.app_button_rect().center();
        deliver(&mut pad, &mut doc, &pointer_at(moved_button));
        assert!(deliver(&mut pad, &mut doc, &left_release()));
        assert!(!take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        deliver(&mut pad, &mut doc, &left_press());
        assert!(deliver(&mut pad, &mut doc, &left_release()));
        assert!(take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
    }

    #[test]
    fn app_opener_keyboard_repeat_is_consumed_without_a_second_request() {
        let mut pad = CollaborationLaunchpad::new(test_rail);
        pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
        pad.card_rects = pad.cards();
        pad.focus_synthetic_accessibility(&[6]);
        let mut doc = AutoCommit::new();
        assert!(deliver(&mut pad, &mut doc, &named_key(NamedKey::Enter)));
        assert!(take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        let mut repeated = named_key(NamedKey::Enter);
        assert!(matches!(repeated, WindowEvent::KeyboardInput { .. }));
        if let WindowEvent::KeyboardInput { event, .. } = &mut repeated {
            event.repeat = true;
        }
        assert!(deliver(&mut pad, &mut doc, &repeated));
        assert!(!take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
        assert!(deliver(&mut pad, &mut doc, &named_key(NamedKey::Space)));
        assert!(take_open_app(
            &mut ui::widget::Edits::lend(&mut doc),
            &automerge::ROOT
        ));
    }

    #[test]
    fn pending_requests_are_consumed_once_and_keep_false_witnesses() {
        let mut pad = CollaborationLaunchpad::new(test_rail);
        let mut doc = AutoCommit::new();
        let nested = doc
            .put_object(automerge::ROOT, "launchpad", automerge::ObjType::Map)
            .unwrap();
        for root in [automerge::ROOT, nested] {
            let choices = std::iter::once(LaunchpadChoice::OpenApp)
                .chain(LaunchpadAction::ALL.into_iter().map(LaunchpadChoice::Card));
            for choice in choices {
                assert!(pad.perform_semantic_action(
                    choice.path(),
                    &Action::Activate,
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root,
                ));
                match choice {
                    LaunchpadChoice::OpenApp => {
                        assert!(take_open_app(&mut ui::widget::Edits::lend(&mut doc), &root));
                    }
                    LaunchpadChoice::Card(action) => assert_eq!(
                        LaunchpadAction::take(&mut ui::widget::Edits::lend(&mut doc), &root),
                        Some(action)
                    ),
                }
                assert!(
                    matches!(
                        doc.get(&root, choice.path()).unwrap(),
                        Some((Value::Scalar(value), _)) if matches!(value.as_ref(), ScalarValue::Boolean(false))
                    ),
                    "consuming a request preserves its false witness"
                );
                let before = (doc.base_heads(), doc.pending_ops());
                assert!(!take_open_app(
                    &mut ui::widget::Edits::lend(&mut doc),
                    &root
                ));
                assert_eq!(
                    LaunchpadAction::take(&mut ui::widget::Edits::lend(&mut doc), &root),
                    None
                );
                assert_eq!(
                    (doc.base_heads(), doc.pending_ops()),
                    before,
                    "polling a consumed request writes nothing"
                );
            }
        }
    }

    /// A host rail as wide as the shell's on a laptop display.
    fn test_rail(_height: f64) -> f64 {
        140.0
    }

    fn pointer_at(point: Point) -> WindowEvent {
        WindowEvent::CursorMoved {
            device_id: winit::event::DeviceId::dummy(),
            position: winit::dpi::PhysicalPosition::new(point.x, point.y),
        }
    }

    fn left_press() -> WindowEvent {
        WindowEvent::MouseInput {
            device_id: winit::event::DeviceId::dummy(),
            state: ElementState::Pressed,
            button: MouseButton::Left,
        }
    }

    use ui::synthetic_input::named_key;

    #[test]
    fn launchpad_layout_keeps_edges_bounds_and_clears_chrome_at_every_size() {
        /// HIG §9.5: an idle card's edge derives from its own floor and must
        /// register on every shipped palette — the former `border_standard` at
        /// 0.72 alpha was invisible on the dark grid.
        fn launchpad_cards_keep_an_edge_on_every_palette() {
            for theme in [Theme::dark(), Theme::light(), Theme::high_contrast()] {
                let floor = theme.tokens.colors.surfaces.raised;
                let delta = ui::surface::edge_delta(ui::surface::edge_on(floor), floor);
                assert!(
                    delta >= ui::tokens::colors::HAIRLINE_MIN_DELTA,
                    "{}: the card edge must separate it from its floor (Δ luminance {delta:.4})",
                    theme.name
                );
            }
        }

        fn compact_large_text_heading_stays_below_top_chrome() {
            let mut pad = CollaborationLaunchpad::new(test_rail);
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
                let mut pad = CollaborationLaunchpad::new(test_rail);
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
                let (cards, _) = CollaborationLaunchpad::card_geometry(rect, test_rail);
                assert!(
                    cards
                        .iter()
                        .all(|card| card.width() >= 280.0 && card.height() >= 64.0)
                );
                assert!(cards.windows(2).all(|pair| pair[0] != pair[1]));
            }
        }

        launchpad_cards_keep_an_edge_on_every_palette();
        compact_large_text_heading_stays_below_top_chrome();
        desktop_app_button_has_visible_bounds_in_compact_layouts();
        wide_and_narrow_layouts_keep_all_cards_actionable();
    }

    #[test]
    fn desktop_launchpad_clears_the_workspace_rail() {
        let rect = Rect::new(0.0, 0.0, 960.0, 540.0);
        let (cards, _) = CollaborationLaunchpad::card_geometry(rect, test_rail);
        let rail_clearance = test_rail(rect.height());
        assert!(
            cards.iter().all(|card| card.x0 >= rail_clearance),
            "desktop launchpad cards must not start beneath the persistent workspace rail: {cards:?}"
        );
    }

    #[test]
    fn a_canvas_across_two_displays_centres_on_the_presenting_one() {
        let mut pad = CollaborationLaunchpad::new(test_rail);
        pad.rect = Rect::new(0.0, 48.0, 2048.0, 768.0);
        let right = Rect::new(1024.0, 0.0, 2048.0, 768.0);
        pad.set_presentation_area(Some(right), false);
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
        pad.set_presentation_area(Some(left), true);
        assert!(
            pad.cards()
                .iter()
                .all(|c| c.x0 >= test_rail(720.0) && c.x1 <= left.x1),
            "the rail's display keeps its clearance: {:?}",
            pad.cards()
        );
    }

    #[test]
    fn launchpad_cards_route_to_their_owning_apps_by_click_or_assistive_action() {
        fn desktop_launchpad_exposes_an_app_opener() {
            let pad = CollaborationLaunchpad::new(test_rail);
            assert!(
                pad.accessibility()
                    .unwrap()
                    .children
                    .iter()
                    .any(|node| node.name.as_deref() == Some("Open an app"))
            );
        }

        fn every_card_has_a_unique_semantic_route() {
            let routes = LaunchpadAction::ALL.map(LaunchpadAction::path);
            let unique = routes.into_iter().collect::<std::collections::HashSet<_>>();
            assert_eq!(unique.len(), LaunchpadAction::ALL.len());
            assert!(
                routes
                    .iter()
                    .all(|route| route.starts_with("shell.empty-workspace."))
            );
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

        fn call_card_names_the_action_it_performs() {
            let call = LaunchpadAction::Meet.app_action().expect("call action");
            assert_eq!(
                (call.app_id, call.resource, call.action),
                ("so.tilo.chat", "chat.workspace", "start-call")
            );
            for card in LaunchpadAction::ALL {
                if let Some(action) = card.app_action() {
                    assert_eq!(
                        card.app_target(),
                        Some((action.app_id, action.display_name)),
                        "{card:?} acts in the app it opens"
                    );
                }
            }
        }

        fn assistive_activation_publishes_the_same_action_as_a_card_click() {
            let mut launchpad = CollaborationLaunchpad::new(test_rail);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;

            assert!(launchpad.perform_semantic_action(
                EMPTY_WORKSPACE_ACTIVITY,
                &Action::Activate,
                &mut ui::widget::Edits::lend(&mut doc),
                &root,
            ));
            assert!(matches!(
                doc.get(&root, EMPTY_WORKSPACE_ACTIVITY),
                Ok(Some((automerge::Value::Scalar(value), _)))
                    if matches!(value.as_ref(), automerge::ScalarValue::Boolean(true))
            ));
        }

        desktop_launchpad_exposes_an_app_opener();
        every_card_has_a_unique_semantic_route();
        collaboration_cards_target_the_apps_that_own_those_features();
        call_card_names_the_action_it_performs();
        assistive_activation_publishes_the_same_action_as_a_card_click();
    }

    #[test]
    fn a_card_publishes_its_own_action_by_click_or_keyboard() {
        fn a_clicked_card_publishes_its_own_action_not_the_launcher() {
            let mut pad = CollaborationLaunchpad::new(test_rail);
            pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            pad.card_rects = pad.cards();
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            let notes = pad.card_rects[1].center();
            let mut handled = false;
            for event in [pointer_at(notes), left_press(), left_release()] {
                handled = deliver(&mut pad, &mut doc, &event);
            }
            assert!(handled);
            assert_eq!(
                LaunchpadAction::take(&mut ui::widget::Edits::lend(&mut doc), &root),
                Some(LaunchpadAction::Notes)
            );
            assert!(
                !take_open_app(&mut ui::widget::Edits::lend(&mut doc), &root),
                "a card names its action; only the button opens the launcher"
            );
        }

        fn arrow_keys_move_and_enter_opens_the_selected_card() {
            let mut pad = CollaborationLaunchpad::new(test_rail);
            pad.rect = Rect::new(0.0, 0.0, 1280.0, 800.0);
            pad.card_rects = pad.cards();
            pad.set_focus(true);
            let mut doc = AutoCommit::new();
            let root = automerge::ROOT;
            for key in [NamedKey::ArrowRight, NamedKey::Enter] {
                assert!(deliver(&mut pad, &mut doc, &named_key(key)));
            }
            assert_eq!(
                LaunchpadAction::take(&mut ui::widget::Edits::lend(&mut doc), &root),
                Some(LaunchpadAction::Notes)
            );
        }

        a_clicked_card_publishes_its_own_action_not_the_launcher();
        arrow_keys_move_and_enter_opens_the_selected_card();
    }
}
