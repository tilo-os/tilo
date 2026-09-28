//! The scrollable account list — the app's main screen.
//!
//! A custom `Widget` that owns the entire row presentation: a coloured
//! initials avatar, the issuer + account label, the large rotating code,
//! and a [`CountdownRing`] showing seconds left in the period. Rows are
//! laid out by hand (fixed height) from the widget's rect so the rich,
//! mixed-size content stays pixel-aligned, and the list manages its own
//! vertical scroll.
//!
//! Interaction is reported back to the app as [`ListEvent`]s (drained via
//! [`AccountList::take_events`]): clicking a code copies it; clicking a
//! row selects it; the per-row reorder chevrons and delete control move
//! and remove accounts. Codes and ring fractions are recomputed every
//! frame from the live wall clock in [`AccountList::draw`], so the list
//! animates without any external timer plumbing.

use std::collections::HashMap;

use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{FontChoice, TextRenderer, TextStyle, TextWeight};
use ui::theme::Theme;
use ui::widget::{Avatar, Widget};

use crate::model::{Account, AccountId, account_color};
use crate::ring::CountdownRing;
use crate::totp;

/// Row height at 1× before measurement; rows grow with the measured text.
const ROW_HEIGHT: f64 = 76.0;
/// Narrow cards deliberately give the code its own second line.  Shrinking a
/// six/eight-digit credential beside the account identity makes both harder to
/// scan, and at phone-ish widths the old single-line geometry overlapped them.
const NARROW_ROW_HEIGHT: f64 = 108.0;
/// Rows narrower than this always stack the code under the identity; wider
/// rows stack only when the measured identity + code + ring do not fit.
const NARROW_BREAKPOINT: f64 = 480.0;
/// Gap between two account cards — siblings of equal weight, so the `s3`
/// step (HIG §7b), spelled out because the list lays out without a theme.
const ROW_GAP: f64 = ui::tokens::spacing::Spacing::BASE.s3 as f64;
/// Air above the first card and below the last one (`s2`): the first card
/// never touches the header's hairline.
const LIST_PAD_V: f64 = ui::tokens::spacing::Spacing::BASE.s2 as f64;
const PAD: f64 = 16.0;
const AVATAR_R: f64 = 24.0;
const RING_R: f64 = 22.0;
const ACTION_W: f64 = 26.0;
/// Minimum copy target left of the code (the 1× value; it grows with the
/// measured code so the identity text never runs under the digits).
const COPY_ZONE_W: f64 = 210.0;
/// Issuer / label / code type sizes, shared by measurement and paint.
const ISSUER_SIZE: f32 = 16.0;
const LABEL_SIZE: f32 = 13.0;
const CODE_SIZE: f32 = 30.0;
const STACKED_CODE_SIZE: f32 = 24.0;
/// Top inset of the identity block in a stacked (narrow) row.
const STACKED_TOP: f64 = 18.0;
/// Least width the identity column keeps before a wide row stacks instead.
const MIN_IDENTITY_W: f64 = 140.0;
/// Vertical padding above and below the identity block in a wide row.
const ROW_PAD_V: f64 = 14.0;
/// Widest six/eight-digit code, used to size the copy zone and the
/// one-line/stacked decision independently of which account is shown.
const CODE_PROBE: &str = "0000 0000";

/// What the user did in the list, drained by the app each frame.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ListEvent {
    Copy(AccountId),
    Select(AccountId),
    MoveUp(AccountId),
    MoveDown(AccountId),
    Delete(AccountId),
}

/// Which sub-region of a row the pointer is over.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Zone {
    Body,
    Copy,
    Up,
    Down,
    Delete,
}

