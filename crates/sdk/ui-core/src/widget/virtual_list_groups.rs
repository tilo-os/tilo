//! The groups of a [`super::VirtualList`]: the heading a run of its rows stands under, the
//! band it is drawn in, and whether the run is open.
//!
//! A model says which group each row is in ([`super::ListModel::group_at`]); rows of one
//! group come together, and the list draws the heading over the group's first row, held at
//! the top of the box while the person scrolls through it. A tail group holds the items done
//! with, which the model stands last (the model's [`super::RowGroup::Tail`]): it stands under
//! the list's tail title, closed until the person opens it, and its rows meanwhile take no
//! room and are never built.
//!
//! A heading is a disclosure of the list's own, in its focus order: the arrow keys reach it
//! like a row, Enter or Space folds and unfolds its group, and the accessibility tree carries
//! it as a focusable disclosure, expanded while its rows show and holding the count of the
//! rows under it, which an agent opens and closes by its [`HEADING_TARGET`].

use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{ListModel, VirtualList};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::section::{SectionHeaderFace, band_height};
use crate::widget::{Interaction, Tone};

/// What an accessibility target naming a heading starts with: the rest is the heading, as
/// [`super::VirtualList`] hands it to `perform_semantic_action`.
pub(super) const HEADING_TARGET: &str = "group:";

/// A group's heading, whether it is the list's tail, and the tone it reads in.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Group {
    /// What the heading says.
    pub heading: String,
    /// Whether it is the list's tail: the items done with.
    pub tail: bool,
    /// How the heading and its count read.
    pub tone: Tone,
}

/// A heading's band as the last layout placed it, in the list's own coordinates (a y of zero
/// is the top of the box, as [`super::VirtualList::item_view_top`] reads).
#[derive(Clone, Debug, PartialEq)]
pub(super) struct Band {
    /// The row whose group the band heads.
    pub index: usize,
    /// Where the band stands.
    pub top: f64,
    /// The heading the group reads.
    pub heading: String,
    /// Whether the group is closed: its rows take no room and are not built.
    pub closed: bool,
    /// How the heading and its count read.
    pub tone: Tone,
}

/// Which groups are open, by heading, and the bands the last layout placed.
#[derive(Default)]
pub(super) struct Groups {
    /// Whether a heading's group shows its rows. A heading first seen takes the state its
    /// group asks for: a tail group starts closed, any other open.
    open: HashMap<String, bool>,
    /// The bands of the groups whose first row the last layout placed.
    pub(super) bands: Vec<Band>,
    /// The rows heading a group folded away, as the last mount walked them.
    folded: Vec<usize>,
    /// The heading a press is holding, until it is let go: a heading opens on the release, as
    /// a button does, so starting a press and moving off it folds nothing.
    held: Option<String>,
    /// The band height the last theme asks, in logical pixels.
    pub height: f64,
}

/// What the row `index` is grouped by, without the model's help when it groups nothing.
pub(super) fn group_of<M: ListModel>(model: &M, index: usize, tail: Option<&str>) -> Option<Group> {
    Some(match model.group_at(index)? {
        super::RowGroup::Heading { text, tone } => Group {
            heading: text,
            tail: false,
            tone,
        },
        super::RowGroup::Tail => Group {
            heading: tail?.to_owned(),
            tail: true,
            // The items done with read as quietly as they stand.
            tone: Tone::Neutral,
        },
    })
}

impl Groups {
    /// The band height the theme asks, kept for laying the bands out. A heading is a target a
    /// finger presses, so under a coarse pointer its band grows to the target a finger needs.
    pub(super) fn theme(
        &mut self,
        theme: &Theme,
        input: crate::interaction::InteractionEnvironment,
    ) -> f64 {
        let target = if input.supports_hover() {
            0.0
        } else {
            f64::from(input.minimum_target())
        };
        self.height = band_height(theme).max(target);
        self.height
    }

    /// Whether the heading's group is open: a group the person has not folded takes the state
    /// its rows ask for, a tail group closed, and the person's press keeps it.
    fn is_open(&self, group: &Group) -> bool {
        self.open
            .get(&group.heading)
            .copied()
            .unwrap_or(!group.tail)
    }

