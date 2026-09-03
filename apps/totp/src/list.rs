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

use automerge::{AutoCommit, ObjId};
use taffy::{Dimension, NodeId, Size, Style, TaffyTree};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, RoundedRect};
use vello::peniko::{BlendMode, Fill};
use winit::event::{ElementState, MouseButton, MouseScrollDelta, WindowEvent};

use ui::color_compat::ColorCompat;
use ui::presence::PresenceCtx;
use ui::text_render::{TextRenderer, TextWeight};
use ui::theme::Theme;
use ui::widget::{Avatar, Widget};

use crate::model::{Account, AccountId, account_color};
use crate::ring::CountdownRing;
use crate::totp;

const ROW_HEIGHT: f64 = 76.0;
const ROW_GAP: f64 = 10.0;
const PAD: f64 = 16.0;
const AVATAR_R: f64 = 24.0;
const RING_R: f64 = 22.0;
const ACTION_W: f64 = 26.0;
const COPY_ZONE_W: f64 = 210.0;

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
        self.accounts.len() as f64 * (ROW_HEIGHT + ROW_GAP)
    }

    fn max_scroll(&self) -> f64 {
        (self.content_height() - self.rect.height()).max(0.0)
    }

    fn clamp_scroll(&mut self) {
        self.scroll = self.scroll.clamp(0.0, self.max_scroll());
    }

    /// Bounding rect of visible row `i` (in absolute coords, scrolled).
    fn row_rect(&self, i: usize) -> Rect {
        let y0 = self.rect.y0 + ROW_GAP / 2.0 + i as f64 * (ROW_HEIGHT + ROW_GAP) - self.scroll;
        Rect::new(self.rect.x0 + PAD, y0, self.rect.x1 - PAD, y0 + ROW_HEIGHT)
    }

    /// Geometry landmarks within a row, derived once and shared by draw +
    /// hit-test so they never drift.
    fn row_layout(r: Rect) -> RowLayout {
        let actions_x1 = r.x1 - 12.0;
        let actions_x0 = actions_x1 - ACTION_W;
        let ring_center_x = actions_x0 - 16.0 - RING_R;
        let ring_center = Point::new(ring_center_x, r.center().y);
        let code_right = ring_center_x - RING_R - 18.0;
        let avatar_c = Point::new(r.x0 + 18.0 + AVATAR_R, r.center().y);
        let text_x = avatar_c.x + AVATAR_R + 16.0;
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
            let l = Self::row_layout(r);
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
            // Copy zone: the code area, just left of the ring.
            let copy_x0 = (l.code_right - COPY_ZONE_W).max(l.text_x);
            if p.x >= copy_x0 && p.x <= l.code_right + 8.0 {
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
        account: &Account,
    ) {
        // The account's colour is user-assignable, so it's kept; the disc +
        // centred monogram are painted by the shared toolkit avatar so the
        // shape/placement match every other app's roster.
        let color = account_color(theme, account.color);
        let rect = Rect::new(
            center.x - AVATAR_R,
            center.y - AVATAR_R,
            center.x + AVATAR_R,
            center.y + AVATAR_R,
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
        let l = Self::row_layout(r);
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
        Self::draw_avatar(scene, transform, text_renderer, theme, l.avatar_c, &account);

        // Issuer + label, vertically centred as a block. The available
        // text width shrinks with the row so a narrow window truncates
        // rather than colliding with the code.
        let text_w = (l.code_right - COPY_ZONE_W - l.text_x).max(60.0) as f32;
        let issuer = account.title().to_string();
        let has_label = !account.label.trim().is_empty();
        let issuer_y = if has_label {
            r.center().y - 20.0
        } else {
            r.center().y - 10.0
        };
        text_renderer.draw_text_weighted(
            scene,
            &issuer,
            16.0,
            theme.tokens.colors.text_primary,
            transform * Affine::translate((l.text_x, issuer_y)),
            Some(text_w),
            TextWeight::Semibold,
        );
        if has_label {
            text_renderer.draw_text(
                scene,
                &account.label,
                13.0,
                theme.tokens.colors.text_dim,
                transform * Affine::translate((l.text_x, r.center().y + 4.0)),
                Some(text_w),
            );
        }

        // The big rotating code, right-aligned to `code_right`.
        let raw = totp::code_at(&account, now);
        let grouped = totp::group_code(&raw);
        let code_size = 30.0;
        let (code_w, code_h) =
            text_renderer.measure_styled(&grouped, code_size, None, TextWeight::Bold, false, true);
        let copy_hot = is_hovered && self.hover_zone == Some(Zone::Copy);
        let code_color = if copy_hot {
            account_color(theme, account.color)
        } else {
            theme.tokens.colors.text_primary
        };
        let code_x = l.code_right - f64::from(code_w);
        let code_y = r.center().y - f64::from(code_h) / 2.0;
        text_renderer.draw_text_styled(
            scene,
            &grouped,
            code_size,
            code_color,
            transform * Affine::translate((code_x, code_y)),
            None,
            TextWeight::Bold,
            false,
            true,
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

        // Countdown ring.
        let frac = totp::period_progress(&account, now);
        let secs = totp::seconds_remaining(&account, now);
        self.ring
            .place(l.ring_c, RING_R)
            .set_fraction(frac)
            .set_seconds(secs)
            .set_color(account_color(theme, account.color));
        self.ring.draw_arc(scene, transform, text_renderer, theme);

        // Hover action icons.
        if is_hovered {
            Self::draw_action_icons(scene, transform, theme, &l, r, hovered_zone);
        }
    }

    /// Paint the row background card, tinted for selection/hover, plus the
    /// accent left-edge marker on the selected row. Split out of
    /// [`Self::draw_row`] to keep that function's line count in check.
    fn draw_row_card(
        scene: &mut Scene,
        transform: Affine,
        theme: &Theme,
        r: Rect,
        account: &Account,
        is_selected: bool,
        is_hovered: bool,
    ) {
        let card = RoundedRect::from_rect(r, f64::from(theme.tokens.radii.lg));
        let bg = if is_selected {
            theme.tokens.colors.lift(0.72)
        } else if is_hovered {
            theme.tokens.colors.lift(0.35)
        } else {
            theme.tokens.colors.surfaces.raised.with_alpha_factor(0.55)
        };
        scene.fill(Fill::NonZero, transform, bg, None, &card);
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

fn stroke(w: f64) -> vello::kurbo::Stroke {
    vello::kurbo::Stroke::new(w)
        .with_caps(vello::kurbo::Cap::Round)
        .with_join(vello::kurbo::Join::Round)
}

impl Widget for AccountList {
    fn sync(&mut self, _state: &mut AutoCommit, _path: &ObjId) {}

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
        let now = totp::unix_now();
        // Clip to the viewport so scrolled rows don't bleed into chrome.
        let clip = RoundedRect::from_rect(self.rect, 0.0);
        scene.push_layer(Fill::NonZero, BlendMode::default(), 1.0, transform, &clip);
        for i in 0..self.accounts.len() {
            self.draw_row(scene, transform, text_renderer, theme, i, now);
        }
        scene.pop_layer();
    }

    fn handle_event(
        &mut self,
        event: &WindowEvent,
        scale_factor: f64,
        _modifiers: &winit::event::Modifiers,
        _state: &mut AutoCommit,
        _path: &ObjId,
        handled: &mut bool,
        _focus_target: &mut Option<NodeId>,
        _node_id: NodeId,
        _taffy: &TaffyTree<()>,
    ) {
        match event {
            WindowEvent::CursorMoved { position, .. } => {
                let p = Point::new(position.x / scale_factor, position.y / scale_factor);
                self.last_mouse = p;
                let hit = self.hit(p);
                let (row, zone) = match hit {
                    Some((r, z)) => (Some(r), Some(z)),
                    None => (None, None),
                };
                if row != self.hover_row || zone != self.hover_zone {
                    self.hover_row = row;
                    self.hover_zone = zone;
                    *handled = true;
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
                    *handled = true;
                }
            }
            WindowEvent::MouseWheel { delta, .. }
                if self.rect.contains(self.last_mouse) && self.max_scroll() > 0.0 =>
            {
                let dy = match delta {
                    MouseScrollDelta::LineDelta(_, y) => f64::from(*y) * 48.0,
                    MouseScrollDelta::PixelDelta(pos) => pos.y / scale_factor,
                };
                self.scroll = (self.scroll - dy).clamp(0.0, self.max_scroll());
                *handled = true;
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

    fn list_with_demo() -> AccountList {
        let mut l = AccountList::new();
        l.set_viewport(Rect::new(0.0, 0.0, 600.0, 800.0));
        l.set_accounts(demo_accounts());
        l
    }

    #[test]
    fn rows_stack_without_overlap() {
        let l = list_with_demo();
        let r0 = l.row_rect(0);
        let r1 = l.row_rect(1);
        assert!(r1.y0 >= r0.y1, "row 1 must start at/after row 0 ends");
    }

    #[test]
    fn copy_zone_and_action_zones_hit_test_correctly() {
        let l = list_with_demo();
        let r = l.row_rect(0);
        let layout = AccountList::row_layout(r);
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

    #[test]
    fn empty_list_reports_empty() {
        let mut l = AccountList::new();
        l.set_viewport(Rect::new(0.0, 0.0, 600.0, 800.0));
        assert!(l.is_empty());
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
            1.0,
            &mods,
            &mut doc,
            &root,
            &mut handled,
            &mut None,
            node,
            &taffy,
        );
        handled
    }

    #[test]
    fn mouse_click_in_the_copy_zone_emits_a_copy_event() {
        let mut l = list_with_demo();
        let id = l.account_at(0).unwrap().id;
        let r = l.row_rect(0);
        let layout = AccountList::row_layout(r);
        let copy_p = Point::new(layout.code_right - 20.0, r.center().y);
        dispatch(&mut l, &cursor_at(copy_p));
        let handled = dispatch(&mut l, &left_press());
        assert!(handled, "a click on a real row is handled");
        assert_eq!(l.take_events(), vec![ListEvent::Copy(id)]);
        assert_eq!(l.selected(), Some(id), "copying also selects the row");
    }

    #[test]
    fn mouse_clicks_in_the_action_column_emit_reorder_and_delete() {
        let mut l = list_with_demo();
        let id = l.account_at(0).unwrap().id;
        let r = l.row_rect(0);
        let layout = AccountList::row_layout(r);
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

    #[test]
    fn wheel_scrolls_and_clamps_through_handle_event() {
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
