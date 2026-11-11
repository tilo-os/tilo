//! Windows floating above the desk: a strip's window lifted out of its flow
//! and shown at a rect of its own, over the other strips and windows.
//!
//! A float is the same window the strip holds — its group, tabs and hosts
//! stay where they were, and only where it is laid out changes. It is laid
//! out absolutely at its rect, so it is drawn, hit-tested and reported where
//! it stands on screen, above everything the row draws in its strip; the row
//! draws it in a pass of its own, after the strips, unclipped.
//!
//! Snapping it back is the reverse: the window leaves the float set and is
//! laid out in its strip again, at the gap it was dropped in.

use std::collections::HashMap;

use taffy::{Dimension, LengthPercentageAuto, Position, Size, Style};
use vello::Scene;
use vello::kurbo::{Affine, Point, Rect, Vec2};

use super::GroupRow;
use crate::num::f32_of_f64;
use crate::presence::PresenceCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::tokens::Spacing;
use crate::widget::Widget as _;
use crate::widget::floating_surface::card_radius;
use crate::widget::primitives::paint_floating_window_card;

/// The size a window takes when it floats: the design's floating window,
/// smaller than the strip's tile so it reads as lifted over the desk.
pub const FLOAT_SIZE: (f64, f64) = (640.0, 480.0);
/// The size a conversation sent from a window floats at: a card as narrow as a list panel, beside
/// the window it was sent from rather than over it, as the activity cards stand over the desk.
pub const FLOAT_CARD_SIZE: (f64, f64) = (360.0, 520.0);
/// The pointer's room above a floating window's bar where it is let go: the
/// window hangs below and right of the hand, as a carried window does.
const FLOAT_GRAB: (f64, f64) = (80.0, 14.0);
/// The room a float's card keeps inside the desk on every side: the band of
/// the card's own ground round the window and the focus ring drawn outside
/// that. A float placed to the desk's edge stands this far in, and the strip
/// makes room for it beside a neighbour, so the whole card it draws — ring and
/// band — shows on the desk instead of under the bar above it, past the
/// display, or over the window it opened beside. One step of the spacing
/// scale, as a card's inset is everywhere else.
pub const FLOAT_CARD_ROOM: f64 = Spacing::BASE.s2 as f64;

/// Where a float the person moved or let go goes, for the host to keep in
/// the workspace's record.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FloatAsk {
    /// Let go off the strip: the window floats at this rect, in the
    /// surface's coordinates.
    Float { group: u64, at: Rect },
    /// Let go over the strip: the window snaps back into it at the gap it
    /// was dropped in (the row has already moved it there), and its float
    /// place goes.
    Snap { group: u64 },
    /// The card's collapse was pressed: it folds to its header, or opens from it again.
    Collapse { group: u64 },
}

impl GroupRow {
    /// Show the windows of `floats` at their rects, in the surface's
    /// coordinates, and every other window in its strip. A window that
    /// leaves the set is back in the strip.
    pub fn set_floats(&mut self, mut floats: HashMap<u64, Rect>) {
        // A float the hand is carrying owns where it stands until it is let
        // go: the record catches up then, not under the hand.
        let carried = self
            .group_drag
            .filter(|drag| drag.moved)
            .map(|drag| drag.group_id)
            .and_then(|id| self.floats.get(&id).map(|at| (id, *at)));
        if let Some((id, at)) = carried
            && floats.contains_key(&id)
        {
            floats.insert(id, at);
        }
        if self.floats == floats {
            return;
        }
        let now = crate::motion::frame_now();
        let motion = crate::tokens::EffectiveMotion::new(self.motion, self.reduce_motion);
        self.float_fade.retain(|id, _| floats.contains_key(id));
        for id in floats.keys() {
            self.float_fade.entry(*id).or_insert_with(|| {
                let mut fade = crate::motion::Presence::closed();
                fade.show(
                    true,
                    crate::motion::WindowMotion::SIBLINGS,
                    motion,
                    self.motion_source,
                    now,
                );
                fade
            });
        }
        let back: Vec<u64> = self
            .floats
            .keys()
            .filter(|id| !floats.contains_key(id))
            .copied()
            .collect();
        self.floats = floats;
        self.structure_dirty = true;
        // A window back in its strip takes its share of it again: the one
        // that stayed grew into the room the float left, so the two tile
        // the strip rather than one running off its edge. Every window's
        // width stands where it now belongs from this frame, rather than
        // sliding there while the strip shows one of them over the other.
        if !back.is_empty() {
            for id in back {
                self.share_with_neighbour_at_once(id);
                self.settle_widths.push(id);
            }
            self.structure_dirty = true;
        }
    }