pub struct AccountList {
    rect: Rect,
    accounts: Vec<Account>,
    selected: Option<AccountId>,
    scroll: f64,
    hover_row: Option<usize>,
    hover_zone: Option<Zone>,
    last_mouse: Point,
    events: Vec<ListEvent>,
    ring: CountdownRing,
    /// Set to the empty-reason the app wants shown (e.g. "No matches for
    /// …") so the empty state is contextual.
    empty_message: String,
    /// Row geometry measured from the live text renderer on each paint and
    /// read back by hit-testing and the accessibility bounds. `None` until the
    /// first paint, when the authored 1× rules stand in.
    metrics: Option<RowMetrics>,
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
            accounts: Vec::new(),
            selected: None,
            scroll: 0.0,
            hover_row: None,
            hover_zone: None,
            last_mouse: Point::ZERO,
            events: Vec::new(),
            ring: CountdownRing::new(),
            empty_message: String::new(),
            metrics: None,
        }
    }

    /// Replace the visible (already-filtered, already-ordered) account
    /// set the list renders.
    pub fn set_accounts(&mut self, accounts: Vec<Account>) {
        self.accounts = accounts;
        self.clamp_scroll();
    }

    pub fn set_selected(&mut self, selected: Option<AccountId>) {
        self.selected = selected;
    }

    pub fn selected(&self) -> Option<AccountId> {
        self.selected
    }

    pub fn set_empty_message(&mut self, msg: impl Into<String>) {
        self.empty_message = msg.into();
    }

    /// The contextual empty-state message the list will show when it has no
    /// rows (e.g. "No accounts match …" for a non-matching search).
    pub fn empty_message(&self) -> &str {
        &self.empty_message
    }

    pub fn set_viewport(&mut self, rect: Rect) {
        self.rect = rect;
        self.clamp_scroll();
    }

    pub fn take_events(&mut self) -> Vec<ListEvent> {
        std::mem::take(&mut self.events)
    }

    /// The account at visible index `i`, if any.
    pub fn account_at(&self, i: usize) -> Option<&Account> {
        self.accounts.get(i)
    }

    pub fn len(&self) -> usize {
        self.accounts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.accounts.is_empty()
    }

    #[cfg(test)]
    pub(crate) fn scroll_offset(&self) -> f64 {
        self.scroll
    }

    /// The current painted bounds for an account row. The root uses this when
    /// exporting its virtual accessibility list, so assistive technology and
    /// pointer hit-testing describe the same scrolled geometry.
    pub fn account_bounds(&self, id: AccountId) -> Option<Rect> {
        self.accounts
            .iter()
            .position(|account| account.id == id)
            .map(|index| self.row_rect(index))
            .filter(|rect| rect.intersect(self.rect).area() > 0.0)
    }

    fn content_height(&self) -> f64 {
        let n = self.accounts.len() as f64;
        if n == 0.0 {
            return 0.0;
        }
        n * self.row_height() + (n - 1.0) * ROW_GAP + 2.0 * LIST_PAD_V
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.rect.height()).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// Bounding rect of visible row `i` (in absolute coords, scrolled).
    fn row_rect(&self, i: usize) -> Rect {
        let row_height = self.row_height();
        let y0 = self.rect.y0 + LIST_PAD_V + i as f64 * (row_height + ROW_GAP) - self.scroll;
        Rect::new(self.rect.x0 + PAD, y0, self.rect.x1 - PAD, y0 + row_height)
    }

    /// Width of one row (the viewport minus its side padding).
    fn row_width(&self) -> f64 {
        (self.rect.width() - 2.0 * PAD).max(0.0)
    }

    /// The measured row metrics from the last paint, or the authored 1×
    /// rules for this width before anything has been painted.
    fn metrics(&self) -> RowMetrics {
        self.metrics
            .unwrap_or_else(|| RowMetrics::fallback(self.row_width()))
    }

    fn row_height(&self) -> f64 {
        self.metrics().row_h
    }

    /// Geometry landmarks within a row, derived from one set of metrics and
    /// shared by draw + hit-test so they never drift.
    fn row_layout(&self, r: Rect) -> RowLayout {
        let m = self.metrics();
        let actions_x1 = r.x1 - 12.0;
        let actions_x0 = actions_x1 - ACTION_W;
        let ring_center_x = actions_x0 - 16.0 - m.ring_r;
        let identity_h = m.issuer_h + m.gap + m.label_h;
        let (avatar_cy, code_cy) = if m.stacked {
            let band = stacked_identity_band(identity_h, m.avatar_r, m.gap);
            (
                r.y0 + STACKED_TOP + band / 2.0,
                r.y0 + m.code_top + m.code_h / 2.0,
            )
        } else {
            (r.center().y, r.center().y)
        };
        let ring_center = Point::new(ring_center_x, code_cy);
        let code_right = ring_center_x - m.ring_r - 18.0;
        let avatar_c = Point::new(r.x0 + 18.0 + m.avatar_r, avatar_cy);
        let text_x = avatar_c.x + m.avatar_r + 16.0;
        RowLayout {
            actions_x0,
            actions_x1,
            ring_c: ring_center,
            code_right,
            avatar_c,
            text_x,
        }
    }

    /// Which row + zone is under the absolute point `p`.
    fn hit(&self, p: Point) -> Option<(usize, Zone)> {
        if !self.rect.contains(p) {
            return None;
        }
        for i in 0..self.accounts.len() {
            let r = self.row_rect(i);
            if !r.contains(p) {
                continue;
            }
            let l = self.row_layout(r);
            let m = self.metrics();
            // Action column (3 stacked zones) takes priority.
            if p.x >= l.actions_x0 && p.x <= l.actions_x1 {
                let third = r.height() / 3.0;
                let zone = if p.y < r.y0 + third {
                    Zone::Up
                } else if p.y < r.y0 + 2.0 * third {
                    Zone::Down
                } else {
                    Zone::Delete
                };
                return Some((i, zone));
            }
            // Copy zone: in a stacked row the code deliberately moves below
            // the identity block, so tapping its lower line never steals a
            // normal row-selection tap from the title/label.
            let copy_x0 = if m.stacked {
                l.text_x
            } else {
                (l.code_right - m.copy_zone_w).max(l.text_x)
            };
            let in_code_line = !m.stacked || p.y >= r.y0 + m.code_top - m.gap;
            if in_code_line && p.x >= copy_x0 && p.x <= l.code_right + 8.0 {
                return Some((i, Zone::Copy));
            }
            return Some((i, Zone::Body));
        }
        None
    }

    // ---- drawing -----------------------------------------------------

    fn draw_avatar(
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        center: Point,
        radius: f64,
        account: &Account,
    ) {
        // The account's colour is user-assignable, so it's kept; the disc +
        // centred monogram are painted by the shared toolkit avatar so the
        // shape/placement match every other app's roster. `radius` is the
        // row's measured `avatar_r`, scaled with the text so the monogram
        // (0.4× the disc, through the renderer) stays inside the disc.
        let color = account_color(theme, account.color);
        let rect = Rect::new(
            center.x - radius,
            center.y - radius,
            center.x + radius,
            center.y + radius,
        );
        Avatar::draw_at(
            scene,
            transform,
            rect,
            &account.initials(),
            color,
            theme,
            text_renderer,
        );
    }

    fn draw_action_icons(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        l: &RowLayout,
        r: Rect,
        hovered_zone: Option<Zone>,
    ) {
        use vello::kurbo::BezPath;
        let cx = f64::midpoint(l.actions_x0, l.actions_x1);
        let third = r.height() / 3.0;
        let centers = [
            (Zone::Up, r.y0 + third * 0.5),
            (Zone::Down, r.y0 + third * 1.5),
            (Zone::Delete, r.y0 + third * 2.5),
        ];
        for (zone, cy) in centers {
            let active = hovered_zone == Some(zone);
            let color = if active {
                if zone == Zone::Delete {
                    theme.tokens.colors.semantic.danger
                } else {
                    theme.tokens.colors.text_primary
                }
            } else {
                theme.tokens.colors.text_dim.with_alpha_factor(0.7)
            };
            let s = 4.0;
            let mut path = BezPath::new();
            match zone {
                Zone::Up => {
                    path.move_to((cx - s, cy + s * 0.6));
                    path.line_to((cx, cy - s * 0.6));
                    path.line_to((cx + s, cy + s * 0.6));
                    scene.stroke(&stroke(1.6), transform, color, None, &path);
                }
                Zone::Down => {
                    path.move_to((cx - s, cy - s * 0.6));
                    path.line_to((cx, cy + s * 0.6));
                    path.line_to((cx + s, cy - s * 0.6));
                    scene.stroke(&stroke(1.6), transform, color, None, &path);
                }
                Zone::Delete => {
                    // a small trash glyph: lid + body lines
                    path.move_to((cx - s, cy - s * 0.7));
                    path.line_to((cx + s, cy - s * 0.7));
                    path.move_to((cx - s * 0.7, cy - s * 0.7));
                    path.line_to((cx - s * 0.55, cy + s));
                    path.line_to((cx + s * 0.55, cy + s));
                    path.line_to((cx + s * 0.7, cy - s * 0.7));
                    scene.stroke(&stroke(1.6), transform, color, None, &path);
                }
                _ => {}
            }
        }
    }

    #[allow(
        clippy::too_many_lines,
        reason = "row geometry and its code/security state share one paint path"
    )]
    fn draw_row(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        i: usize,
        now: f64,
    ) {
        // `i` always comes from `0..self.accounts.len()` in `draw`'s loop, so
        // this is unreachable in practice — `.get()` documents that instead
        // of indexing straight into the vector.
        let Some(account) = self.accounts.get(i).cloned() else {
            return;
        };
        let r = self.row_rect(i);
        // Cull rows scrolled outside the viewport.
        if r.y1 < self.rect.y0 || r.y0 > self.rect.y1 {
            return;
        }
        let l = self.row_layout(r);
        let m = self.metrics();
        let is_selected = self.selected == Some(account.id);
        let is_hovered = self.hover_row == Some(i);
        let hovered_zone = if is_hovered { self.hover_zone } else { None };

        Self::draw_row_card(
            scene,
            transform,
            theme,
            r,
            &account,
            is_selected,
            is_hovered,
        );

        // Avatar.
        Self::draw_avatar(
            scene,
            transform,
            text_renderer,
            theme,
            l.avatar_c,
            m.avatar_r,
            &account,
        );

        // Issuer + label, vertically centred as a block on the avatar. The
        // available text width shrinks with the row so a narrow window
        // truncates rather than colliding with the code.
        let stacked = m.stacked;
        let text_w = if stacked {
            (l.actions_x0 - 12.0 - l.text_x).max(60.0)
        } else {
            (l.code_right - m.copy_zone_w - l.text_x).max(60.0)
        } as f32;
        let issuer = account.title().to_string();
        let has_label = !account.label.trim().is_empty();
        let block_h = if has_label {
            m.issuer_h + m.gap + m.label_h
        } else {
            m.issuer_h
        };
        let issuer_y = l.avatar_c.y - block_h / 2.0;
        text_renderer.draw_text_weighted(
            scene,
            &issuer,
            ISSUER_SIZE,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((l.text_x, issuer_y)),
            Some(text_w),
            TextWeight::Semibold,
        );
        if has_label {
            text_renderer.draw_text(
                scene,
                &account.label,
                LABEL_SIZE,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((l.text_x, issuer_y + m.issuer_h + m.gap)),
                Some(text_w),
            );
        }

        // The big rotating code, right-aligned to `code_right`. Shared account
        // metadata can arrive before its device-local secret; make that state
        // explicit instead of painting a plausible-looking (but unusable) code.
        let has_local_secret = account.secret_is_valid();
        let grouped = if has_local_secret {
            totp::group_code(&totp::code_at(&account, now))
        } else {
            "Set up locally".to_string()
        };
        let code_size = if has_local_secret {
            if stacked {
                STACKED_CODE_SIZE
            } else {
                CODE_SIZE
            }
        } else {
            13.0
        };
        let (code_w, code_h) =
            text_renderer.measure_styled(&grouped, code_size, None, TextWeight::Bold, false, true);
        let copy_hot = has_local_secret && is_hovered && self.hover_zone == Some(Zone::Copy);
        let code_color = if copy_hot {
            account_color(theme, account.color)
        } else {
            theme.tokens.colors.text_primary
        };
        let code_x = l.code_right - f64::from(code_w);
        // The code and the ring share one centre line (`l.ring_c.y`).
        let code_y = l.ring_c.y - f64::from(code_h) / 2.0;
        text_renderer.draw_style(
            scene,
            &grouped,
            code_color,
            transform * Affine::translate((code_x, code_y)),
            None,
            &TextStyle::new(code_size)
                .with_weight(TextWeight::Bold)
                .with_family(FontChoice::Monospace),
        );
        // "Copy" affordance under the code on hover.
        if copy_hot {
            text_renderer.draw_text(
                scene,
                "click to copy",
                10.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((code_x, code_y + f64::from(code_h) + 2.0)),
                None,
            );
        }

        if has_local_secret {
            // Reduced motion retains useful expiry information without sweeping
            // continuously: the ring advances in whole-second steps alongside
            // its numeric label.
            let secs = totp::seconds_remaining(&account, now);
            let frac = if theme.reduce_motion {
                f64::from(secs) / f64::from(account.period.max(1))
            } else {
                totp::period_progress(&account, now)
            };
            self.ring
                .place(l.ring_c, m.ring_r)
                .set_fraction(frac)
                .set_seconds(secs)
                .set_color(account_color(theme, account.color));
            self.ring.draw_arc(scene, transform, text_renderer, theme);
        }

        // Hover action icons.
        if is_hovered {
            Self::draw_action_icons(scene, transform, theme, &l, r, hovered_zone);
        }
    }

    /// Paint the row background card, tinted for selection/hover, plus the
    /// accent left-edge marker on the selected row. Split out of
    /// [`Self::draw_row`] to keep that function's line count in check.
    ///
    /// The card is an opaque raised surface with its derived rest edge
    /// (`ui::surface::card`, HIG §7 / §9.5): a translucent wash over the base
    /// had no edge at all on the dark preset, so five stacked rows read as
    /// one undivided block. Hover and selection step the same way the shared
    /// controls do (`bg_hover`, `bg_active`), so the pointer always lifts a
    /// row rather than sinking it.
    fn draw_row_card(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        r: Rect,
        account: &Account,
        is_selected: bool,
        is_hovered: bool,
    ) {
        let bg = if is_selected {
            theme.bg_active
        } else if is_hovered {
            theme.bg_hover
        } else {
            theme.tokens.colors.surfaces.raised
        };
        ui::surface::card(scene, transform, r, f64::from(theme.tokens.radii.lg), bg);
        if is_selected {
            // accent left-edge marker
            let marker = RoundedRect::new(r.x0, r.y0 + 10.0, r.x0 + 3.0, r.y1 - 10.0, 2.0);
            scene.fill(
                Fill::NonZero,
                transform,
                account_color(theme, account.color),
                None,
                &marker,
            );
        }
    }

    fn draw_empty(
        &self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
    ) {
        let msg = if self.empty_message.is_empty() {
            "No accounts yet".to_string()
        } else {
            self.empty_message.clone()
        };
        let size = 16.0;
        let (w, _h) = text_renderer.measure(&msg, size, None);
        let cx = self.rect.center().x - f64::from(w) / 2.0;
        let cy = self.rect.center().y - 30.0;
        text_renderer.draw_text(
            scene,
            &msg,
            size,
            theme.tokens.colors.text_dim,
            transform * Affine::translate((cx, cy)),
            None,
        );
        let hint = "Press the + button or Ctrl+N to add one";
        let (hw, _hh) = text_renderer.measure(hint, 13.0, None);
        text_renderer.draw_text(
            scene,
            hint,
            13.0,
            theme.tokens.colors.text_dim.with_alpha_factor(0.7),
            transform * Affine::translate((self.rect.center().x - f64::from(hw) / 2.0, cy + 28.0)),
            None,
        );
    }
}

