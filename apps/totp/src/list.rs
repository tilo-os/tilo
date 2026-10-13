//! The account list — the app's main screen.
//!
//! One [`ListRowFace`] per account: the account's avatar, its issuer and
//! account name, and, hovered, the row's actions. The rotating code and its
//! countdown ring sit in the row's trailing column, drawn from the live
//! wall clock every frame, so the list animates without any timer plumbing.
//! A shared account whose key is not on this device shows a "Set up" button
//! there instead of a code.
//!
//! Interaction is reported to the app as [`ListEvent`]s
//! ([`AccountList::take_events`]): clicking a row selects it, clicking its
//! code copies it, and the hover actions edit, reorder and delete it. With no
//! accounts the list shows an [`EmptyState`] whose action asks to add one.

use vello::kurbo::{Point, Rect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, WindowEvent};

use ui::chrome::{SelectionState, paint_row_selection};
use ui::icons::Icon;
use ui::paint::PaintCtx;
use ui::text_render::{TextKind, TextRenderer, TextStyle};
use ui::theme::Theme;
use ui::tokens::TextRole;
use ui::widget::{
    AvatarFace, Button, ButtonFace, ButtonRole, ControlSize, EmptyState, EmptyStateContent,
    EventCtx, Interaction, ListRowFace, ProgressRingFace, ProgressRingSize, RowAction, RowLeading,
    ScrollRegion, Tone,
};

use crate::model::{Account, AccountId, account_color};
use crate::totp;

/// Seconds left in a period at which the countdown ring turns to a warning.
pub(crate) const EXPIRING_SECONDS: u32 = 5;
/// The widest six- and eight-digit codes, which size the code column so
/// every row's code and ring line up whichever account is shown.
const CODE_PROBE: &str = "0000\u{2009}0000";

/// What the user did in the list, drained by the app each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListEvent {
    Copy(AccountId),
    Select(AccountId),
    Edit(AccountId),
    MoveUp(AccountId),
    MoveDown(AccountId),
    Delete(AccountId),
    /// Install the key for a shared account that has none on this device.
    SetUp(AccountId),
    /// The empty list's action.
    Add,
}

/// Which part of a row the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Body,
    /// The code and ring: copies the code.
    Code,
    /// One of the row's hover actions, by index in its command list.
    Action(usize),
    SetUp,
}

/// A hover action of a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum RowCommand {
    Edit,
    MoveUp,
    MoveDown,
    Delete,
}

impl RowCommand {
    fn action(self) -> RowAction<'static> {
        match self {
            Self::Edit => RowAction {
                icon: Icon::Pencil,
                label: "Edit account",
            },
            Self::MoveUp => RowAction {
                icon: Icon::ChevronUp,
                label: "Move up",
            },
            Self::MoveDown => RowAction {
                icon: Icon::ChevronDown,
                label: "Move down",
            },
            Self::Delete => RowAction {
                icon: Icon::Trash2,
                label: "Delete account",
            },
        }
    }

    fn event(self, id: AccountId) -> ListEvent {
        match self {
            Self::Edit => ListEvent::Edit(id),
            Self::MoveUp => ListEvent::MoveUp(id),
            Self::MoveDown => ListEvent::MoveDown(id),
            Self::Delete => ListEvent::Delete(id),
        }
    }
}

/// The commands of the row at `index` among `count`: reordering offers only
/// the directions that exist.
fn commands(index: usize, count: usize) -> Vec<RowCommand> {
    let mut commands = vec![RowCommand::Edit];
    if index > 0 {
        commands.push(RowCommand::MoveUp);
    }
    if index + 1 < count {
        commands.push(RowCommand::MoveDown);
    }
    commands.push(RowCommand::Delete);
    commands
}

/// Sizes the rows are laid out with, measured from the theme and the live
/// text renderer so they grow with the text scale.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
struct Metrics {
    row_h: f64,
    /// Inset from a row's edges, and the gap between its parts.
    pad: f64,
    gap: f64,
    /// Space above the first row and below the last, and around the code's
    /// hover pill.
    edge: f64,
    ring: f64,
    code_w: f64,
    code_h: f64,
}

