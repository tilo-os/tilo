//! Which draws each region of a frame paints, listed in one walk of the scene.
//!
//! A frame is drawn region by region, and every region replays the draws that
//! touch it as its own picture. Walking the whole scene once per region made a
//! frame cost its regions times its draws, however small the change was; here
//! the scene is walked once and each draw is listed for the regions it can
//! paint, so drawing a region reads only its own draws. The regions an op is
//! listed for are found through an index of the regions over coarse cells, so
//! that too costs what the op covers rather than the number of regions.
//!
//! A draw is listed for a region only if every layer around it reaches that
//! region too: a layer is listed for the regions its clip shows in, and what
//! is inside it can paint no others.

use smallvec::SmallVec;
use vello_cpu::kurbo::Rect;

use crate::Region;
use crate::decode::{DisplayList, LayerMode, Op};

/// The narrowest index cell, in pixels.
const CELL: u32 = 64;

/// The most cells the region index has; a bigger target uses bigger cells.
const MAX_CELLS: u32 = 4096;

/// The draws of one frame, listed for each region it draws in.
#[derive(Default)]
pub(crate) struct Plan {
    /// The regions a frame is drawn in, as asked, already inside the target.
    regions: Vec<Region>,
    /// Per region, indices into [`DisplayList::ops`] of the draws it paints,
    /// in draw order: a draw reaches a region when its bounds overlap it and
    /// the layers around it reach it too.
    lists: Vec<Vec<u32>>,
    /// Per region, its geometry, for the exact test the cells only narrow.
    bounds: Vec<Rect>,
    /// Per cell, the regions that reach it.
    cells: Vec<SmallVec<[u32; 4]>>,
    /// The cells the index filled, to clear just those.
    filled: Vec<u32>,
    /// Cells across and down, and their size in pixels.
    across: u32,
    down: u32,
    cell: u32,
    /// The regions one op reaches, and how it tells the ones it has.
    hits: SmallVec<[u32; 4]>,
    reached: Vec<u32>,
    stamp: u32,
    /// Per region, the stamp of the innermost open layer whose clip reaches
    /// it, or 0 outside every layer: what tells whether a draw inside a layer
    /// can paint there.
    chain: Vec<u32>,
    /// Per open layer, where its entries in `restore` start and its own stamp.
    open: Vec<(usize, u32)>,
    /// Per entry of an open layer: the region whose `chain` it set, and what
    /// it was, to put back when the layer ends.
    restore: Vec<(u32, u32)>,
    /// The last layer stamp given out.
    layer: u32,
}

impl Plan {
    /// List `list`'s draws for the parts of a `width` x `height` target that
    /// `regions` cover.
    pub(crate) fn build(
        &mut self,
        list: &DisplayList,
        regions: &[Region],
        width: u32,
        height: u32,
    ) {
        self.start(regions, width, height);
        let mut i = 0;
        while let Some(op) = list.ops.get(i) {
            match op {
                Op::End => {
                    if let Some((mark, _)) = self.open.pop() {
                        // The end is listed for the regions the layer was,
                        // and for no other: they are the entries it set.
                        for slot in mark..self.restore.len() {
                            let id = self.restore[slot].0;
                            if let Some(ops) = self.lists.get_mut(id as usize) {
                                ops.push(i as u32);
                            }
                        }
                        while self.restore.len() > mark {
                            if let Some((id, was)) = self.restore.pop()
                                && let Some(slot) = self.chain.get_mut(id as usize)
                            {
                                *slot = was;
                            }
                        }
                    }
                    i += 1;
                }
                Op::Begin {
                    bbox,
                    path,
                    mode,
                    end,
                    ..
                } => {
                    // A layer that shows nothing shows nothing of what is
                    // inside it, and neither does a clip that misses every
                    // region: the whole layer is skipped, not walked.
                    if path.is_some() && !matches!(mode, LayerMode::LuminanceMask) {
                        self.hits(*bbox);
                        self.reaches();
                    } else {
                        self.hits.clear();
                    }
                    if self.hits.is_empty() {
                        i = end.saturating_add(1);
                        continue;
                    }
                    self.layer = self.layer.saturating_add(1);
                    let stamp = self.layer.max(1);
                    let mark = self.restore.len();
                    for slot in 0..self.hits.len() {
                        let id = self.hits[slot];
                        let was = self.chain.get(id as usize).copied().unwrap_or(0);
                        self.restore.push((id, was));
                        if let Some(at) = self.chain.get_mut(id as usize) {
                            *at = stamp;
                        }
                        if let Some(ops) = self.lists.get_mut(id as usize) {
                            ops.push(i as u32);
                        }
                    }
                    self.open.push((mark, stamp));
                    i += 1;
                }
                Op::Draw { bbox, .. }
                | Op::Glyphs { bbox, .. }
                | Op::Text { bbox, .. }
                | Op::Blur { bbox, .. } => {
                    self.hits(*bbox);
                    self.reaches();
                    for slot in 0..self.hits.len() {
                        if let Some(ops) = self.lists.get_mut(self.hits[slot] as usize) {
                            ops.push(i as u32);
                        }
                    }
                    i += 1;
                }
            }
        }
    }

