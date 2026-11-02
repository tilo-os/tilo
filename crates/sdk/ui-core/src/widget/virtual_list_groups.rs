//! The groups of a [`super::VirtualList`]: the heading a run of its rows stands under, the
//! band it is drawn in, and whether the run is open.
//!
//! A model says which group each row is in ([`super::ListModel::group_at`]); rows of one
//! group come together, and the list draws the heading over the group's first row, held at
//! the top of the box while the person scrolls through it. A tail group holds the items done
//! with, which the model stands last (the model's [`super::RowGroup::Tail`]): it stands under
//! the list's tail title, closed until the person opens it, and its rows meanwhile take no
//! room and are never built.

use std::collections::HashMap;

use vello::Scene;
use vello::kurbo::{Affine, Point, Rect};

use super::{ListModel, VirtualList};
use crate::paint::PaintCtx;
use crate::text_render::TextRenderer;
use crate::theme::Theme;
use crate::widget::Interaction;
use crate::widget::section::{SectionHeaderFace, band_height};

/// A group's heading and whether it is the list's tail.
type Group = (String, bool);

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
fn group_of<M: ListModel>(model: &M, index: usize, tail: Option<&str>) -> Option<Group> {
    Some(match model.group_at(index)? {
        super::RowGroup::Heading(heading) => (heading, false),
        super::RowGroup::Tail => (tail?.to_owned(), true),
    })
}

impl Groups {
    /// The band height the theme asks, kept for laying the bands out.
    pub(super) fn theme(&mut self, theme: &Theme) -> f64 {
        self.height = band_height(theme);
        self.height
    }

    /// Whether the heading's group is open: a group first seen takes the state its rows ask
    /// for, and the person's press keeps it.
    fn is_open(&mut self, group: &Group) -> bool {
        if let Some(open) = self.open.get(&group.0) {
            return *open;
        }
        let open = !group.1;
        self.open.insert(group.0.clone(), open);
        open
    }

    /// Whether the heading's group is open, asking the model for the row's group.
    pub(super) fn shows<M: ListModel>(
        &mut self,
        model: &M,
        index: usize,
        tail: Option<&str>,
    ) -> bool {
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
            Some(before) => group_of(model, before, tail).is_none_or(|before| before.0 != group.0),
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
        self.open.insert(heading.to_owned(), open);
        open
    }

    /// The band of the group the row `index` starts, as the layout placed it.
    pub(super) fn place(&mut self, index: usize, index_group: Group, top: f64, closed: bool) {
        self.bands.retain(|band| band.heading != index_group.0);
        self.bands.push(Band {
            index,
            top,
            heading: index_group.0,
            closed,
        });
    }

    /// The bands in view, the one holding the top of the box first, held at its top.
    pub(super) fn in_view(&mut self, held: Option<(Group, bool)>, viewport: f64) -> Vec<Band> {
        let height = self.height;
        self.bands.sort_by(|a, b| a.top.total_cmp(&b.top));
        let mut bands = std::mem::take(&mut self.bands);
        if let Some((group, closed)) = held {
            let at = bands.iter().position(|band| band.heading == group.0);
            let starts = at.is_some_and(|at| bands[at].top >= 0.0);
            if !starts {
                // The group under the top holds its heading there: the one sliding out above
                // it is not drawn as well, or its label would read over the held one.
                let index = at.map(|at| bands[at].index).unwrap_or_default();
                bands.retain(|band| band.heading != group.0);
                bands.insert(
                    0,
                    Band {
                        index,
                        top: 0.0,
                        heading: group.0,
                        closed,
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

    /// The heading whose band holds the y `local` of the list's box.
    pub(super) fn heading_at(&self, local: f64) -> Option<String> {
        let height = self.height;
        self.bands
            .iter()
            .rev()
            .find(|band| local >= band.top && local < band.top + height)
            .map(|band| band.heading.clone())
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
    pub(super) fn row_open(&mut self, index: usize) -> bool {
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
    pub(super) fn grouped_height(&mut self, index: usize, height: f64) -> f64 {
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
            Some((group, closed))
        });
        let viewport = self.viewport.viewport();
        self.groups.in_view(held, viewport)
    }

    /// Paint the heading of every group in view over the rows, the one holding the top of the
    /// box at its top.
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
        let height = self.groups.height;
        let x1 = if self.rows_beside_bar {
            self.rect.x1 - self.gutter
        } else {
            self.rect.x1
        };
        let rects = bands
            .iter()
            .map(|band| {
                let top = self.rect.y0 + band.top;
                Rect::new(self.rect.x0, top, x1, top + height)
            })
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
        let mut ctx = PaintCtx::new(scene, transform, text, theme);
        for (band, rect) in bands.iter().zip(rects) {
            let hovered = cursor.is_some_and(|point| rect.contains(point));
            let held = self.groups.held() == Some(band.heading.as_str());
            SectionHeaderFace::group(&band.heading)
                .disclosure(!band.closed)
                .paint(
                    &mut ctx,
                    rect,
                    Interaction {
                        hovered,
                        pressed: hovered && held,
                        ..Interaction::default()
                    },
                );
        }
    }

    /// A press at `point` (the list's own coordinates) on a heading's band, held until it is
    /// let go. Whether a band was there.
    pub(super) fn hold_band(&mut self, point: Point) -> bool {
        if !self.rect.contains(point) {
            return false;
        }
        self.bands_in_view();
        let Some(heading) = self.groups.heading_at(point.y - self.rect.y0) else {
            return false;
        };
        self.groups.hold(heading);
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
            || self.groups.heading_at(point.y - self.rect.y0).as_deref() != Some(heading.as_str())
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
