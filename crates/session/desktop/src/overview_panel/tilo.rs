//! The rows below the workspaces in the overview of the person's Tilo
//! ([`crate::overview_tilo`]): where they stand in the list, how they are
//! drawn, what a press on one or its control chooses, and how assistive
//! technology reads them.
//!
//! They follow the "go to" list in the same slots, so they scroll with it:
//! a section's name takes a slot of its own, and a row is a list row with
//! its detail under its title and, when it has one, its control at the
//! trailing edge, where a site workspace's Leave stands.

use ui::TextRole;
use ui::a11y::{AccessNode, AccessState, Role};
use ui::num::f32_of_f64;
use ui::widget::{Button, ButtonFace, Interaction, ListRowFace, RowLeading};
use vello::kurbo::{Point, Rect};

use super::OverviewPanel;
use crate::overview_panel::OverviewPick;
use crate::overview_tilo::{TiloItem, TiloRow};

/// Wide enough for "Manage billing" in every language the desktop speaks.
const ACTION_WIDTH: f64 = 148.0;
const ACTION_HEIGHT: f64 = 28.0;

impl OverviewPanel {
    /// The slots of the "go to" list before the rows: the other workspaces,
    /// New workspace and Join.
    pub(super) fn go_to_slots(&self) -> usize {
        let workspaces = self
            .desk
            .cards
            .len()
            .saturating_sub(usize::from(self.here_index().is_some()));
        workspaces
            .saturating_add(usize::from(self.can_add()))
            .saturating_add(usize::from(self.desk.can_join))
    }

    /// How many slots the rows take.
    pub(super) fn tilo_slots(&self) -> usize {
        self.tilo_rows.len()
    }

    /// Row `index`'s slot.
    fn tilo_rect(&self, index: usize) -> Rect {
        self.list_slot_rect(self.go_to_slots().saturating_add(index))
    }

    fn tilo_item(&self, index: usize) -> Option<&TiloItem> {
        match self.tilo_rows.get(index)? {
            TiloRow::Item(item) => Some(item),
            TiloRow::Caption(_) => None,
        }
    }

    /// Row `index`'s control, at its trailing edge.
    fn tilo_action_rect(&self, index: usize) -> Option<Rect> {
        self.tilo_item(index)?.action.as_ref()?;
        let row = self.tilo_rect(index);
        let pad = self.metrics.row_pad;
        let h = ACTION_HEIGHT.min(row.height() - 8.0).max(0.0);
        let cy = row.center().y;
        Some(Rect::new(
            row.x1 - pad - ACTION_WIDTH,
            cy - h / 2.0,
            row.x1 - pad,
            cy + h / 2.0,
        ))
    }

    /// The row under `p`, where it can be seen, and whether `p` is on its
    /// control.
    pub(super) fn tilo_hit(&self, p: Point) -> Option<(usize, bool)> {
        let band = self.list_band();
        if !band.contains(p) {
            return None;
        }
        (0..self.tilo_rows.len()).find_map(|index| {
            self.tilo_item(index)?;
            if self
                .tilo_action_rect(index)
                .is_some_and(|rect| rect.contains(p))
            {
                return Some((index, true));
            }
            self.tilo_rect(index).contains(p).then_some((index, false))
        })
    }

    /// What a press on row `index`, or on its control, chooses: nothing for
    /// a row that only says something.
    pub(super) fn tilo_pick(&self, index: usize, on_action: bool) -> Option<OverviewPick> {
        let item = self.tilo_item(index)?;
        if on_action {
            item.action.as_ref().map(|(_, pick)| pick.clone())
        } else {
            item.press.clone()
        }
    }

    /// What the semantic target `target` names among the rows: a row's
    /// key presses it, `<key>:action` its control.
    pub(super) fn tilo_target(&self, target: &str) -> Option<OverviewPick> {
        let (key, action) = match target.strip_suffix(":action") {
            Some(key) => (key, true),
            None => (target, false),
        };
        let item = self.tilo_rows.iter().find_map(|row| match row {
            TiloRow::Item(item) if item.key == key => Some(item),
            _ => None,
        })?;
        if action {
            item.action.as_ref().map(|(_, pick)| pick.clone())
        } else {
            item.press.clone()
        }
    }