    /// The regions this plan lists draws for, inside the target.
    pub(crate) fn regions(&self) -> &[Region] {
        &self.regions
    }

    /// The draws of one region, in draw order, taken out of the plan: hand
    /// them back with [`Self::give_back`] once the region is drawn.
    pub(crate) fn take(&mut self, index: usize) -> Vec<u32> {
        let index = index.min(self.lists.len().saturating_sub(1));
        self.lists
            .get_mut(index)
            .map(std::mem::take)
            .unwrap_or_default()
    }

    /// Give a region's draws back, ready for the next frame.
    pub(crate) fn give_back(&mut self, index: usize, ops: Vec<u32>) {
        if let Some(list) = self.lists.get_mut(index) {
            *list = ops;
        }
    }

    /// Empty every list and size the index to this frame's regions and
    /// target.
    fn start(&mut self, regions: &[Region], width: u32, height: u32) {
        self.regions.clear();
        self.regions.extend_from_slice(regions);
        self.bounds.clear();
        self.bounds.extend(self.regions.iter().map(|region| {
            Rect::new(
                f64::from(region.x),
                f64::from(region.y),
                f64::from(region.x + region.width),
                f64::from(region.y + region.height),
            )
        }));
        self.lists.truncate(regions.len());
        for list in &mut self.lists {
            list.clear();
        }
        while self.lists.len() < regions.len() {
            self.lists.push(Vec::new());
        }
        self.open.clear();
        self.restore.clear();
        self.hits.clear();
        self.reached.truncate(regions.len());
        self.reached.resize(regions.len(), 0);
        self.chain.clear();
        self.chain.resize(regions.len(), 0);
        self.layer = 0;
        self.index(width, height);
    }

    /// Index the regions over cells of the target.
    fn index(&mut self, width: u32, height: u32) {
        let (cell, across, down) = {
            let mut cell = CELL;
            loop {
                let across = width.div_ceil(cell).max(1);
                let down = height.div_ceil(cell).max(1);
                if across.saturating_mul(down) <= MAX_CELLS || cell >= width.max(height) {
                    break (cell, across, down);
                }
                cell = cell.saturating_mul(2);
            }
        };
        if (cell, across, down) != (self.cell, self.across, self.down) {
            self.cell = cell;
            self.across = across;
            self.down = down;
            self.cells.clear();
            self.cells.resize((across * down) as usize, SmallVec::new());
            self.filled.clear();
        }
        for cell in std::mem::take(&mut self.filled) {
            if let Some(regions) = self.cells.get_mut(cell as usize) {
                regions.clear();
            }
        }
        for (id, region) in self.regions.iter().enumerate() {
            if region.width == 0 || region.height == 0 {
                continue;
            }
            let (x0, x1) = (region.x / cell, (region.x + region.width - 1) / cell);
            let (y0, y1) = (region.y / cell, (region.y + region.height - 1) / cell);
            for y in y0..=y1.min(down.saturating_sub(1)) {
                for x in x0..=x1.min(across.saturating_sub(1)) {
                    let index = y * across + x;
                    if let Some(regions) = self.cells.get_mut(index as usize) {
                        regions.push(id as u32);
                        self.filled.push(index);
                    }
                }
            }
        }
    }

    /// Keep only the regions the innermost open layer reaches: outside any
    /// layer every region stays.
    fn reaches(&mut self) {
        let current = self.open.last().map_or(0, |&(_, stamp)| stamp);
        if current == 0 {
            return;
        }
        let mut hits = std::mem::take(&mut self.hits);
        hits.retain(|id| self.chain.get(*id as usize).copied().unwrap_or(0) == current);
        self.hits = hits;
    }

