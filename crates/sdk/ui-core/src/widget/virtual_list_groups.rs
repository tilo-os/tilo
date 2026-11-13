//! The groups of a [`super::VirtualList`]: the heading a run of its rows stands under, the band it
//! is drawn in, and whether the run is open.
use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{ListModel, VirtualList};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::section::{SectionHeaderFace, band_height, heading_count};
use crate::widget::{Interaction, Tone};

pub(super) const HEADING_TARGET: &str = "group:";

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct Group {
    pub heading: String,
    pub tail: bool,
    pub tone: Tone,
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct Band {
    pub index: usize,
    pub top: f64,
    pub heading: String,
    /// Whether the group is closed: its rows take no room and are not built.
    pub closed: bool,
    pub tail: bool,
    pub tone: Tone,
}

#[derive(Default)]
pub(super) struct Groups {
    open: HashMap<String, bool>,
    pub(super) bands: Vec<Band>,
    folded: Vec<usize>,
    held: Option<String>,
    /// The band height the last theme asks, in logical pixels.
    pub height: f64,
}

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
            tone: Tone::Neutral,
        },
    })
}

impl Groups {
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

    pub(super) fn shows<M: ListModel>(&self, model: &M, index: usize, tail: Option<&str>) -> bool {
        match group_of(model, index, tail) {
            None => true,
            Some(group) => self.is_open(&group),
        }
    }

    pub(super) fn starts<M: ListModel>(&self, model: &M, index: usize, tail: Option<&str>) -> bool {
        let Some(group) = group_of(model, index, tail) else {
            return false;
        };
        match index.checked_sub(1) {
            None => true,
            Some(before) => {
                group_of(model, before, tail).is_none_or(|before| before.heading != group.heading)
            }
        }
    }

    pub(super) fn hold(&mut self, heading: String) {
        self.held = Some(heading);
    }

    pub(super) fn held(&self) -> Option<&str> {
        self.held.as_deref()
    }

    pub(super) fn release(&mut self) -> Option<String> {
        self.held.take()
    }

    pub(super) fn toggle(&mut self, heading: &str) -> bool {
        let open = !self.open.get(heading).copied().unwrap_or(true);
        self.set_open(heading, open);
        open
    }