    /// The windows floating above the desk, by group id, at their rects.
    #[must_use]
    pub fn floats(&self) -> &HashMap<u64, Rect> {
        &self.floats
    }

    /// Each floating window the row just drew whole over what is under it — the card it shows: its
    /// bar and its body as one window, and the shadow that lifts it, as one rect in the surface's
    /// coordinates. A float still fading in lets what is under it show, so it is not one yet.
    #[must_use]
    pub fn drawn_covers(&self) -> &[(u64, Rect)] {
        &self.drawn_covers
    }

    /// Whether the group `id` floats above the desk.
    #[must_use]
    pub fn is_floating(&self, id: u64) -> bool {
        self.floats.contains_key(&id)
    }

    /// Each of the row's groups in its order, whether it floats above the
    /// desk rather than being laid out in its strip.
    pub(super) fn floating_flags(&self) -> Vec<bool> {
        self.groups
            .iter()
            .map(|group| self.floats.contains_key(&group.id))
            .collect()
    }

    /// The floating window whose rect holds screen point `p`, the one on top
    /// first.
    #[must_use]
    pub fn float_group_at(&self, p: Point) -> Option<u64> {
        self.groups
            .iter()
            .rev()
            .find(|group| self.floats.contains_key(&group.id) && group.get_rect().contains(p))
            .map(|group| group.id)
    }

    /// Where a float the person moved or let go goes, once: drained by the
    /// host and kept in the workspace's record.
    /// A floating window whose snap control was pressed goes back into its strip where it stood.
    pub fn take_pending_float(&mut self) -> Option<FloatAsk> {
        self.pending_float.take().or_else(|| {
            self.groups.iter_mut().find_map(|group| {
                if group.take_pending_snap() {
                    Some(FloatAsk::Snap { group: group.id })
                } else {
                    group
                        .take_pending_collapse()
                        .then_some(FloatAsk::Collapse { group: group.id })
                }
            })
        })
    }

    /// Tell each window whether it floats, so its controls offer what a float does.
    pub(super) fn mark_floating(&mut self) {
        for group in &mut self.groups {
            group.set_floating(self.floats.contains_key(&group.id));
            group.set_collapsed(self.collapsed_cards.contains(&group.id));
        }
    }

    /// Tell the row which of its floating cards the desk has folded to their header, so their
    /// collapse controls point the way they fold them ([`FloatAsk::Collapse`]).
    pub fn set_collapsed(&mut self, collapsed: &std::collections::BTreeSet<u64>) {
        let collapsed: std::collections::HashSet<u64> = collapsed.iter().copied().collect();
        if collapsed == self.collapsed_cards {
            return;
        }
        self.collapsed_cards = collapsed;
        self.structure_dirty = true;
        for group in &mut self.groups {
            group.set_collapsed(self.collapsed_cards.contains(&group.id));
        }
    }

    /// The style of the window at `i` when it floats, over its `own`: absolute
    /// at the float size, placed by [`Self::apply_float_layout`] at the rect
    /// itself. `None` for a window the strip lays out.
    pub(super) fn float_node_style(&self, i: usize, own: &Style) -> Option<Style> {
        let id = self.groups.get(i)?.id;
        let at = self.floats.get(&id)?;
        Some(Style {
            position: Position::Absolute,
            inset: taffy::Rect {
                left: LengthPercentageAuto::length(0.0),
                top: LengthPercentageAuto::length(0.0),
                right: LengthPercentageAuto::auto(),
                bottom: LengthPercentageAuto::auto(),
            },
            size: Size {
                width: Dimension::length(f32_of_f64(at.width())),
                height: Dimension::length(f32_of_f64(at.height())),
            },
            min_size: Size::auto(),
            max_size: Size::auto(),
            margin: taffy::Rect::zero(),
            flex_grow: 0.0,
            flex_shrink: 0.0,
            ..own.clone()
        })
    }