    /// Whether the heading's group is open, asking the model for the row's group.
    pub(super) fn shows<M: ListModel>(&self, model: &M, index: usize, tail: Option<&str>) -> bool {
        match group_of(model, index, tail) {
            None => true,
            Some(group) => self.is_open(&group),
        }
    }

    /// Whether the row `index` starts its group: the heading over it.
    pub(super) fn starts<M: ListModel>(&self, model: &M, index: usize, tail: Option<&str>) -> bool {
        let Some(group) = group_of(model, index, tail) else {
            return false;
        };
        // A row before a closed group starts it too.
        match index.checked_sub(1) {
            None => true,
            Some(before) => {
                group_of(model, before, tail).is_none_or(|before| before.heading != group.heading)
            }
        }
    }

    /// Hold the heading a press landed on.
    pub(super) fn hold(&mut self, heading: String) {
        self.held = Some(heading);
    }

    /// The heading a press holds, while it holds it.
    pub(super) fn held(&self) -> Option<&str> {
        self.held.as_deref()
    }

    /// Let go of the heading a press held: the one it held.
    pub(super) fn release(&mut self) -> Option<String> {
        self.held.take()
    }

    /// A heading's group opened or closed: whether it is open now.
    pub(super) fn toggle(&mut self, heading: &str) -> bool {
        let open = !self.open.get(heading).copied().unwrap_or(true);
        self.set_open(heading, open);
        open
    }

    /// Show the heading's group's rows, or fold them away: whether that changed it.
    pub(super) fn set_open(&mut self, heading: &str, open: bool) -> bool {
        if self.open.get(heading).copied().unwrap_or(true) == open {
            return false;
        }
        self.open.insert(heading.to_owned(), open);
        true
    }

    /// The band of the group the row `index` starts, as the layout placed it.
    pub(super) fn place(&mut self, index: usize, index_group: Group, top: f64, closed: bool) {
        // A group first placed takes the state its rows ask for, so a press on its heading
        // opens a tail group rather than closing what it never opened.
        self.open
            .entry(index_group.heading.clone())
            .or_insert(!index_group.tail);
        self.bands
            .retain(|band| band.heading != index_group.heading);
        self.bands.push(Band {
            index,
            top,
            heading: index_group.heading,
            closed,
            tone: index_group.tone,
        });
    }

    /// The bands in view, the one holding the top of the box first, held at its top. `held` is
    /// the group under the top of the box, whether it is closed, and the index of its first
    /// row.
    pub(super) fn in_view(
        &mut self,
        held: Option<(Group, bool, usize)>,
        viewport: f64,
    ) -> Vec<Band> {
        let height = self.height;
        self.bands.sort_by(|a, b| a.top.total_cmp(&b.top));
        let mut bands = std::mem::take(&mut self.bands);
        if let Some((group, closed, start)) = held {
            let at = bands.iter().position(|band| band.heading == group.heading);
            let starts = at.is_some_and(|at| bands[at].top >= 0.0);
            if !starts {
                // The group under the top holds its heading there: the one sliding out above
                // it is not drawn as well, or its label would read over the held one.
                let index = at.map_or(start, |at| bands[at].index);
                bands.retain(|band| band.heading != group.heading);
                bands.insert(
                    0,
                    Band {
                        index,
                        top: 0.0,
                        heading: group.heading,
                        closed,
                        tone: group.tone,
                    },
                );
            }
        }
        bands.retain(|band| band.top < viewport && band.top + height > 0.0);
        self.bands.clone_from(&bands);
        bands
    }

    /// Forget the folded groups the last mount walked.
    pub(super) fn clear_folded(&mut self) {
        self.folded.clear();
    }

    /// The row `index` heads a folded group: its band stands where it would.
    pub(super) fn fold(&mut self, index: usize) {
        self.folded.push(index);
    }

    /// The rows heading a folded group, as the last mount walked them.
    pub(super) fn take_folded(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.folded)
    }

    /// The band holding the y `local` of the list's box.
    pub(super) fn band_at(&self, local: f64) -> Option<Band> {
        let height = self.height;
        self.bands
            .iter()
            .rev()
            .find(|band| local >= band.top && local < band.top + height)
            .cloned()
    }
}