    pub(super) fn set_open(&mut self, heading: &str, open: bool) -> bool {
        if self.open.get(heading).copied().unwrap_or(true) == open {
            return false;
        }
        self.open.insert(heading.to_owned(), open);
        true
    }

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
            tail: index_group.tail,
            tone: index_group.tone,
        });
    }

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
                        tail: group.tail,
                        tone: group.tone,
                    },
                );
            }
        }
        bands.retain(|band| band.top < viewport && band.top + height > 0.0);
        self.bands.clone_from(&bands);
        bands
    }

    pub(super) fn clear_folded(&mut self) {
        self.folded.clear();
    }

    pub(super) fn fold(&mut self, index: usize) {
        self.folded.push(index);
    }

    pub(super) fn take_folded(&mut self) -> Vec<usize> {
        std::mem::take(&mut self.folded)
    }

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
    #[must_use]
    pub fn with_tail_title(mut self, title: impl Into<String>) -> Self {
        self.tail_title = Some(title.into());
        self
    }

    pub(super) fn set_tail_title(&mut self, title: Option<String>) {
        if self.tail_title == title {
            return;
        }
        self.tail_title = title;
        self.headings_changed();
    }

    pub fn set_view_title(&mut self, title: Option<String>, count: Option<usize>) {
        if self.view_title == title && self.view_count == count {
            return;
        }
        self.view_title = title;
        self.view_count = count;
        self.headings_changed();
    }

    fn headings_changed(&mut self) {
        self.regrouped();
        self.visible_window_changed();
    }

    #[must_use]
    pub fn in_view(mut self, title: impl Into<String>, count: Option<usize>) -> Self {
        self.set_view_title(Some(title.into()), count);
        self
    }

    /// Whether the view's title names the list's one group, so that heading would say nothing
    /// the title does not: the rows then stand under no heading at all. Read off the rows once,
    /// and again whenever they are grouped anew.
    pub(super) fn heads_nothing(&self) -> bool {
        if let Some(read) = self.heads_nothing.get() {
            return read;
        }
        let read = self.reads_heads_nothing();
        self.heads_nothing.set(Some(read));
        read
    }

    fn reads_heads_nothing(&self) -> bool {
        let Some(title) = self.view_title.as_deref() else {
            return false;
        };
        let len = self.model.len();
        let tail = self.tail_title.as_deref();
        let Some(first) = group_of(&self.model, 0, tail) else {
            return false;
        };
        let open = (0..len).filter(|index| !self.model.done(*index)).count();
        if first.tail || first.heading != title || self.view_count.unwrap_or(0) != open {
            return false;
        }
        (1..len).all(|index| {
            group_of(&self.model, index, tail)
                .is_some_and(|group| !group.tail && group.heading == first.heading)
        })
    }

    pub(super) fn row_group(&mut self, index: usize) -> Option<Group> {
        if self.heads_nothing() {
            return None;
        }
        let key = self.model.key_at(index)?;
        if let Some(group) = self.row_groups.get(&key) {
            return group.clone();
        }
        let group = group_of(&self.model, index, self.tail_title.as_deref());
        self.row_groups.insert(key, group.clone());
        group
    }

    pub(super) fn regrouped(&mut self) {
        self.row_groups.clear();
        self.heads_nothing.set(None);
    }

    pub(super) fn regroup_keys<'k>(&mut self, keys: impl IntoIterator<Item = &'k M::Key>)
    where
        M::Key: 'k,
    {
        self.heads_nothing.set(None);
        for key in keys {
            self.row_groups.remove(key);
        }
    }

    pub(super) fn row_open(&self, index: usize) -> bool {
        self.heads_nothing()
            || self
                .groups
                .shows(&self.model, index, self.tail_title.as_deref())
    }

    pub(super) fn row_starts(&self, index: usize) -> bool {
        !self.heads_nothing()
            && self
                .groups
                .starts(&self.model, index, self.tail_title.as_deref())
    }

    pub(super) fn grouped_height(&self, index: usize, height: f64) -> f64 {
        let band = self.band_room(index);
        if !self.row_open(index) {
            return band;
        }
        height + band
    }

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

    pub(super) fn place_bands(&mut self, mounted: impl Iterator<Item = usize>) {
        let mut starts: Vec<usize> = std::mem::take(&mut self.groups.bands)
            .into_iter()
            .map(|band| band.index)
            .collect();
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

    pub(super) fn bands_in_view(&mut self) -> Vec<Band> {
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

    pub(super) fn band_rect(&self, top: f64) -> Rect {
        let x1 = if self.rows_beside_bar {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1
        };
        let top = self.shown_top(top);
        Rect::new(self.rect.x0, top, x1, top + self.groups.height)
    }

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
            // left of it, and how many of the rows it holds are left to do on the edge their
            // facts end on, where that number says something the rows do not.
            let count = self.shown_heading_count(band);
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

    /// The number the heading of `band` shows over its rows, where the number says something
    /// the rows do not ([`heading_count`]): a group of one row draws no count, since the row
    /// under it says the one thing there is to count.
    pub(super) fn shown_heading_count(&self, band: &Band) -> Option<usize> {
        heading_count(self.heading_open_rows(band), None)
    }

    /// How many of the rows under the band's heading a person still has to do: the ones they are
    /// not done with, since a number beside a heading says what is left ([`heading_count`]). The
    /// tail heads the rows done with, so it counts every one of them.
    pub(super) fn heading_open_rows(&self, band: &Band) -> usize {
        let rows = self.heading_under(band);
        if band.tail {
            return rows.count();
        }
        rows.filter(|index| !self.model.done(*index)).count()
    }

    fn heading_under(&self, band: &Band) -> std::ops::Range<usize> {
        let len = self.model.len();
        let tail = self.tail_title.as_deref();
        let group = |index: usize| super::groups::group_of(&self.model, index, tail);
        let same = |index: usize| group(index).is_some_and(|group| group.heading == band.heading);
        let mut start = band.index.min(len);
        while start > 0 && same(start - 1) {
            start -= 1;
        }
        let mut end = start;
        while end < len && group(end).is_none_or(|group| group.heading == band.heading) {
            end += 1;
        }
        start..end
    }

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

    fn regroup(&mut self) {
        self.mounted_range = 0..0;
        self.remount = true;
        self.visible_window_changed();
        self.structure_dirty = true;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Stop {
    Heading(usize),
    Row(usize),
}

impl<M: ListModel> VirtualList<M> {
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

    fn stop_at(&mut self, index: usize) -> Stop {
        if self.row_open(index) {
            Stop::Row(index)
        } else {
            Stop::Heading(self.group_start(index))
        }
    }

    fn keyboard_index(&mut self) -> Option<usize> {
        if let Some((_, start)) = self.heading_focus() {
            return Some(start);
        }
        let key = self.focused_key.clone()?;
        self.model.index_of(&key)
    }

    fn keyboard_stop(&mut self) -> Option<Stop> {
        if let Some((_, start)) = self.heading_focus() {
            return Some(Stop::Heading(start));
        }
        let key = self.focused_key.clone()?;
        self.model.index_of(&key).map(Stop::Row)
    }

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

    fn prev_stop(&mut self) -> Option<Stop> {
        match self.keyboard_stop()? {
            Stop::Heading(start) => {
                let above = start.checked_sub(1)?;
                Some(self.stop_at(above))
            }
            Stop::Row(index) => {
                if self.row_starts(index) {
                    return Some(Stop::Heading(index));
                }
                let above = index.checked_sub(1)?;
                Some(self.stop_at(above))
            }
        }
    }

    fn first_stop(&mut self) -> Option<Stop> {
        (self.model.len() > 0).then(|| self.stop_at(0))
    }

    fn last_stop(&mut self) -> Option<Stop> {
        let last = self.model.len().checked_sub(1)?;
        Some(self.stop_at(last))
    }

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

    pub(super) fn focus_heading(&mut self, heading: &str) -> bool {
        let Some(index) = self.heading_start(heading) else {
            return false;
        };
        self.focused_key = None;
        self.focused_group = Some((heading.to_owned(), index));
        self.scroll_to_index(index);
        true
    }

    pub(super) fn toggle_heading(&mut self, heading: &str) -> bool {
        self.groups.toggle(heading);
        self.regroup();
        true
    }

    pub(super) fn set_heading_open(&mut self, heading: &str, open: bool) -> bool {
        if !self.groups.set_open(heading, open) {
            return false;
        }
        self.regroup();
        true
    }

    pub(super) fn heading_keyboard(&mut self) -> Option<String> {
        self.heading_focus().map(|(heading, _)| heading)
    }

    pub(super) fn keyboard_row(&mut self) -> Option<usize> {
        self.keyboard_index()
    }

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