    /// The regions `bbox` can paint, into [`Self::hits`]: those the cells it
    /// covers hold and whose own geometry it overlaps. Bounds that are not
    /// finite reach all of them.
    fn hits(&mut self, bbox: Rect) {
        self.hits.clear();
        if self.reached.is_empty() {
            return;
        }
        if self.stamp == u32::MAX {
            self.reached.fill(0);
            self.stamp = 0;
        }
        self.stamp += 1;
        if !(bbox.x0.is_finite()
            && bbox.y0.is_finite()
            && bbox.x1.is_finite()
            && bbox.y1.is_finite())
        {
            self.hits.extend(0..self.reached.len() as u32);
            return;
        }
        let cell = f64::from(self.cell);
        let (across, down) = (f64::from(self.across), f64::from(self.down));
        let (x0, x1) = ((bbox.x0 / cell).floor(), (bbox.x1 / cell).floor());
        let (y0, y1) = ((bbox.y0 / cell).floor(), (bbox.y1 / cell).floor());
        if x1 < 0.0 || y1 < 0.0 || x0 >= across || y0 >= down {
            return;
        }
        let (x0, x1) = (
            x0.max(0.0) as u32,
            (x1.max(0.0) as u32).min(self.across - 1),
        );
        let (y0, y1) = (y0.max(0.0) as u32, (y1.max(0.0) as u32).min(self.down - 1));
        for y in y0..=y1 {
            for x in x0..=x1 {
                let Some(regions) = self.cells.get((y * self.across + x) as usize) else {
                    continue;
                };
                for slot in 0..regions.len() {
                    let id = regions[slot];
                    let Some(&seen) = self.reached.get(id as usize) else {
                        continue;
                    };
                    if seen == self.stamp {
                        continue;
                    }
                    if let Some(cell) = self.reached.get_mut(id as usize) {
                        *cell = self.stamp;
                    }
                    if self
                        .bounds
                        .get(id as usize)
                        .is_some_and(|region| overlaps(*region, bbox))
                    {
                        self.hits.push(id);
                    }
                }
            }
        }
    }
}

fn overlaps(a: Rect, b: Rect) -> bool {
    a.x0 < b.x1 && a.x1 > b.x0 && a.y0 < b.y1 && a.y1 > b.y0
}

#[cfg(test)]
mod tests {
    #![allow(clippy::indexing_slicing, reason = "test assertions read fixed draws")]

    use super::{Plan, Region};
    use crate::decode::DisplayList;
    use vello::Scene;
    use vello::kurbo::{Affine, Circle, Rect};
    use vello::peniko::{Color, Fill};

    fn decode(scene: &Scene) -> DisplayList {
        let mut list = DisplayList::default();
        let mut glyphs = crate::glyph::GlyphCache::default();
        let mut images = crate::image::ImageCache::default();
        let mut stats = crate::Stats::default();
        list.decode(scene.encoding(), &mut glyphs, &mut images, &mut stats);
        list
    }

    /// A draw inside clips that miss a region is not listed for it: the
    /// layer's whole span is walked past, not into.
    #[test]
    fn a_draw_under_clips_that_miss_a_region_is_not_listed_for_it() {
        let mut scene = Scene::new();
        scene.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Circle::new((90.0, 90.0), 70.0),
        );
        scene.push_clip_layer(
            Fill::NonZero,
            Affine::IDENTITY,
            &Rect::new(40.0, 0.0, 200.0, 120.0),
        );
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::BLACK,
            None,
            &Rect::new(0.0, 0.0, 300.0, 300.0),
        );
        scene.pop_layer();
        scene.pop_layer();
        scene.fill(
            Fill::NonZero,
            Affine::IDENTITY,
            Color::BLACK,
            None,
            &Rect::new(0.0, 0.0, 512.0, 384.0),
        );
        let list = decode(&scene);
        let mut plan = Plan::default();
        // Inside the clips, and outside them: the clipped draw is listed for
        // the first only.
        plan.build(&list, &[Region::new(60, 40, 20, 20)], 512, 384);
        assert_eq!(
            plan.take(0),
            vec![0, 1, 2, 3, 4, 5],
            "every layer and its draw cover the region inside the clips"
        );
        plan.build(&list, &[Region::new(250, 190, 200, 120)], 512, 384);
        assert_eq!(
            plan.take(0),
            vec![5],
            "only the draw that is not clipped away covers the region outside them"
        );
    }
}