impl<M: ListModel> VirtualList<M> {
    /// The title over the list's tail group: the items done with, after every other group,
    /// closed until the person opens it. A list whose model names no tail reads as it did.
    #[must_use]
    pub fn with_tail_title(mut self, title: impl Into<String>) -> Self {
        self.tail_title = Some(title.into());
        self
    }

    pub(super) fn set_tail_title(&mut self, title: Option<String>) {
        self.tail_title = title;
    }

    /// The row's group, as the model names it: asked of the model once per row, while its
    /// props stand as they were.
    pub(super) fn row_group(&mut self, index: usize) -> Option<Group> {
        let key = self.model.key_at(index)?;
        if let Some(group) = self.row_groups.get(&key) {
            return group.clone();
        }
        let group = group_of(&self.model, index, self.tail_title.as_deref());
        self.row_groups.insert(key, group.clone());
        group
    }

    /// Forget what the rows were grouped by: their props changed.
    pub(super) fn regrouped(&mut self) {
        self.row_groups.clear();
    }

    /// Forget the group of the rows keyed `keys`: their props changed.
    pub(super) fn regroup_keys<'k>(&mut self, keys: impl IntoIterator<Item = &'k M::Key>)
    where
        M::Key: 'k,
    {
        for key in keys {
            self.row_groups.remove(key);
        }
    }

    /// Whether the row `index` takes room: its group is open.
    pub(super) fn row_open(&self, index: usize) -> bool {
        self.groups
            .shows(&self.model, index, self.tail_title.as_deref())
    }

    /// Whether the row `index` starts a group: the heading over it.
    pub(super) fn row_starts(&self, index: usize) -> bool {
        self.groups
            .starts(&self.model, index, self.tail_title.as_deref())
    }

    /// The height the row `index` takes in the layout: its own, with the band of the heading
    /// over it when it starts a group; only that band while its group is closed.
    pub(super) fn grouped_height(&self, index: usize, height: f64) -> f64 {
        let band = self.groups.height;
        let starts = self.row_starts(index);
        if !self.row_open(index) {
            return if starts { band } else { 0.0 };
        }
        if starts { height + band } else { height }
    }

    /// The room the band over the row `index` takes, when it starts a group.
    pub(super) fn band_room(&self, index: usize) -> f64 {
        if self.row_starts(index) {
            self.groups.height
        } else {
            0.0
        }
    }

    /// The box the row `index` itself takes: the list's width, one bleed in from its edges, at
    /// its own height. The viewport holds a row that starts a group together with the band of
    /// the heading over it, so the band comes off the top: a row's lift, focus ring and
    /// disclosure hang on the row, never on the heading above it.
    pub(super) fn row_box(&self, index: usize) -> Option<Rect> {
        let top = self.viewport.item_view_top(index)?;
        let height = self.viewport.item_height(index)?;
        let band = self.band_room(index);
        Some(self.lift_rect(top + band, (height - band).max(0.0)))
    }

    /// Place the band of the group the row `index` starts, if it starts one.
    pub(super) fn place_band(&mut self, index: usize, top: f64) {
        let Some(group) = self.row_group(index) else {
            return;
        };
        if !self.row_starts(index) {
            return;
        }
        let closed = !self.row_open(index);
        self.groups.place(index, group, top, closed);
    }

    /// Place the band of every group the layout holds: those whose first row is mounted, and
    /// those whose rows are folded away under it, from where the viewport stands the row.
    pub(super) fn place_bands(&mut self, mounted: impl Iterator<Item = usize>) {
        let mut starts: Vec<usize> = std::mem::take(&mut self.groups.bands)
            .into_iter()
            .map(|band| band.index)
            .collect();
        // A group folded away before the person ever opened it has no row of its own: the
        // mount that walked its rows named the one heading it.
        starts.extend(self.groups.take_folded());
        for index in mounted {
            if self.row_starts(index) {
                starts.push(index);
            }
        }
        starts.sort_unstable();
        starts.dedup();
        for index in starts {
            let top = self.viewport.item_view_top(index).unwrap_or_default();
            self.place_band(index, top);
        }
    }

    /// The bands the groups in view show, the one holding the top of the box first.
    pub(super) fn bands_in_view(&mut self) -> Vec<Band> {
        // Where each group starts now: a scroll moves the bands with their rows, and the
        // layout that placed them may be a frame behind.
        for band in &mut self.groups.bands {
            if let Some(top) = self.viewport.item_view_top(band.index) {
                band.top = top;
            }
        }
        let held = self.viewport.hit_test(0.0).and_then(|top| {
            let group = self.row_group(top)?;
            if self.row_starts(top) {
                return None;
            }
            let closed = !self.row_open(top);
            let start = self.group_start(top);
            Some((group, closed, start))
        });
        let viewport = self.viewport.viewport();
        self.groups.in_view(held, viewport)
    }

    /// The box the heading at `top` (the list's own coordinates) stands its content in: the
    /// column its rows' content stands in, to the edge their facts end on, so a heading reads
    /// on the same grid as the rows under it.
    pub(super) fn header_rect(&self, top: f64) -> Rect {
        let band = self.band_rect(top);
        let x0 = band.x0 + self.lead + self.chevron_column;
        Rect::new(
            x0,
            band.y0,
            (self.rect.x1 - self.trailing_inset()).max(x0),
            band.y1,
        )
    }

    /// The box the band at `top` (the list's own coordinates) is drawn in.
    pub(super) fn band_rect(&self, top: f64) -> Rect {
        let x1 = if self.rows_beside_bar {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1
        };
        let top = self.shown_top(top);
        Rect::new(self.rect.x0, top, x1, top + self.groups.height)
    }

    /// Paint the heading of every group in view over the rows, the one holding the top of the
    /// box at its top. The heading holding the keyboard wears its focus ring.
    pub(super) fn draw_bands(
        &mut self,
        scene: &mut Scene,
        transform: Affine,
        text: &mut TextRenderer,
        theme: &Theme,
    ) {
        let bands = self.bands_in_view();
        if bands.is_empty() {
            return;
        }
        let cursor = self.last_cursor;
        let focused = if self.is_focused {
            self.focused_group
                .as_ref()
                .map(|(heading, _)| heading.clone())
        } else {
            None
        };
        let rects = bands
            .iter()
            .map(|band| self.band_rect(band.top))
            .collect::<Vec<_>>();
        // A band hides what it slides over: the rows of the group it holds while they scroll
        // under it, and the band of the group leaving at the top.
        for rect in &rects {
            scene.fill(
                vello::peniko::Fill::NonZero,
                transform,
                theme.tokens.colors.surfaces.content,
                None,
                rect,
            );
        }
        let radius = f64::from(theme.tokens.radii.item);
        // The ring goes under the heading it belongs to, so both are drawn in one pass over
        // the bands and the ring first.
        for band in &bands {
            let has_keyboard = focused.as_deref() == Some(band.heading.as_str());
            if has_keyboard {
                let rect = self.band_rect(band.top);
                // Inside the band, where the ring's own 1 px reach cannot fall past the
                // list's edge and be clipped away.
                crate::widget::paint_focus_ring(
                    scene,
                    transform,
                    rect.inflate(-2.0, -2.0),
                    (radius - 2.0).max(0.0),
                    theme,
                );
            }
        }
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        for (band, rect) in bands.iter().zip(rects) {
            let hovered = cursor.is_some_and(|point| rect.contains(point));
            let held = self.groups.held() == Some(band.heading.as_str());
            let has_keyboard = focused.as_deref() == Some(band.heading.as_str());
            // The heading reads in the column its rows stand in, its chevron in the gutter
            // left of it, and how many rows it holds on the edge their facts end on.
            let count = self.heading_rows(band);
            SectionHeaderFace::group(&band.heading)
                .tone(band.tone)
                .disclosure(!band.closed)
                .trailing_count(count)
                .paint(
                    &mut ctx,
                    self.header_rect(band.top),
                    Interaction {
                        hovered,
                        pressed: hovered && held,
                        focused: has_keyboard,
                        ..Interaction::default()
                    },
                );
        }
    }

    /// A press at `point` (the list's own coordinates) on a heading's band, held until it is
    /// let go, the heading taking the keyboard. Whether a band was there.
    pub(super) fn hold_band(&mut self, point: Point) -> bool {
        if !self.rect.contains(point) {
            return false;
        }
        self.bands_in_view();
        let Some(band) = self.groups.band_at(point.y - self.shown_top(0.0)) else {
            return false;
        };
        self.focused_key = None;
        self.focused_group = Some((band.heading.clone(), band.index));
        self.groups.hold(band.heading);
        true
    }

    /// A press on a heading's band let go at `point`: the group it heads opened or closed,
    /// when the pointer never left it. Whether a band was there.
    pub(super) fn release_band(&mut self, point: Point) -> bool {
        let Some(heading) = self.groups.release() else {
            return false;
        };
        self.bands_in_view();
        if !self.rect.contains(point)
            || self
                .groups
                .band_at(point.y - self.shown_top(0.0))
                .map(|band| band.heading)
                != Some(heading.clone())
        {
            return false;
        }
        self.groups.toggle(&heading);
        self.regroup();
        true
    }

    /// Lay the list out again after a group opened or closed: its rows mount, or let go.
    fn regroup(&mut self) {
        self.mounted_range = 0..0;
        self.remount = true;
        self.visible_window_changed();
        self.structure_dirty = true;
    }
}