impl Metrics {
    fn measure(text: &mut TextRenderer, theme: &Theme) -> Self {
        let spacing = &theme.tokens.spacing;
        let (code_w, code_h) = text.measure_style(CODE_PROBE, None, &code_style(theme));
        let edge = f64::from(spacing.s2);
        Self {
            row_h: ListRowFace::new("Ag")
                .secondary("Ag")
                .height(text, theme)
                .max(f64::from(code_h) + 2.0 * edge),
            pad: f64::from(spacing.s3),
            gap: f64::from(spacing.s3),
            edge,
            // A small ring that holds the seconds left.
            ring: ProgressRingFace::new(0.0)
                .size(ProgressRingSize::Small)
                .value("30")
                .diameter(theme),
            code_w: f64::from(code_w),
            code_h: f64::from(code_h),
        }
    }

    /// Width of a code row's trailing control: the code, a gap and the ring.
    fn code_control(&self) -> f64 {
        self.ring + self.gap + self.code_w
    }
}

/// The rotating code's style: a `Title` in the secret kind, so the digits
/// are tabular and unambiguous.
fn code_style(theme: &Theme) -> TextStyle {
    theme.text(TextRole::Title).with_kind(TextKind::Secret)
}

/// Where the parts of one row are, relative to the row's top-leading corner
/// so they follow it as the list scrolls.
#[derive(Clone, Debug, Default)]
struct RowLayout {
    /// What the [`ListRowFace`] paints: the whole row.
    face: Rect,
    /// The width the face reserves at its trailing edge for the code and
    /// ring, or for the "Set up" button and its caption.
    trailing: f64,
    /// What a row without a key says about it, left of its button; empty when
    /// the row is too narrow to say it.
    caption: Rect,
    /// The code and ring's hover and hit area; empty for a row without a key.
    code: Rect,
    /// The "Set up" button; empty for a row with a key.
    set_up: Rect,
    /// The hover actions, in command order.
    actions: Vec<Rect>,
}

pub struct AccountList {
    /// The area the list fills: below the window's header row.
    rect: Rect,
    /// The readable column inside it that rows stand in.
    column: Rect,
    accounts: Vec<Account>,
    selected: Option<AccountId>,
    scroller: ScrollRegion,
    /// The theme of the last paint, which the scroll region's lanes, steps
    /// and fade read between paints.
    theme: Theme,
    hover: Option<(usize, Zone)>,
    last_mouse: Point,
    events: Vec<ListEvent>,
    /// Measured on each paint and read back by hit-testing and the
    /// accessibility bounds, so the rows the pointer targets are the rows that
    /// were painted. `None` until the first paint.
    metrics: Option<Metrics>,
    layouts: Vec<RowLayout>,
    /// The empty state's action, once painted.
    empty_action: Rect,
}

impl Default for AccountList {
    fn default() -> Self {
        Self::new()
    }
}

impl AccountList {
    pub fn new() -> Self {
        Self {
            rect: Rect::ZERO,
            column: Rect::ZERO,
            accounts: Vec::new(),
            selected: None,
            scroller: ScrollRegion::vertical(),
            theme: Theme::dark(),
            hover: None,
            last_mouse: Point::ZERO,
            events: Vec::new(),
            metrics: None,
            layouts: Vec::new(),
            empty_action: Rect::ZERO,
        }
    }

    /// Replace the (already ordered) accounts the list shows.
    pub fn set_accounts(&mut self, accounts: Vec<Account>) {
        self.accounts = accounts;
        self.layouts.clear();
        self.sync_content();
    }

    pub fn set_selected(&mut self, selected: Option<AccountId>) {
        self.selected = selected;
    }

    pub fn selected(&self) -> Option<AccountId> {
        self.selected
    }

    /// Place the list in `rect`; rows stand in `column`, a readable strip of it.
    pub fn set_viewport(&mut self, rect: Rect, column: Rect) {
        self.rect = rect;
        self.column = column;
        self.scroller.set_viewport(rect, &self.theme);
        self.sync_content();
    }

    pub fn take_events(&mut self) -> Vec<ListEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    /// Show `index`'s row hovered, as a capture of the hover state needs.
    pub(crate) fn stage_hover(&mut self, index: usize, code: bool) {
        self.hover = Some((index, if code { Zone::Code } else { Zone::Body }));
    }