struct RowLayout {
    actions_x0: f64,
    actions_x1: f64,
    ring_c: Point,
    code_right: f64,
    avatar_c: Point,
    text_x: f64,
}

/// Height of a stacked row's identity band: the issuer + label block, or the
/// avatar disc less the one-`gap` overhang it is allowed on each side (the
/// 1× disc is 48 px on a 40 px block, and that reads fine because glyph ink
/// sits inside its line box). Shared by `RowMetrics::measure` and
/// `row_layout` so the code line and the avatar centre agree.
fn stacked_identity_band(identity_h: f64, avatar_r: f64, gap: f64) -> f64 {
    identity_h.max(2.0 * avatar_r - 2.0 * gap)
}

/// Row geometry derived from the measured text, refreshed on every paint and
/// read by hit-testing and the accessibility bounds, so the rows the pointer
/// targets are the rows that were painted at the current text scale.
#[derive(Clone, Copy, Debug, PartialEq)]
struct RowMetrics {
    /// The code drops onto its own line under the identity block.
    stacked: bool,
    row_h: f64,
    /// Line boxes of the issuer and label, and the gap between them.
    issuer_h: f64,
    label_h: f64,
    gap: f64,
    /// Line box of the code at this row shape's code size.
    code_h: f64,
    /// Radius of the countdown ring; grows with the text scale so its
    /// seconds label stays inside it.
    ring_r: f64,
    /// Radius of the initials avatar. The shared `Avatar::draw_at` sizes the
    /// monogram at 0.4× the disc through the renderer, which applies the text
    /// scale, so the disc follows the same scale to keep the initials inside.
    avatar_r: f64,
    /// Width of the copy target left of the code.
    copy_zone_w: f64,
    /// Stacked rows only: top of the code line, relative to the row's top.
    code_top: f64,
}