    /// Lay the floating windows out at their rects: each is absolutely
    /// placed, so its laid-out rect is its float's origin and size.
    pub(super) fn apply_float_layout(&mut self, taffy: &taffy::TaffyTree<()>) {
        let placed: Vec<(u64, Rect)> = self
            .groups
            .iter()
            .filter_map(|group| Some((group.id, *self.floats.get(&group.id)?)))
            .collect();
        if placed.is_empty() {
            return;
        }
        for (group, node) in self.groups.iter_mut().zip(self.node_ids.iter()) {
            let Some((_, at)) = placed.iter().find(|(id, _)| *id == group.id) else {
                continue;
            };
            group.apply_layout(taffy, *node, at.origin());
        }
    }

    /// The hand carrying the floating window `id` has moved `by` since the
    /// drag took it: the window follows, and the strip says where it would
    /// land when it is over it.
    pub(super) fn carry_float(&mut self, id: u64, by: Vec2) {
        let Some(place) = self.floats.get(&id).copied() else {
            return;
        };
        let from = *self.float_drag_from.get_or_insert(place);
        let moved = from + by;
        if moved != place {
            self.floats.insert(id, moved);
            self.structure_dirty = true;
        }
    }

    /// The index of the floating window the pointer is over, the one drawn
    /// on top first: the window the hand reaches, not the strip under it.
    pub(super) fn float_under_cursor(&self) -> Option<usize> {
        self.groups
            .iter()
            .enumerate()
            .rfind(|(i, group)| {
                !self.layout_hides_group(*i)
                    && self.floats.contains_key(&group.id)
                    && group.get_rect().contains(self.last_cursor)
            })
            .map(|(i, _)| i)
    }

    /// A floating window dragged by its bar: it follows the hand, and over
    /// the strip the gap it would land in shows as any reorder drag's does.
    pub(super) fn carry_float_drag(&mut self, id: u64, start: Point) {
        self.carry_float(id, self.last_cursor - start);
        if self.covers(self.last_cursor) {
            let (target, lane) = self.group_insertion_for(self.last_cursor);
            self.group_reorder_target = Some(target);
            self.group_reorder_lane = lane;
        } else {
            self.group_reorder_target = None;
            self.group_reorder_lane = None;
        }
    }

    /// The hand let a window of the strip go where the strip is not: it
    /// floats where the hand let it go.
    pub(super) fn lift_from_drag(&mut self, id: u64) {
        let at = self.float_rect_at(self.last_cursor);
        self.pending_float = Some(FloatAsk::Float { group: id, at });
        self.group_reorder_target = None;
        self.group_reorder_lane = None;
    }

    /// The hand let the floating window `id` go: over the strip it snaps
    /// back into the gap it was dropped in, else it stays where it is.
    /// `Some` when the release claims the gesture.
    pub(super) fn put_down_float(&mut self, id: u64, keep: bool) -> bool {
        let Some(from) = self.float_drag_from.take() else {
            return false;
        };
        if !keep {
            self.floats.insert(id, from);
            self.structure_dirty = true;
            return true;
        }
        if self.covers(self.last_cursor) {
            let (insert_at, _) = self.group_insertion_for(self.last_cursor);
            if let Some(src) = self.groups.iter().position(|group| group.id == id) {
                self.commit_group_reorder(src, id, insert_at, None);
            }
            self.pending_float = Some(FloatAsk::Snap { group: id });
        } else {
            // Where the hand carried it, at the size it floats at: a card stays a card.
            let carried = self.floats.get(&id).copied().unwrap_or(from);
            let at = self.keep_float_inside(carried);
            self.floats.insert(id, at);
            self.structure_dirty = true;
            self.pending_float = Some(FloatAsk::Float { group: id, at });
        }
        true
    }