/// One stop of the list's keyboard: a group's heading, or one of its rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    /// The heading over the group whose first row is this index.
    Heading(usize),
    /// The row at this index.
    Row(usize),
}

impl<M: ListModel> VirtualList<M> {
    /// The heading holding the keyboard, and the index of the first row under it, while the
    /// rows still say the heading heads that group.
    fn heading_focus(&mut self) -> Option<(String, usize)> {
        let (heading, index) = self.focused_group.clone()?;
        if self
            .row_group(index)
            .is_none_or(|group| group.heading != heading)
        {
            // The rows moved under it: the heading is not there any more.
            self.focused_group = None;
            return None;
        }
        let start = self.group_start(index);
        self.focused_group = Some((heading.clone(), start));
        Some((heading, start))
    }

    /// The first row of the group the row `index` is in.
    fn group_start(&mut self, index: usize) -> usize {
        let Some(group) = self.row_group(index) else {
            return index;
        };
        let mut start = index;
        while start > 0
            && self
                .row_group(start - 1)
                .is_some_and(|before| before.heading == group.heading)
        {
            start -= 1;
        }
        start
    }

    /// The stop the row `index` is: its group's heading while the group is closed, the row
    /// itself while it shows.
    fn stop_at(&mut self, index: usize) -> Stop {
        if self.row_open(index) {
            Stop::Row(index)
        } else {
            Stop::Heading(self.group_start(index))
        }
    }