impl RowMetrics {
    /// The authored 1× rules, used before the first paint has measured
    /// anything (and by the geometry tests, which never paint).
    fn fallback(row_w: f64) -> Self {
        let stacked = row_w < NARROW_BREAKPOINT;
        // The 1× line boxes (16 px semibold issuer, 13 px label) and the `s1`
        // gap; `code_top` follows the same rule `measure` applies so pre-paint
        // hit geometry equals painted geometry.
        let (issuer_h, label_h, gap) = (20.0, 16.25, 4.0);
        Self {
            stacked,
            row_h: if stacked {
                NARROW_ROW_HEIGHT
            } else {
                ROW_HEIGHT
            },
            issuer_h,
            label_h,
            gap,
            code_h: if stacked { 30.0 } else { 37.5 },
            ring_r: RING_R,
            avatar_r: AVATAR_R,
            copy_zone_w: COPY_ZONE_W,
            code_top: STACKED_TOP + issuer_h + gap + label_h + gap,
        }
    }

    /// Measure the row shape for `row_w` with the styles the row paints with.
    /// A row stacks when it is narrower than [`NARROW_BREAKPOINT`] or when the
    /// identity column, the widest code, the ring and the action column do
    /// not all fit on one line at the current text scale.
    fn measure(tr: &mut TextRenderer, theme: &Theme, row_w: f64) -> Self {
        let sp = &theme.tokens.spacing;
        let gap = f64::from(sp.s1);
        let issuer_h = f64::from(
            tr.measure_weighted("Ag", ISSUER_SIZE, None, TextWeight::Semibold)
                .1,
        );
        let label_h = f64::from(tr.measure("Ag", LABEL_SIZE, None).1);
        let text_scale = f64::from(theme.text_scale).max(1.0);
        let ring_r = RING_R * text_scale;
        let avatar_r = AVATAR_R * text_scale;
        let (wide_code_w, wide_code_h) =
            tr.measure_styled(CODE_PROBE, CODE_SIZE, None, TextWeight::Bold, false, true);
        let wide_code_w = f64::from(wide_code_w);
        let copy_zone_w = (wide_code_w + 2.0 * f64::from(sp.s6)).max(COPY_ZONE_W);
        let needed = 18.0
            + 2.0 * avatar_r
            + 16.0
            + MIN_IDENTITY_W
            + copy_zone_w
            + 18.0
            + 2.0 * ring_r
            + 16.0
            + ACTION_W
            + 12.0;
        let stacked = row_w < NARROW_BREAKPOINT.max(needed);
        if stacked {
            let (_, code_h) = tr.measure_styled(
                CODE_PROBE,
                STACKED_CODE_SIZE,
                None,
                TextWeight::Bold,
                false,
                true,
            );
            let code_h = f64::from(code_h);
            // The identity band is the text block, or the avatar disc less
            // the one-`gap` overhang it already has at 1× (a 48 px disc on a
            // 40 px block), whichever is taller — so at 1× nothing moves and
            // a scaled disc still never reaches the code line.
            let identity_band = stacked_identity_band(issuer_h + gap + label_h, avatar_r, gap);
            let code_top = STACKED_TOP + identity_band + gap;
            let code_cy = code_top + code_h / 2.0;
            let row_h =
                (code_cy + (code_h / 2.0).max(ring_r) + f64::from(sp.s2)).max(NARROW_ROW_HEIGHT);
            Self {
                stacked: true,
                row_h,
                issuer_h,
                label_h,
                gap,
                code_h,
                ring_r,
                avatar_r,
                copy_zone_w,
                code_top,
            }
        } else {
            let identity_h = issuer_h + gap + label_h;
            let row_h = (identity_h + 2.0 * ROW_PAD_V)
                .max(2.0 * ring_r + 2.0 * f64::from(sp.s3))
                .max(2.0 * avatar_r + 2.0 * f64::from(sp.s3))
                .max(ROW_HEIGHT);
            Self {
                stacked: false,
                row_h,
                issuer_h,
                label_h,
                gap,
                code_h: f64::from(wide_code_h),
                ring_r,
                avatar_r,
                copy_zone_w,
                code_top: 0.0,
            }
        }
    }
}