    /// The current painted bounds of an account's row, when it is on screen.
    /// The root uses it to export its virtual accessibility list, so assistive
    /// technology and pointer hit-testing describe the same scrolled geometry.
    pub fn account_bounds(&self, id: AccountId) -> Option<Rect> {
        self.accounts
            .iter()
            .position(|account| account.id == id)
            .map(|index| self.row_rect(index))
            .filter(|rect| rect.intersect(self.rect).area() > 0.0)
    }

    /// Scroll so the row of `id` is fully inside the viewport.
    pub fn reveal(&mut self, id: AccountId) {
        let Some(m) = self.metrics else { return };
        let Some(index) = self.accounts.iter().position(|account| account.id == id) else {
            return;
        };
        let top = index as f64 * m.row_h;
        self.scroller.scroll_into_view(Rect::new(
            0.0,
            top,
            self.column.width(),
            top + m.row_h + 2.0 * m.edge,
        ));
    }

    // ---- geometry shared by painting and hit-testing -------------------

    fn content_height(&self) -> f64 {
        self.metrics
            .map_or(0.0, |m| self.accounts.len() as f64 * m.row_h + 2.0 * m.edge)
    }

    fn sync_content(&mut self) {
        self.scroller
            .set_content_size(self.column.width(), self.content_height());
    }

    /// Bounding rect of visible row `i` in absolute coordinates, scrolled.
    fn row_rect(&self, i: usize) -> Rect {
        let m = self.metrics.unwrap_or_default();
        let y0 = self.rect.y0 + m.edge + i as f64 * m.row_h - self.scroller.offset().y;
        Rect::new(self.column.x0, y0, self.column.x1, y0 + m.row_h)
    }

    /// Measure the rows for `theme` and place the parts of each: the layout
    /// every paint and hit-test reads. Called at the start of each paint, and
    /// by a test that hit-tests without painting.
    pub(crate) fn layout(&mut self, text: &mut TextRenderer, theme: &Theme) {
        let m = Metrics::measure(text, theme);
        self.metrics = Some(m);
        self.theme = theme.clone();
        self.scroller.set_viewport(self.rect, theme);
        self.sync_content();
        let count = self.accounts.len();
        let width = self.column.width();
        let set_up = ButtonFace::new(Rect::ZERO, SET_UP_LABEL).control_size(ControlSize::Small);
        let set_up_w = set_up.intrinsic_width(text, theme);
        let set_up_h = ButtonFace::height(ControlSize::Small, theme);
        let (caption_w, caption_h) =
            text.measure_style(UNKEYED_CAPTION, None, &theme.text(TextRole::Caption));
        let (caption_w, caption_h) = (f64::from(caption_w), f64::from(caption_h));
        // The caption explaining the button shows when the title keeps half a
        // utility panel's width beside it.
        let caption_fits = width - (m.pad + set_up_w + m.gap + caption_w)
            >= f64::from(theme.tokens.metrics.panel_width_utility) / 2.0;
        let layouts = self
            .accounts
            .iter()
            .enumerate()
            .map(|(i, account)| {
                let has_secret = account.secret_is_valid();
                let trailing = if has_secret {
                    m.code_control()
                } else if caption_fits {
                    set_up_w + m.gap + caption_w
                } else {
                    set_up_w
                };
                let face = Rect::new(0.0, 0.0, width, m.row_h);
                let cy = m.row_h / 2.0;
                let slot = ListRowFace::new("Ag")
                    .trailing_width(trailing)
                    .trailing_rect(face, text, theme)
                    .unwrap_or_else(|| Rect::new(width - trailing, 0.0, width, m.row_h));
                let pill = m.code_h.max(m.ring) + 2.0 * m.edge;
                let code = if has_secret {
                    Rect::new(
                        slot.x0 - m.edge,
                        cy - pill / 2.0,
                        slot.x1 + m.edge,
                        cy + pill / 2.0,
                    )
                } else {
                    Rect::ZERO
                };
                let set_up = if has_secret {
                    Rect::ZERO
                } else {
                    Rect::new(
                        slot.x1 - set_up_w,
                        cy - set_up_h / 2.0,
                        slot.x1,
                        cy + set_up_h / 2.0,
                    )
                };
                let caption = if has_secret || !caption_fits {
                    Rect::ZERO
                } else {
                    Rect::new(
                        set_up.x0 - m.gap - caption_w,
                        cy - caption_h / 2.0,
                        set_up.x0 - m.gap,
                        cy + caption_h / 2.0,
                    )
                };
                let row_commands = commands(i, count);
                let actions: Vec<RowAction<'_>> = row_commands.iter().map(|c| c.action()).collect();
                let hovered = Interaction {
                    hovered: true,
                    ..Interaction::default()
                };
                let actions = ListRowFace::new("Ag")
                    .trailing_width(trailing)
                    .actions(&actions)
                    .interaction(hovered)
                    .action_rects(face, theme);
                RowLayout {
                    face,
                    trailing,
                    caption,
                    code,
                    set_up,
                    actions,
                }
            })
            .collect();
        self.layouts = layouts;
    }