    /// The index the keyboard is on: under the focused heading, or the focused row's.
    fn keyboard_index(&mut self) -> Option<usize> {
        if let Some((_, start)) = self.heading_focus() {
            return Some(start);
        }
        let key = self.focused_key.clone()?;
        self.model.index_of(&key)
    }

    /// The stop the keyboard is on.
    fn keyboard_stop(&mut self) -> Option<Stop> {
        if let Some((_, start)) = self.heading_focus() {
            return Some(Stop::Heading(start));
        }
        let key = self.focused_key.clone()?;
        self.model.index_of(&key).map(Stop::Row)
    }

    /// The stop after the keyboard's, down the list: a heading's group opens under it, or,
    /// closed, the heading gives the keyboard to the heading below it.
    fn next_stop(&mut self) -> Option<Stop> {
        let count = self.model.len();
        match self.keyboard_stop()? {
            Stop::Heading(start) => {
                if self.row_open(start) {
                    return Some(Stop::Row(start));
                }
                let group = self.row_group(start)?;
                let mut below = start.saturating_add(1);
                while below < count
                    && self
                        .row_group(below)
                        .is_some_and(|at| at.heading == group.heading)
                {
                    below = below.saturating_add(1);
                }
                (below < count).then(|| {
                    // The heading under a folded group is the stop the keyboard meets next,
                    // standing over the rows it heads however they are.
                    if self.row_starts(below) {
                        Stop::Heading(below)
                    } else {
                        self.stop_at(below)
                    }
                })
            }
            Stop::Row(index) => {
                let below = index.checked_add(1)?;
                (below < count).then(|| self.stop_at(below))
            }
        }
    }