fn stroke(w: f64) -> vello::kurbo::Stroke {
    vello::kurbo::Stroke::new(w)
        .with_caps(vello::kurbo::Cap::Round)
        .with_join(vello::kurbo::Join::Round)
}

impl Widget for AccountList {
    fn build_layout(
        &mut self,
        taffy: &mut TaffyTree<()>,
        _theme: &Theme,
        node_map: &mut HashMap<NodeId, *mut dyn Widget>,
    ) -> NodeId {
        #[allow(
            clippy::unwrap_used,
            reason = "taffy 0.10's new_leaf always inserts and returns Ok; see ui::widget::TaffyTreeExt's doc comment"
        )]
        let node = taffy
            .new_leaf(Style {
                size: Size {
                    width: Dimension::percent(1.0),
                    height: Dimension::percent(1.0),
                },
                flex_grow: 1.0,
                ..Default::default()
            })
            .unwrap();
        node_map.insert(node, std::ptr::from_mut::<dyn Widget>(self));
        node
    }

    fn apply_layout(&mut self, taffy: &TaffyTree<()>, node: NodeId, origin: Point) {
        let layout = taffy.layout(node).copied().unwrap_or_default();
        let x0 = origin.x + f64::from(layout.location.x);
        let y0 = origin.y + f64::from(layout.location.y);
        self.rect = Rect::new(
            x0,
            y0,
            x0 + f64::from(layout.size.width),
            y0 + f64::from(layout.size.height),
        );
        self.clamp_scroll();
    }

    fn draw(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text_renderer: &mut TextRenderer,
        theme: &Theme,
        _presence: &PresenceCtx,
    ) {
        if self.rect.width() <= 0.0 || self.rect.height() <= 0.0 {
            return;
        }
        if self.accounts.is_empty() {
            self.draw_empty(scene, transform, text_renderer, theme);
            return;
        }
        // Measure the row shape for this width and text scale before painting
        // so the rows, the scroll range and the next hit-test all agree.
        self.metrics = Some(RowMetrics::measure(text_renderer, theme, self.row_width()));
        self.clamp_scroll();
        let now = totp::unix_now();
        // Clip to the viewport so scrolled rows don't bleed into chrome.
        let clip = RoundedRect::from_rect(self.rect, 0.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        for i in 0..self.accounts.len() {
            self.draw_row(scene, transform, text_renderer, theme, i, now);
        }
        scene.pop_layer();
    }

    fn handle_event(&mut self, event: &WindowEvent, ctx: &mut ui::widget::EventCtx<'_>) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / ctx.scale_factor, position.y / ctx.scale_factor);
                self.last_mouse = p;
                let hit = self.hit(p);
                let (row, zone) = match hit {
                    Some((r, z)) => (Some(r), Some(z)),
                    None => (None, None),
                };
                if row != self.hover_row || zone != self.hover_zone {
                    self.hover_row = row;
                    self.hover_zone = zone;
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseInput {
                state: ElementState::Pressed,
                button: MouseButton::Left,
                ..
            } => {
                if let Some((i, zone)) = self.hit(self.last_mouse)
                    && let Some(account) = self.accounts.get(i)
                {
                    let id = account.id;
                    let ev = match zone {
                        // A peer can share an account identity without sharing
                        // its bearer secret. Clicking its code area selects the
                        // row rather than attempting a meaningless clipboard
                        // write; assistive technology exposes the same disabled
                        // Copy control from the root semantic tree.
                        Zone::Copy if !account.secret_is_valid() => ListEvent::Select(id),
                        Zone::Copy => ListEvent::Copy(id),
                        Zone::Up => ListEvent::MoveUp(id),
                        Zone::Down => ListEvent::MoveDown(id),
                        Zone::Delete => ListEvent::Delete(id),
                        Zone::Body => ListEvent::Select(id),
                    };
                    if matches!(zone, Zone::Body | Zone::Copy) {
                        self.selected = Some(id);
                    }
                    self.events.push(ev);
                    *ctx.handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.rect.contains(self.last_mouse) && self.max_scroll() > 0.0 =>
            {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / ctx.scale_factor,
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
                *ctx.handled = true;
            }
            _ => {}
        }
    }

    fn name(&self) -> &'static str {
        "AccountList"
    }

    fn get_rect(&self) -> Rect {
        self.rect
    }

    /// Codes + rings refresh every frame from the wall clock.
    fn needs_animation_frame(&self) -> bool {
        true
    }
}

#[cfg(test)]
#[allow(clippy::float_cmp, reason = "tests intentionally assert exact values")]
mod tests {
    use super::*;
    use crate::model::demo_accounts;
    use automerge::AutoCommit;

    fn list_with_demo() -> AccountList {
        let mut l = AccountList::new();
        l.set_viewport(Rect::new(0.0, 0.0, 600.0, 800.0));
        l.set_accounts(demo_accounts());
        l
    }

    /// Row metrics: identity band, stacking, narrow layout, text scale, and the empty list.
    #[test]
    fn row_metrics_stack_and_grow_with_text_scale() {
        // Case `the_stacked_identity_band_is_unchanged_at_one_x_and_grows_with_the_disc`.
        {
            // 1×: a 48 px disc on a 40.25 px block keeps the block's height (the
            // disc's one-`gap` overhang per side is the accepted 1× look).
            assert_eq!(stacked_identity_band(40.25, AVATAR_R, 4.0), 40.25);
            // 2×: the 96 px disc now decides, so the code line lands under it.
            assert_eq!(stacked_identity_band(80.5, 2.0 * AVATAR_R, 4.0), 88.0);
            // The pre-paint rules carry the 1× radius.
            assert_eq!(RowMetrics::fallback(300.0).avatar_r, AVATAR_R);
        }

        // Case `rows_stack_without_overlap`.
        {
            let l = list_with_demo();
            let r0 = l.row_rect(0);
            let r1 = l.row_rect(1);
            assert!(r1.y0 >= r0.y1, "row 1 must start at/after row 0 ends");
        }

        // Case `narrow_rows_put_codes_on_a_separate_copy_line`.
        {
            let mut l = AccountList::new();
            l.set_viewport(Rect::new(0.0, 0.0, 390.0, 800.0));
            l.set_accounts(demo_accounts());
            let r = l.row_rect(0);
            let layout = l.row_layout(r);
            assert_eq!(r.height(), NARROW_ROW_HEIGHT);
            assert!(layout.ring_c.y > layout.avatar_c.y + AVATAR_R);
            // Identity taps remain body selection; code taps are explicitly on the
            // lower line and reach the same Copy event path as wide rows.
            assert_eq!(
                l.hit(Point::new(layout.text_x + 4.0, r.y0 + 36.0)),
                Some((0, Zone::Body))
            );
            assert_eq!(
                l.hit(Point::new(layout.code_right - 20.0, r.y0 + 74.0)),
                Some((0, Zone::Copy))
            );
        }

        // Case `measured_rows_match_the_authored_shape_at_normal_text`.
        {
            let mut tr = TextRenderer::new();
            let theme = Theme::dark();
            let m = RowMetrics::measure(&mut tr, &theme, 568.0);
            assert!(
                !m.stacked,
                "a 600px viewport keeps identity and code on one line"
            );
            assert_eq!(m.row_h, ROW_HEIGHT, "1× rows keep their authored height");
            assert_eq!(m.ring_r, RING_R);
            let narrow = RowMetrics::measure(&mut tr, &theme, 358.0);
            assert!(narrow.stacked);
            assert_eq!(narrow.row_h, NARROW_ROW_HEIGHT);
        }

        // Case `large_text_grows_rows_and_stacks_before_the_code_meets_the_identity`.
        {
            let mut tr = TextRenderer::new();
            let theme = Theme::dark().with_text_scale(2.0);
            // The renderer scales text the way the launch harness does for a
            // 200% theme (`FrameHarness` mirrors `theme.text_scale` into it).
            tr.set_text_scale(theme.text_scale);
            // A laptop-wide row at 200% no longer fits identity + code + ring on
            // one line, so it stacks and grows instead of overlapping.
            let m = RowMetrics::measure(&mut tr, &theme, 700.0);
            assert!(m.stacked, "200% text stacks a 700px row");
            assert!(
                m.row_h > NARROW_ROW_HEIGHT,
                "stacked row grows with its text"
            );
            assert!(
                m.ring_r > RING_R,
                "the ring grows so its seconds fit inside"
            );
            assert!(
                m.code_top >= STACKED_TOP + m.issuer_h + m.gap + m.label_h,
                "the code line starts below the identity block"
            );
            // A very wide row still fits on one line and is taller than at 1×.
            let wide = RowMetrics::measure(&mut tr, &theme, 1400.0);
            assert!(!wide.stacked);
            assert!(wide.row_h > ROW_HEIGHT);
            assert!(
                wide.copy_zone_w > COPY_ZONE_W,
                "copy target follows the wider code"
            );
        }

        // Case `empty_list_reports_empty`.
        {
            let mut l = AccountList::new();
            l.set_viewport(Rect::new(0.0, 0.0, 600.0, 800.0));
            assert!(l.is_empty());
        }
    }

    /// Hit-testing follows painted metrics; clicks and the wheel emit copy/reorder/delete and scroll.
    #[test]
    fn row_hit_testing_and_pointer_events_emit_actions_and_scroll() {
        // Case `copy_zone_and_action_zones_hit_test_correctly`.
        {
            let l = list_with_demo();
            let r = l.row_rect(0);
            let layout = l.row_layout(r);
            // Just left of the ring → copy zone.
            let copy_p = Point::new(layout.code_right - 20.0, r.center().y);
            assert_eq!(l.hit(copy_p), Some((0, Zone::Copy)));
            // The avatar/text area → body (select).
            let body_p = Point::new(layout.text_x + 4.0, r.center().y);
            assert_eq!(l.hit(body_p), Some((0, Zone::Body)));
            // Top of the action column → reorder-up.
            let up_p = Point::new(
                f64::midpoint(layout.actions_x0, layout.actions_x1),
                r.y0 + 6.0,
            );
            assert_eq!(l.hit(up_p), Some((0, Zone::Up)));
            // Bottom of the action column → delete.
            let del_p = Point::new(
                f64::midpoint(layout.actions_x0, layout.actions_x1),
                r.y1 - 6.0,
            );
            assert_eq!(l.hit(del_p), Some((0, Zone::Delete)));
        }

        // Case `painted_metrics_drive_hit_testing`.
        {
            // After a paint at 200%, the hit-test uses the measured (stacked)
            // geometry: the code line is where it was painted, not at the 1× y.
            let mut l = AccountList::new();
            l.set_viewport(Rect::new(0.0, 0.0, 732.0, 800.0));
            l.set_accounts(demo_accounts());
            let mut tr = TextRenderer::new();
            let theme = Theme::dark().with_text_scale(2.0);
            tr.set_text_scale(theme.text_scale);
            l.metrics = Some(RowMetrics::measure(&mut tr, &theme, l.row_width()));
            let r = l.row_rect(0);
            let layout = l.row_layout(r);
            let m = l.metrics();
            assert!(m.stacked);
            assert_eq!(r.height(), m.row_h);
            // Identity area above the code line selects; the code line copies.
            assert_eq!(
                l.hit(Point::new(layout.text_x + 4.0, r.y0 + STACKED_TOP + 4.0)),
                Some((0, Zone::Body))
            );
            assert_eq!(
                l.hit(Point::new(
                    layout.code_right - 20.0,
                    r.y0 + m.code_top + 4.0
                )),
                Some((0, Zone::Copy))
            );
            // The ring sits on the code's centre line, clear of the identity.
            assert!(layout.ring_c.y > layout.avatar_c.y + AVATAR_R);
        }

        // Case `mouse_click_in_the_copy_zone_emits_a_copy_event`.
        {
            let mut l = list_with_demo();
            let id = l.account_at(0).unwrap().id;
            let r = l.row_rect(0);
            let layout = l.row_layout(r);
            let copy_p = Point::new(layout.code_right - 20.0, r.center().y);
            dispatch(&mut l, &cursor_at(copy_p));
            let handled = dispatch(&mut l, &left_press());
            assert!(handled, "a click on a real row is handled");
            assert_eq!(l.take_events(), vec![ListEvent::Copy(id)]);
            assert_eq!(l.selected(), Some(id), "copying also selects the row");
        }

        // Case `mouse_clicks_in_the_action_column_emit_reorder_and_delete`.
        {
            let mut l = list_with_demo();
            let id = l.account_at(0).unwrap().id;
            let r = l.row_rect(0);
            let layout = l.row_layout(r);
            let col_x = f64::midpoint(layout.actions_x0, layout.actions_x1);
            // Top third of the action column → move up.
            dispatch(&mut l, &cursor_at(Point::new(col_x, r.y0 + 4.0)));
            dispatch(&mut l, &left_press());
            assert_eq!(l.take_events(), vec![ListEvent::MoveUp(id)]);
            // Bottom third → delete.
            dispatch(&mut l, &cursor_at(Point::new(col_x, r.y1 - 4.0)));
            dispatch(&mut l, &left_press());
            assert_eq!(l.take_events(), vec![ListEvent::Delete(id)]);
        }

        // Case `wheel_scrolls_and_clamps_through_handle_event`.
        {
            let mut l = AccountList::new();
            // A viewport shorter than the content so scrolling is possible.
            l.set_viewport(Rect::new(0.0, 0.0, 600.0, 220.0));
            l.set_accounts(demo_accounts());
            assert!(l.max_scroll() > 0.0, "five rows overflow a 220px viewport");

            // The wheel handler only acts when the pointer is over the list.
            dispatch(&mut l, &cursor_at(Point::new(300.0, 110.0)));
            // Scroll far down: the offset clamps to the bottom, never past it.
            dispatch(&mut l, &wheel(-100.0));
            assert_eq!(
                l.scroll,
                l.max_scroll(),
                "scrolling down clamps to the bottom"
            );
            // Scroll far up: clamps to the top, never negative.
            dispatch(&mut l, &wheel(100.0));
            assert_eq!(l.scroll, 0.0, "scrolling up clamps to the top");
        }
    }

    // ---- pointer routing through the real `handle_event` ----------------
    // Hit-testing is proven above via `hit()`; these drive real synthetic
    // pointer events through `AccountList::handle_event` so the
    // MouseInput→ListEvent emission and the wheel-scroll clamp are verified
    // through the production entry point, not the internal hit-test alone.

    use crate::test_events::{cursor_at, left_press, no_mods, wheel};

    fn dispatch(l: &mut AccountList, ev: &WindowEvent) -> bool {
        let mut doc = AutoCommit::new();
        let root = automerge::ROOT;
        let mods = no_mods();
        let mut taffy: TaffyTree<()> = TaffyTree::new();
        let node = taffy.new_leaf(Style::default()).unwrap();
        let mut handled = false;
        l.handle_event(
            ev,
            &mut ui::widget::EventCtx {
                scale_factor: 1.0,
                modifiers: &mods,
                state: &mut doc,
                path: &root,
                handled: &mut handled,
                focus_target: &mut None,
                node_id: node,
                taffy: &taffy,
            },
        );
        handled
    }
}