    /// `rect` of row `i`'s part, from its row-local `local`.
    fn at(&self, i: usize, local: Rect) -> Rect {
        let row = self.row_rect(i);
        local + vello::kurbo::Vec2::new(row.x0, row.y0)
    }

    /// Which row and part is under `p`, from the layout of the last paint.
    fn hit(&self, p: Point) -> Option<(usize, Zone)> {
        if !self.scroller.content_rect().contains(p) {
            return None;
        }
        let row = (0..self.accounts.len()).find(|&i| self.row_rect(i).contains(p))?;
        let layout = self.layouts.get(row)?;
        if self.at(row, layout.code).contains(p) && layout.code.area() > 0.0 {
            return Some((row, Zone::Code));
        }
        if self.at(row, layout.set_up).contains(p) && layout.set_up.area() > 0.0 {
            return Some((row, Zone::SetUp));
        }
        if self.hover.is_some_and(|(hovered, _)| hovered == row)
            && let Some(index) = layout
                .actions
                .iter()
                .position(|rect| self.at(row, *rect).contains(p))
        {
            return Some((row, Zone::Action(index)));
        }
        Some((row, Zone::Body))
    }

    // ---- painting ------------------------------------------------------

    pub fn draw(&mut self, ctx: &mut PaintCtx<'_>) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        let theme = ctx.theme;
        if self.accounts.is_empty() {
            self.metrics = None;
            self.paint_empty(ctx);
            return;
        }
        self.empty_action = Rect::ZERO;
        self.layout(ctx.text, theme);
        let Some(m) = self.metrics else { return };
        let now = totp::unix_now();
        // Clip to the viewport so scrolled rows never bleed into the header.
        ctx.scene.push_layer(
            Fill::NonZero,
            BlendMode::default(),
            1.0,
            ctx.transform,
            &self.rect,
        );
        let ground = theme.tokens.colors.surfaces.content;
        for i in 0..self.accounts.len() {
            let row = self.row_rect(i);
            if row.y1 < self.rect.y0 || row.y0 > self.rect.y1 {
                continue;
            }
            self.paint_row(ctx, &m, i, now, ground);
        }
        ctx.scene.pop_layer();
        self.scroller.paint_bars(ctx.scene, ctx.transform, theme);
    }