    /// Draw the rows that stand in `band`.
    pub(super) fn paint_tilo_rows(&self, ctx: &mut ui::paint::PaintCtx<'_>, band: Rect) {
        for (index, row) in self.tilo_rows.iter().enumerate() {
            let rect = self.tilo_rect(index);
            if rect.y1 < band.y0 || rect.y0 > band.y1 {
                continue;
            }
            match row {
                TiloRow::Caption(caption) => self.paint_tilo_caption(ctx, rect, caption),
                TiloRow::Item(item) => self.paint_tilo_item(ctx, index, rect, item),
            }
        }
    }

    /// A section's name, at the foot of its slot, as the list's own caption.
    fn paint_tilo_caption(&self, ctx: &mut ui::paint::PaintCtx<'_>, rect: Rect, caption: &str) {
        let tokens = &ctx.theme.tokens;
        let style = tokens.typography.style(TextRole::Caption);
        let caption = ctx
            .text
            .elide_to_width_style(caption, f32_of_f64(rect.width()), &style);
        let (_, h) = ctx.text.measure_style(&caption, None, &style);
        ctx.text.draw_style(
            ctx.scene,
            &caption,
            tokens.colors.text_tertiary,
            ctx.transform
                .pre_translate((rect.x0, rect.y1 - f64::from(h) - self.metrics.s1).into()),
            None,
            &style,
        );
    }

    fn paint_tilo_item(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        index: usize,
        rect: Rect,
        item: &TiloItem,
    ) {
        let action = self.tilo_action_rect(index);
        let mut face = ListRowFace::new(&item.title);
        if let Some(detail) = &item.detail {
            face = face.secondary(detail);
        }
        if let Some(icon) = item.icon {
            face = face.leading(RowLeading::Icon(icon));
        }
        let mut face = face.interaction(Interaction {
            hovered: item.press.is_some()
                && self.cursor.is_some_and(|cursor| rect.contains(cursor)),
            ..Interaction::default()
        });
        if let Some(action) = action {
            face = face.trailing_width(action.width() + self.metrics.s3);
        }
        face.paint(ctx, rect);
        if let (Some(rect), Some((label, _))) = (action, item.action.as_ref()) {
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &ButtonFace::new(rect, label)
                    .hovered(self.cursor.is_some_and(|p| rect.contains(p))),
            );
        }
    }

    /// The rows as assistive technology reads them: each section a group of
    /// its rows, each row a button when it does something, its control a
    /// button of its own.
    pub(super) fn tilo_accessibility(&self) -> Vec<AccessNode> {
        let mut nodes: Vec<AccessNode> = Vec::new();
        for (index, row) in self.tilo_rows.iter().enumerate() {
            let rect = self.tilo_rect(index);
            match row {
                TiloRow::Caption(caption) => nodes.push(
                    AccessNode::new(Role::Group)
                        .with_hashed_key(caption)
                        .with_name(caption.clone())
                        .with_bounds(rect),
                ),
                TiloRow::Item(item) => {
                    let role = if item.press.is_some() {
                        Role::Button
                    } else {
                        Role::ListItem
                    };
                    let mut node = AccessNode::new(role)
                        .with_action_target(item.key.clone())
                        .with_name(item.title.clone())
                        .with_bounds(rect)
                        .with_state(AccessState::new().focusable(item.press.is_some()));
                    if let Some(detail) = &item.detail {
                        node = node.with_description(detail.clone());
                    }
                    if let (Some((label, _)), Some(bounds)) =
                        (&item.action, self.tilo_action_rect(index))
                    {
                        node.children.push(
                            AccessNode::new(Role::Button)
                                .with_action_target(format!("{}:action", item.key))
                                .with_name(format!("{label}: {}", item.title))
                                .with_bounds(bounds)
                                .with_state(AccessState::new().focusable(true)),
                        );
                    }
                    match nodes.last_mut() {
                        Some(group) if group.role == Role::Group => group.children.push(node),
                        _ => nodes.push(node),
                    }
                }
            }
        }
        nodes
    }

    /// Every row and control a script presses, by its key.
    pub(super) fn tilo_test_rects(&self) -> Vec<(String, Rect)> {
        let mut rects = Vec::new();
        for (index, row) in self.tilo_rows.iter().enumerate() {
            if let TiloRow::Item(item) = row {
                rects.push((item.key.clone(), self.tilo_rect(index)));
                if let Some(action) = self.tilo_action_rect(index) {
                    rects.push((format!("{}:action", item.key), action));
                }
            }
        }
        rects
    }
}