    /// The rect a window let go at `p` floats in: the floating size, hung
    /// from the hand, kept inside the row's display with the room its card
    /// draws in.
    #[must_use]
    pub fn float_rect_at(&self, p: Point) -> Rect {
        let at = Rect::from_origin_size(
            Point::new(p.x - FLOAT_GRAB.0, p.y - FLOAT_GRAB.1),
            (FLOAT_SIZE.0, FLOAT_SIZE.1),
        );
        self.keep_float_inside(at)
    }

    /// `at` kept inside the desk the row stands on ([`keep_float_inside`]).
    #[must_use]
    pub fn keep_float_inside(&self, at: Rect) -> Rect {
        keep_float_inside(self.rect, at)
    }

    /// Whether a floating window's fading in asks for another frame.
    pub(super) fn float_fade_needs_frame(&self) -> bool {
        let now = crate::motion::frame_now();
        self.float_fade
            .values()
            .any(|fade| fade.is_moving(now) && fade.is_open())
    }

    /// Draw the floating windows above everything the row draws in its
    /// strip: unclipped, in their own pass, the one dragged last.
    pub(super) fn draw_floats(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        (tr, theme, presence): (&mut TextRenderer, &Theme, &PresenceCtx),
    ) {
        self.drawn_covers.clear();
        let now = crate::motion::frame_now();
        let floated: Vec<(usize, f32)> = (0..self.groups.len())
            .filter_map(|i| {
                let id = self.groups.get(i)?.id;
                let fade = self.float_fade.get(&id)?;
                Some((i, crate::motion::unit_opacity(fade.progress(now))))
            })
            .collect();
        for (i, alpha) in floated {
            if alpha <= f32::EPSILON {
                continue;
            }
            // A float is a card over the desk, as every card the floating layer stacks is: its bar
            // is the card's header and the window its body, in the card's own material, radius,
            // hairline and lift — no window outline or focus ring round it, as a song's card has
            // none.
            let card = self.groups[i].get_rect().union(self.groups[i].bar_rect());
            let radius = card_radius(card, theme);
            if alpha < 1.0 - f32::EPSILON {
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    alpha,
                    transform,
                    &card.inflate(160.0, 160.0),
                );
            } else {
                // Drawn whole: the windows under it are drawn around its lift, so none of them reads
                // through its bar or its body.
                let tab = self.groups[i].active_tab_id();
                self.drawn_covers
                    .push((tab, crate::surface::floating_shadow_bounds(card, theme)));
            }
            self.groups[i].set_edge_by_card(true);
            paint_floating_window_card(scene, transform, card, theme, |scene, on| {
                let shape = vello::kurbo::RoundedRect::from_rect(card, radius);
                scene.push_layer(
                    vello::peniko::Fill::NonZero,
                    vello::peniko::BlendMode::default(),
                    1.0,
                    transform,
                    &shape,
                );
                self.draw_group_plain(i, scene, transform, (tr, on, presence));
                scene.pop_layer();
            });
            self.groups[i].set_edge_by_card(false);
            if alpha < 1.0 - f32::EPSILON {
                scene.pop_layer();
            }
        }
    }
}

/// `at` kept inside the desk `area`, clear of its edge by [`FLOAT_CARD_ROOM`]: the card a float
/// draws round its window — the band of its own ground and the focus ring outside it — stays whole.
#[must_use]
pub fn keep_float_inside(area: Rect, at: Rect) -> Rect {
    let room = FLOAT_CARD_ROOM;
    let x = at
        .x0
        .min(area.x1 - room - at.width())
        .max(area.x0 + room)
        .max(0.0);
    let y = at
        .y0
        .min(area.y1 - room - at.height())
        .max(area.y0 + room)
        .max(0.0);
    Rect::from_origin_size(Point::new(x, y), at.size())
}