    fn paint_row(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &Metrics,
        i: usize,
        now: f64,
        ground: vello::peniko::Color,
    ) {
        let (Some(account), Some(layout)) = (self.accounts.get(i), self.layouts.get(i)) else {
            return;
        };
        let theme = ctx.theme;
        let row = self.row_rect(i);
        let selected = self.selected == Some(account.id);
        let hovered_zone = self
            .hover
            .filter(|(hovered, _)| *hovered == i)
            .map(|(_, z)| z);
        let hovered = hovered_zone.is_some() && !self.scroller.is_dragging();
        let has_secret = account.secret_is_valid();

        let commands = commands(i, self.accounts.len());
        let actions: Vec<RowAction<'_>> = commands.iter().map(|c| c.action()).collect();
        let hovered_action = match hovered_zone {
            Some(Zone::Action(index)) => Some(index),
            _ => None,
        };
        let id = account.id.to_string();
        let colour = account_color(theme, account.color);
        let avatar = AvatarFace::new(account.title(), &id).presence(colour);
        let mut face = ListRowFace::new(account.title())
            .leading(RowLeading::Avatar(avatar))
            .selected(selected)
            .trailing_width(layout.trailing)
            .actions(&actions)
            .hovered_action(hovered_action)
            .interaction(Interaction {
                hovered,
                ..Interaction::default()
            });
        if !account.label.trim().is_empty() {
            face = face.secondary(&account.label);
        }
        face.paint(ctx, self.at(i, layout.face));

        // The inset rule between two rows hides beside a lifted one: a selected
        // row, or the row the pointer is on.
        let next_selected = self
            .accounts
            .get(i + 1)
            .is_some_and(|next| self.selected == Some(next.id));
        let next_hovered =
            self.hover.is_some_and(|(row, _)| row == i + 1) && !self.scroller.is_dragging();
        if i + 1 < self.accounts.len() && !selected && !hovered && !next_selected && !next_hovered {
            // Where the title starts: past the avatar, which is the list-size
            // avatar beside a secondary line and the inline size without one.
            let metrics = &theme.tokens.metrics;
            let avatar = if account.label.trim().is_empty() {
                metrics.icon_lg
            } else {
                metrics.min_pointer_target
            };
            let text_x = row.x0 + m.pad + f64::from(avatar) + m.gap;
            ui::surface::separator(
                ctx.scene,
                ctx.transform,
                text_x,
                row.x1 - m.pad,
                row.y1 - 0.5,
                0.0,
                ground,
            );
        }

        if has_secret {
            self.paint_code(ctx, m, i, account, now, hovered_zone == Some(Zone::Code));
        } else {
            if layout.caption.area() > 0.0 {
                let caption = self.at(i, layout.caption);
                ctx.text_role(
                    UNKEYED_CAPTION,
                    TextRole::Caption,
                    theme.text_dim,
                    caption.origin(),
                    None,
                );
            }
            let rect = self.at(i, layout.set_up);
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                theme,
                &ButtonFace::new(rect, SET_UP_LABEL)
                    .role(ButtonRole::Secondary)
                    .control_size(ControlSize::Small)
                    .hovered(hovered_zone == Some(Zone::SetUp)),
            );
        }
    }

    fn paint_code(
        &self,
        ctx: &mut PaintCtx<'_>,
        m: &Metrics,
        i: usize,
        account: &Account,
        now: f64,
        hovered: bool,
    ) {
        let Some(layout) = self.layouts.get(i) else {
            return;
        };
        let theme = ctx.theme;
        let row = self.row_rect(i);
        let cy = row.center().y;
        // Hovering the code lifts it like a button: it is where you click.
        if hovered {
            paint_row_selection(
                ctx.scene,
                ctx.transform,
                self.at(i, layout.code),
                SelectionState::new(true, false, false),
                theme.tokens.colors.surfaces.content,
                theme,
            );
        }
        let code = totp::group_code(&totp::code_at(account, now));
        let style = code_style(theme);
        let (width, height) = ctx.text.measure_style(&code, None, &style);
        let ring_x1 = self.at(i, layout.code).x1 - m.edge;
        let ring = Rect::from_center_size(Point::new(ring_x1 - m.ring / 2.0, cy), (m.ring, m.ring));
        let code_right = ring.x0 - m.gap;
        ctx.text(
            &code,
            &style,
            theme.text_primary,
            Point::new(code_right - f64::from(width), cy - f64::from(height) / 2.0),
            None,
        );
        if hovered {
            let side = f64::from(theme.tokens.metrics.icon_md);
            Icon::Copy.draw(
                ctx.scene,
                ctx.transform,
                Rect::from_center_size(ring.center(), (side, side)),
                theme.text_primary,
            );
            return;
        }
        // Reduced motion keeps the countdown honest without sweeping: the
        // ring steps once a second.
        let seconds = totp::seconds_remaining(account, now);
        let fraction = if theme.reduce_motion {
            f64::from(seconds) / f64::from(account.period.max(1))
        } else {
            totp::period_progress(account, now)
        };
        let left = seconds.to_string();
        let mut face = ProgressRingFace::new(fraction)
            .size(ProgressRingSize::Small)
            .value(&left);
        if seconds <= EXPIRING_SECONDS {
            face = face.tone(Tone::Warning);
        }
        face.paint(ctx, ring);
    }

    fn paint_empty(&mut self, ctx: &mut PaintCtx<'_>) {
        let content = EmptyStateContent {
            icon: Some(Icon::Shield),
            message: Some("Scan a QR code or paste a setup link to see its one-time codes here."),
            action_label: Some("Add account"),
            ..EmptyStateContent::new("No accounts yet")
        };
        let action = EmptyState::paint(ctx, self.rect, &content, self.last_mouse);
        self.empty_action = action.unwrap_or(Rect::ZERO);
    }

    // ---- input ---------------------------------------------------------

    /// Whether the scroll bar's fade needs frames now.
    pub fn needs_animation_frame(&self) -> bool {
        self.scroller.needs_animation_frame(&self.theme)
    }

    /// When the scroll bar starts to fade.
    pub fn next_wake_deadline(&self) -> Option<ui::time::Instant> {
        self.scroller.next_wake_deadline(&self.theme)
    }

    /// Whether the scroll bar's thumb is being dragged.
    pub fn is_dragging(&self) -> bool {
        self.scroller.is_dragging()
    }

    pub fn handle_event(&mut self, event: &WindowEvent, ctx: &mut EventCtx<'_>) {
        // The scroll region takes the wheel, touchpad gestures and its bar.
        self.scroller.handle_event(event, ctx, &self.theme);
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                self.last_mouse = ctx.logical(position);
                if self.scroller.is_dragging() {
                    return;
                }
                // The row's actions appear on hover, so the row under the
                // pointer is resolved before its parts.
                let p = self.last_mouse;
                let row = (0..self.accounts.len())
                    .find(|&i| self.rect.contains(p) && self.row_rect(i).contains(p));
                let previous = self.hover;
                self.hover = row.map(|r| (r, Zone::Body));
                self.hover = self.hit(p).or(self.hover);
                if self.hover != previous {
                    *ctx.handled = true;
                }
            }
            WindowEvent::CursorLeft { .. } => {
                if self.hover.take().is_some() {
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } if !*ctx.handled => self.press(ctx.handled),
            _ => {}
        }
    }

    fn press(&mut self, handled: &mut bool) {
        let p = self.last_mouse;
        if self.accounts.is_empty() {
            if self.empty_action.contains(p) {
                self.events.push(ListEvent::Add);
                *handled = true;
            }
            return;
        }
        let Some((i, zone)) = self.hit(p) else { return };
        let (Some(account), Some(_)) = (self.accounts.get(i), self.layouts.get(i)) else {
            return;
        };
        let id = account.id;
        let event = match zone {
            Zone::Body => ListEvent::Select(id),
            Zone::Code => ListEvent::Copy(id),
            Zone::SetUp => ListEvent::SetUp(id),
            Zone::Action(index) => match commands(i, self.accounts.len()).get(index) {
                Some(command) => command.event(id),
                None => return,
            },
        };
        if matches!(zone, Zone::Body | Zone::Code | Zone::SetUp) {
            self.selected = Some(id);
        }
        self.events.push(event);
        *handled = true;
    }
}

