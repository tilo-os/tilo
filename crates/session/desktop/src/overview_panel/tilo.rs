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
use ui::widget::{
    Button, ButtonFace, ButtonRole, Interaction, ListRowFace, RowLeading, TextBlockFace,
    TextLineFace,
};
use vello::kurbo::{Point, Rect};

use super::OverviewPanel;
use crate::i18n::t;
use crate::overview_panel::OverviewPick;
use crate::overview_tilo::{TiloFace, TiloItem, TiloMark, TiloRow};

/// The controls' width before they were first measured: enough for "Manage billing" in every
/// language the desktop speaks.
const ACTION_WIDTH: f64 = 148.0;
const ACTION_HEIGHT: f64 = 28.0;

/// A face at a row's leading edge, drawn by presence so it looks as that person or agent does in
/// every stack: a person's circle or an agent's rounded square, in the colour it wears, on the
/// panel it stands on.
fn paint_face(ctx: &mut ui::paint::PaintCtx<'_>, slot: Rect, worn: &TiloFace) {
    let face = ui::presence::Face {
        is_agent: worn.is_agent,
        ..ui::presence::Face::new(
            &worn.name,
            &worn.identity,
            ui::presence::presence_colour(worn.colour),
        )
    };
    face.paint(
        ctx,
        ui::presence::Face::disc_in(slot),
        ui::widget::SurfaceLevel::Floating.color(ctx.theme),
    );
}

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

    /// The row with `key`, when the desk shows one.
    pub(super) fn tilo_row(&self, key: &str) -> Option<&TiloItem> {
        self.tilo_rows.iter().find_map(|row| match row {
            TiloRow::Item(item) if item.key == key => Some(item),
            _ => None,
        })
    }

    /// Whether `item`'s irreversible control is asking, so nothing is carried out until the
    /// person answers on its row.
    fn tilo_asks(&self, item: &TiloItem) -> bool {
        item.confirm.is_some() && self.tilo_asking.as_deref() == Some(item.key.as_str())
    }

    /// A row's height: a section's name stands in the gap that opens the section, a row is as
    /// tall as its two lines, and a row asking about an irreversible control keeps the question
    /// and its two answers under them.
    fn tilo_height(&self, index: usize) -> f64 {
        match self.tilo_rows.get(index) {
            Some(TiloRow::Caption(_)) => self.metrics.section_gap + self.metrics.caption_h,
            Some(TiloRow::Item(item)) => {
                self.metrics.tilo_row_h
                    + if self.tilo_asks(item) {
                        self.metrics.s2 + self.tilo_question_h + self.metrics.s2 + ACTION_HEIGHT
                    } else {
                        0.0
                    }
            }
            None => 0.0,
        }
    }

    /// Whether row `index` stands whole above the foot of `band`: a row the foot would cut is not
    /// shown, and a section's name only with its first row, so the list ends on a whole row above
    /// the scroll thumb that says there is more.
    fn tilo_shown(&self, index: usize, band: Rect) -> bool {
        let whole = |index: usize| self.tilo_rect(index).y1 <= band.y1 + 0.5;
        match self.tilo_rows.get(index) {
            Some(TiloRow::Caption(_)) => whole(index) && whole(index.saturating_add(1)),
            Some(TiloRow::Item(_)) => whole(index),
            None => false,
        }
    }

    /// How many of the list's slots the rows fill, so the list scrolls to the last of them.
    pub(super) fn tilo_slots(&self) -> usize {
        let m = self.metrics;
        let pitch = m.workspace_row_h + m.workspace_row_gap;
        let height: f64 = (0..self.tilo_rows.len())
            .map(|index| self.tilo_height(index))
            .sum();
        // Whole slots, rounded up: a fraction left over is air under the last row.
        if pitch <= 0.0 {
            return 0;
        }
        ui::num::usize_of_f64((height / pitch).ceil())
    }

    /// The column the rows stand in, whatever their heights: from the first row's place to the
    /// list's trailing edge.
    fn tilo_band(&self) -> Rect {
        self.list_slot_rect(self.go_to_slots())
    }

    /// Row `index`'s rect: below the "go to" list, after the rows before it.
    fn tilo_rect(&self, index: usize) -> Rect {
        let first = self.tilo_band();
        let above: f64 = (0..index).map(|before| self.tilo_height(before)).sum();
        let height = self.tilo_height(index);
        Rect::new(
            first.x0,
            first.y0 + above,
            first.x1,
            first.y0 + above + height,
        )
    }

    /// The row at `index`, when it is one that says something: how the pointer over it reads.
    pub(super) fn tilo_item(&self, index: usize) -> Option<&TiloItem> {
        match self.tilo_rows.get(index)? {
            TiloRow::Item(item) => Some(item),
            TiloRow::Caption(_) => None,
        }
    }

    /// Fit the controls to their widest label, so each says it whole and they stand in one
    /// column; and measure the question an irreversible control is asking, which wraps to the
    /// rows' width.
    pub(super) fn measure_tilo_actions(
        &mut self,
        text: &mut ui::text_render::TextRenderer,
        theme: &ui::theme::Theme,
    ) {
        self.tilo_action_width = self
            .tilo_rows
            .iter()
            .filter_map(|row| match row {
                TiloRow::Item(item) => item.action.as_ref(),
                TiloRow::Caption(_) => None,
            })
            .map(|(label, _)| ButtonFace::new(Rect::ZERO, label).intrinsic_width(text, theme))
            .fold(0.0, f64::max);
        let asking = self
            .tilo_asking
            .clone()
            .and_then(|key| self.tilo_row(&key).cloned())
            .and_then(|item| item.confirm);
        let style = theme.text(TextRole::Caption);
        self.tilo_question_h = asking.as_ref().map_or(0.0, |confirm| {
            TextBlockFace::new(&confirm.question, &style).height(
                text,
                (self.tilo_band().width() - 2.0 * self.metrics.row_pad).max(0.0),
            )
        });
        self.tilo_answers_w = asking.as_ref().map_or((0.0, 0.0), |confirm| {
            let mut width =
                |label: &str| ButtonFace::new(Rect::ZERO, label).intrinsic_width(text, theme);
            (width(&t("overview-tilo-keep")), width(&confirm.answer))
        });
        // A question makes its row taller than the one it stood on: a list whose foot would cut
        // it scrolls, so the person reads what they are asked and answers there.
        if asking.is_some() {
            self.scroll_asking_into_view();
        }
    }

    /// Scroll the list so the row asking is whole above its foot, where the scroll can reach that
    /// far.
    fn scroll_asking_into_view(&mut self) {
        let asking = (0..self.tilo_rows.len()).find(|&index| {
            self.tilo_item(index)
                .is_some_and(|item| self.tilo_asks(item))
        });
        let Some(index) = asking else {
            return;
        };
        let over = self.tilo_rect(index).y1 - self.list_band().y1;
        if over <= 0.5 {
            return;
        }
        let scrolled = (self.scroll_y + over).clamp(0.0, self.max_scroll_y());
        if (scrolled - self.scroll_y).abs() > f64::EPSILON {
            self.scroll_y = scrolled;
            self.bump();
        }
    }

    fn action_width(&self) -> f64 {
        if self.tilo_action_width > 0.0 {
            self.tilo_action_width
        } else {
            ACTION_WIDTH
        }
    }

    /// Row `index`'s control, at its trailing edge: none while it is asking, because the two
    /// answers stand in its place.
    fn tilo_action_rect(&self, index: usize) -> Option<Rect> {
        let item = self.tilo_item(index)?;
        item.action.as_ref()?;
        if self.tilo_asks(item) {
            return None;
        }
        let row = self.tilo_rect(index);
        let pad = self.metrics.row_pad;
        let h = ACTION_HEIGHT.min(row.height() - 8.0).max(0.0);
        let cy = row.center().y;
        Some(Rect::new(
            row.x1 - pad - self.action_width(),
            cy - h / 2.0,
            row.x1 - pad,
            cy + h / 2.0,
        ))
    }

    /// The question an asking row stands on, and its two answers: Keep, and the answer that goes
    /// ahead, at the row's trailing edge under what is lost.
    fn tilo_ask_rects(&self, index: usize) -> Option<(Rect, Rect, Rect)> {
        let item = self.tilo_item(index)?;
        if !self.tilo_asks(item) {
            return None;
        }
        let row = self.tilo_rect(index);
        let pad = self.metrics.row_pad;
        let question = Rect::new(
            row.x0 + pad,
            row.y0 + self.metrics.tilo_row_h + self.metrics.s2,
            row.x1 - pad,
            row.y0 + self.metrics.tilo_row_h + self.metrics.s2 + self.tilo_question_h,
        );
        let buttons = Rect::new(
            question.x0,
            question.y1 + self.metrics.s2,
            question.x1,
            question.y1 + self.metrics.s2 + ACTION_HEIGHT,
        );
        let (keep_w, go_w) = self.tilo_answers_w;
        // Keep and the answer stand at the trailing edge, the answer last: the one that goes
        // ahead never crowds the row's edge, however long its label is in a language.
        let keep = Rect::new(
            (buttons.x1 - go_w - self.metrics.s2 - keep_w).max(buttons.x0),
            buttons.y0,
            (buttons.x1 - go_w - self.metrics.s2).max(buttons.x0),
            buttons.y1,
        );
        let going = Rect::new(buttons.x1 - go_w, buttons.y0, buttons.x1, buttons.y1);
        Some((question, keep, going))
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
            if !self.tilo_shown(index, band) {
                return None;
            }
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
    ///
    /// An irreversible control asks first: its first press asks on the row and carries nothing
    /// out, Shift or not, because nothing takes a revocation back.
    pub(super) fn tilo_press(&mut self, index: usize, on_action: bool) {
        let Some(item) = self.tilo_item(index) else {
            return;
        };
        let (key, press, action, asks) = (
            item.key.clone(),
            item.press.clone(),
            item.action.clone(),
            item.confirm.is_some(),
        );
        if !on_action {
            if let Some(pick) = press {
                self.choose(pick);
            }
            return;
        }
        let Some((_, pick)) = action else {
            return;
        };
        if asks && self.tilo_asking.as_deref() != Some(key.as_str()) {
            self.tilo_asking = Some(key);
            self.bump();
        } else {
            self.choose(pick);
        }
    }

    /// Put a question away, keeping things as they are: what a press elsewhere, Escape and the
    /// answer Keep all do. Whether one was asking.
    pub(super) fn tilo_keep(&mut self) -> bool {
        let asking = self.tilo_asking.take().is_some();
        if asking {
            self.bump();
        }
        asking
    }

    /// A press at `p` while a row asks: its own two answers answer it, and a press anywhere else
    /// keeps things as they are and is spent there, so it cannot also act on what it landed on.
    /// Whether anything was asking.
    pub(super) fn tilo_asking_answer(&mut self, p: Point) -> bool {
        let Some(key) = self.tilo_asking.clone() else {
            return false;
        };
        let asking = (0..self.tilo_rows.len())
            .find(|&index| self.tilo_item(index).is_some_and(|item| item.key == key));
        let Some((_, _, going)) = asking.and_then(|index| self.tilo_ask_rects(index)) else {
            return false;
        };
        let pick = going
            .contains(p)
            .then(|| {
                self.tilo_row(&key)
                    .and_then(|item| item.action.as_ref())
                    .map(|(_, pick)| pick.clone())
            })
            .flatten();
        self.tilo_keep();
        if let Some(pick) = pick {
            self.choose(pick);
        }
        true
    }

    /// What the semantic target `target` names among the rows: a row's key presses it,
    /// `<key>:action` its control. What cannot be undone asks first: [`Self::activate`] puts the
    /// question before it hands this back.
    pub(crate) fn tilo_target(&self, target: &str) -> Option<OverviewPick> {
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
            if rect.y1 < band.y0 || !self.tilo_shown(index, band) {
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
        let line = TextLineFace::styled(caption, &style, TextRole::Caption).start();
        let (_, h) = line.measure(ctx.text);
        let band = Rect::new(
            rect.x0,
            rect.y1 - h - self.metrics.s1,
            rect.x1,
            rect.y1 - self.metrics.s1,
        );
        line.paint(ctx, band, tokens.colors.text_tertiary);
    }

    fn paint_tilo_item(
        &self,
        ctx: &mut ui::paint::PaintCtx<'_>,
        index: usize,
        rect: Rect,
        item: &TiloItem,
    ) {
        let action = self.tilo_action_rect(index);
        let asking = self.tilo_asks(item);
        let worn = match &item.mark {
            Some(TiloMark::Face(worn)) => Some(worn),
            _ => None,
        };
        let paint = |ctx: &mut ui::paint::PaintCtx<'_>, slot: Rect| {
            if let Some(worn) = worn {
                paint_face(ctx, slot, worn);
            }
        };
        let mut face = ListRowFace::new(&item.title);
        if !asking && !item.facts.is_empty() {
            face = face.facts(&item.facts);
        }
        let mut face = match &item.mark {
            Some(TiloMark::Icon(icon)) => face.leading(RowLeading::Icon(*icon)),
            Some(TiloMark::Face(_)) => face.leading(RowLeading::Paint(&paint)),
            None => face,
        }
        .interaction(Interaction {
            hovered: item.press.is_some()
                && self.cursor.is_some_and(|cursor| rect.contains(cursor)),
            ..Interaction::default()
        });
        if let Some(action) = action {
            face = face.trailing_width(action.width() + self.metrics.s3);
        }
        // A row that asks keeps its name where it stands, and its question under it: the name
        // is drawn in the room a row takes, not centred in the taller row the question makes.
        let head = if asking {
            Rect::new(rect.x0, rect.y0, rect.x1, rect.y0 + self.metrics.tilo_row_h)
        } else {
            rect
        };
        face.paint(ctx, head);
        if let Some((question, keep, going)) = self.tilo_ask_rects(index)
            && let Some(confirm) = &item.confirm
        {
            // What the person loses, under the name they are about to lose it from.
            let style = ctx.theme.text(TextRole::Caption);
            TextBlockFace::new(&confirm.question, &style).paint(
                ctx,
                question,
                ctx.theme.tokens.colors.text_primary,
            );
            let keep_label = t("overview-tilo-keep");
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &ButtonFace::new(keep, &keep_label)
                    .hovered(self.cursor.is_some_and(|p| keep.contains(p))),
            );
            // The one place a destructive face is filled: the answer that goes ahead.
            Button::paint(
                ctx.scene,
                ctx.transform,
                ctx.text,
                ctx.theme,
                &ButtonFace::new(going, &confirm.answer)
                    .role(ButtonRole::Destructive)
                    .confirming()
                    .hovered(self.cursor.is_some_and(|p| going.contains(p))),
            );
        }
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
                    let asking = self.tilo_asks(item);
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
                    if asking {
                        // A row that asks says what would be lost, and offers the two answers in
                        // the order they stand: Keep first, the one that goes ahead last.
                        if let Some(confirm) = &item.confirm {
                            node = node.with_description(confirm.question.clone());
                        }
                    } else if !item.facts.is_empty() {
                        node = node.with_description(
                            item.facts
                                .iter()
                                .map(|fact| fact.text.as_str())
                                .collect::<Vec<_>>()
                                .join(", "),
                        );
                    }
                    if let Some((_question, keep, going)) = self.tilo_ask_rects(index)
                        && let Some(confirm) = &item.confirm
                    {
                        node.children.push(
                            AccessNode::new(Role::Button)
                                .with_action_target(format!("{}:keep", item.key))
                                .with_name(t("overview-tilo-keep"))
                                .with_bounds(keep)
                                .with_state(AccessState::new().focusable(true)),
                        );
                        node.children.push(
                            AccessNode::new(Role::Button)
                                .with_action_target(format!("{}:action", item.key))
                                .with_name(format!("{}: {}", confirm.answer, item.title))
                                .with_bounds(going)
                                .with_state(AccessState::new().focusable(true)),
                        );
                    } else if let (Some((label, _)), Some(bounds)) =
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
                if let Some((question, keep, going)) = self.tilo_ask_rects(index) {
                    rects.push((format!("{}:question", item.key), question));
                    rects.push((format!("{}:keep", item.key), keep));
                    rects.push((format!("{}:action", item.key), going));
                }
            }
        }
        rects
    }
}