    /// The stop before the keyboard's, up the list.
    fn prev_stop(&mut self) -> Option<Stop> {
        match self.keyboard_stop()? {
            Stop::Heading(start) => {
                let above = start.checked_sub(1)?;
                Some(self.stop_at(above))
            }
            Stop::Row(index) => {
                if self.row_starts(index) {
                    // The heading over the row the keyboard is on comes first.
                    return Some(Stop::Heading(index));
                }
                let above = index.checked_sub(1)?;
                Some(self.stop_at(above))
            }
        }
    }

    /// The first stop of the list.
    fn first_stop(&mut self) -> Option<Stop> {
        (self.model.len() > 0).then(|| self.stop_at(0))
    }

    /// The last stop of the list: the heading of a closed group its rows end under.
    fn last_stop(&mut self) -> Option<Stop> {
        let last = self.model.len().checked_sub(1)?;
        Some(self.stop_at(last))
    }

    /// Hand the keyboard to `stop`, as the arrow keys and the page keys do. Whether it went.
    fn focus_stop(&mut self, stop: Stop, extend: bool) -> bool {
        match stop {
            Stop::Row(index) => self.focus_index(index, extend),
            Stop::Heading(index) => {
                let Some(group) = self.row_group(index) else {
                    return false;
                };
                let start = self.group_start(index);
                self.focused_key = None;
                self.focused_group = Some((group.heading, start));
                self.scroll_to_index(start);
                true
            }
        }
    }

    /// The index of the first row under the heading: where the last layout placed its band,
    /// else where the keyboard left it.
    pub(super) fn heading_start(&self, heading: &str) -> Option<usize> {
        if let Some((focused, index)) = &self.focused_group
            && focused == heading
        {
            return Some(*index);
        }
        self.groups
            .bands
            .iter()
            .find(|band| band.heading == heading)
            .map(|band| band.index)
    }

    /// Hand the keyboard to the heading `heading`. Whether it is there.
    pub(super) fn focus_heading(&mut self, heading: &str) -> bool {
        let Some(index) = self.heading_start(heading) else {
            return false;
        };
        self.focused_key = None;
        self.focused_group = Some((heading.to_owned(), index));
        self.scroll_to_index(index);
        true
    }

    /// Fold or unfold the heading's group, as a press on its band does.
    pub(super) fn toggle_heading(&mut self, heading: &str) -> bool {
        self.groups.toggle(heading);
        self.regroup();
        true
    }

    /// Show or fold away the heading's group's rows: whether that changed it.
    pub(super) fn set_heading_open(&mut self, heading: &str, open: bool) -> bool {
        if !self.groups.set_open(heading, open) {
            return false;
        }
        self.regroup();
        true
    }

    /// The heading the keyboard is on, before a key the list takes itself.
    pub(super) fn heading_keyboard(&mut self) -> Option<String> {
        self.heading_focus().map(|(heading, _)| heading)
    }

    /// The row the keyboard is on, for the keys that start where the keyboard is.
    pub(super) fn keyboard_row(&mut self) -> Option<usize> {
        self.keyboard_index()
    }

    /// The arrow, home and end keys move the keyboard through the headings and rows: from
    /// nowhere, to the first stop as a row list does.
    pub(super) fn move_stop(&mut self, key: &winit::keyboard::Key, extend: bool) -> Option<bool> {
        use winit::keyboard::{Key, NamedKey};
        let stop = match key {
            Key::Named(NamedKey::ArrowDown) => {
                if self.keyboard_stop().is_some() {
                    self.next_stop()
                } else {
                    self.first_stop()
                }
            }
            Key::Named(NamedKey::ArrowUp) => {
                if self.keyboard_stop().is_some() {
                    self.prev_stop()
                } else {
                    self.first_stop()
                }
            }
            Key::Named(NamedKey::Home) => self.first_stop(),
            Key::Named(NamedKey::End) => self.last_stop(),
            _ => return None,
        };
        Some(stop.is_some_and(|stop| self.focus_stop(stop, extend)))
    }
}