const SET_UP_LABEL: &str = "Set up";
/// What a shared account without its key here says about itself.
const UNKEYED_CAPTION: &str = "Not set up on this device";

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::demo_accounts;
    use crate::test_events::{cursor_at, left_press, wheel};

    fn list(width: f64, height: f64, scale: f32) -> AccountList {
        let mut list = AccountList::new();
        let rect = Rect::new(0.0, 0.0, width, height);
        list.set_viewport(rect, rect.inset(-16.0));
        list.set_accounts(demo_accounts());
        let theme = Theme::dark().with_text_scale(scale);
        let mut text = TextRenderer::new();
        text.set_text_scale(theme.text_scale);
        let mut scene = vello::Scene::new();
        list.draw(&mut PaintCtx::new(
            &mut scene,
            vello::kurbo::Affine::IDENTITY,
            &mut text,
            &theme,
        ));
        list
    }

    #[test]
    fn row_metrics_stack_and_grow_with_text_scale() {
        for width in [390.0, 600.0, 732.0] {
            let normal = list(width, 800.0, 1.0);
            let scaled = list(width, 800.0, 2.0);
            assert!(scaled.row_rect(0).height() > normal.row_rect(0).height());
            for list in [&normal, &scaled] {
                for row in 0..list.len() {
                    let rect = list.row_rect(row);
                    if row > 0 {
                        assert!(rect.y0 >= list.row_rect(row - 1).y1);
                    }
                    let code = list.at(row, list.layouts[row].code);
                    assert!(code.x0 >= rect.x0 && code.x1 <= rect.x1);
                    assert!(code.y0 >= rect.y0 && code.y1 <= rect.y1);
                    assert_eq!(list.hit(code.center()), Some((row, Zone::Code)));
                    assert_eq!(list.account_bounds(list.accounts[row].id), Some(rect));
                }
            }
        }
        assert!(AccountList::new().is_empty());
    }

    #[test]
    fn resting_hovered_and_code_rows_paint_in_both_themes() {
        let mut list = list(600.0, 800.0, 1.0);
        let mut text = TextRenderer::new();
        let row = 2;
        for mut theme in [Theme::dark(), Theme::light()] {
            theme.reduce_motion = true;
            for hover in [None, Some((row, Zone::Body)), Some((row, Zone::Code))] {
                list.hover = hover;
                let mut scene = vello::Scene::new();
                list.draw(&mut PaintCtx::new(
                    &mut scene,
                    vello::kurbo::Affine::IDENTITY,
                    &mut text,
                    &theme,
                ));
                assert!(!scene.encoding().is_empty(), "actual row state paints");
            }
        }
    }

    fn dispatch(list: &mut AccountList, event: &WindowEvent) -> bool {
        let mut taffy = taffy::TaffyTree::new();
        let node = taffy.new_leaf(taffy::Style::default()).unwrap();
        let mut doc = automerge::AutoCommit::new();
        let mut focus = None;
        let mut handled = false;
        list.handle_event(
            event,
            &mut EventCtx {
                scale_factor: 1.0,
                modifiers: &winit::event::Modifiers::default(),
                state: ui::widget::Edits::lend(&mut doc),
                path: &automerge::ROOT,
                handled: &mut handled,
                focus_target: &mut focus,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }

    #[test]
    fn row_hit_testing_and_pointer_events_emit_actions_and_scroll() {
        for (width, scale) in [(390.0, 1.0), (732.0, 2.0)] {
            let mut list = list(width, 800.0, scale);
            let row = 1;
            let id = list.accounts[row].id;
            let copy = list.at(row, list.layouts[row].code).center();
            assert!(dispatch(&mut list, &cursor_at(copy)));
            assert!(dispatch(&mut list, &left_press()));
            assert_eq!(list.take_events(), vec![ListEvent::Copy(id)]);
            assert_eq!(list.selected(), Some(id));
            for (index, command) in commands(row, list.len()).iter().enumerate() {
                let point = list.at(row, list.layouts[row].actions[index]).center();
                dispatch(&mut list, &cursor_at(point));
                assert_eq!(list.hit(point), Some((row, Zone::Action(index))));
                assert!(dispatch(&mut list, &left_press()));
                assert_eq!(list.take_events(), vec![command.event(id)]);
            }
        }
        let mut list = list(600.0, 220.0, 1.0);
        assert!(list.scroller.overflows_vertically());
        dispatch(&mut list, &cursor_at(Point::new(300.0, 110.0)));
        assert!(dispatch(&mut list, &wheel(-100.0)));
        assert!(list.scroller.offset().y > 0.0);
        assert!(dispatch(&mut list, &wheel(100.0)));
        assert!(list.scroller.offset().y.abs() < 0.001);
        let first = list.accounts.first().unwrap().id;
        let final_account = list.accounts.last().unwrap().id;
        assert!(
            list.account_bounds(final_account).is_none(),
            "offscreen rows have no admitted bounds"
        );
        list.reveal(final_account);
        let visible = list
            .account_bounds(final_account)
            .expect("the revealed row is admitted");
        assert!(visible.y0 >= list.rect.y0 && visible.y1 <= list.rect.y1);
        assert!(list.account_bounds(first).is_none());
        assert!(
            list.hit(Point::new(list.rect.center().x, list.rect.y0 - 1.0))
                .is_none(),
            "a pointer above the viewport cannot hit an offscreen row"
        );
    }
}
