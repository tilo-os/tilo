//! Damage rectangles: the pixels a list of them covers, and how a long list merges into a few.

use std::cmp::Reverse;

use vello::FineDamageRect;

fn right(rect: FineDamageRect) -> u32 {
    rect.x.saturating_add(rect.width)
}

fn bottom(rect: FineDamageRect) -> u32 {
    rect.y.saturating_add(rect.height)
}

fn area(rect: FineDamageRect) -> u64 {
    u64::from(rect.width).saturating_mul(u64::from(rect.height))
}

fn contains(outer: FineDamageRect, inner: FineDamageRect) -> bool {
    outer.x <= inner.x
        && outer.y <= inner.y
        && right(outer) >= right(inner)
        && bottom(outer) >= bottom(inner)
}

fn damage_union(a: FineDamageRect, b: FineDamageRect) -> FineDamageRect {
    let x = a.x.min(b.x);
    let y = a.y.min(b.y);
    FineDamageRect {
        x,
        y,
        width: right(a).max(right(b)).saturating_sub(x),
        height: bottom(a).max(bottom(b)).saturating_sub(y),
    }
}

/// The pixels `rects` cover, each counted once however many rectangles overlap it.
///
/// Splits the plane at every left and right edge; inside each vertical slab the covering
/// rectangles are runs of rows, whose union is a sort and one pass.
pub(crate) fn damage_area(rects: &[FineDamageRect]) -> u64 {
    if let [only] = rects {
        return area(*only);
    }
    let mut edges: Vec<u32> = rects
        .iter()
        .flat_map(|rect| [rect.x, right(*rect)])
        .collect();
    edges.sort_unstable();
    edges.dedup();
    let mut rows: Vec<(u32, u32)> = Vec::with_capacity(rects.len());
    let mut total = 0_u64;
    for slab in edges.windows(2) {
        let &[left, end] = slab else { continue };
        rows.clear();
        rows.extend(
            rects
                .iter()
                .filter(|rect| rect.x <= left && right(**rect) >= end)
                .map(|rect| (rect.y, bottom(*rect))),
        );
        rows.sort_unstable();
        let mut covered = 0_u64;
        let mut run: Option<(u32, u32)> = None;
        for &(top, low) in &rows {
            run = match run {
                Some((start, stop)) if top <= stop => Some((start, stop.max(low))),
                Some((start, stop)) => {
                    covered = covered.saturating_add(u64::from(stop.saturating_sub(start)));
                    Some((top, low))
                }
                None => Some((top, low)),
            };
        }
        if let Some((start, stop)) = run {
            covered = covered.saturating_add(u64::from(stop.saturating_sub(start)));
        }
        total = total.saturating_add(covered.saturating_mul(u64::from(end.saturating_sub(left))));
    }
    total
}

/// `rects` without the ones another contains, in their original order; of equal ones the first
/// stays.
///
/// A rectangle can only be contained by one at least as large, so taking them largest first
/// means each is checked against the survivors alone and none is ever removed again.
fn without_contained(rects: Vec<FineDamageRect>) -> Vec<FineDamageRect> {
    if rects.len() < 2 {
        return rects;
    }
    let mut order: Vec<(usize, FineDamageRect)> = rects.iter().copied().enumerate().collect();
    // Width and height after area put a container first among empty rectangles too.
    order.sort_unstable_by_key(|&(index, rect)| {
        (
            Reverse(area(rect)),
            Reverse(rect.width),
            Reverse(rect.height),
            index,
        )
    });
    let mut keep = vec![false; rects.len()];
    let mut survivors: Vec<FineDamageRect> = Vec::with_capacity(rects.len());
    for (index, rect) in order {
        if survivors.iter().any(|&survivor| contains(survivor, rect)) {
            continue;
        }
        survivors.push(rect);
        if let Some(flag) = keep.get_mut(index) {
            *flag = true;
        }
    }
    rects
        .into_iter()
        .zip(keep)
        .filter_map(|(rect, kept)| kept.then_some(rect))
        .collect()
}

/// One rectangle in the merge, with the cheapest partner it knows of. What joining them costs
/// is kept apart, in [`Merge::costs`], so finding the cheapest merge is a scan of plain numbers.
#[derive(Clone, Copy)]
struct Candidate {
    rect: FineDamageRect,
    area: u64,
    live: bool,
    partner: usize,
    /// The partner was merged away and the merged rectangle costs more: another partner may be
    /// cheapest, and is looked for only if this one would be merged next. Its cost is then only
    /// a lower bound.
    stale: bool,
}

/// The rectangles being merged.
struct Merge {
    candidates: Vec<Candidate>,
    /// The least [`growth`] joining each rectangle to another live one costs; `u64::MAX` once it
    /// is merged away, so the scan for the cheapest merge passes over it.
    costs: Vec<u64>,
}

impl Merge {
    fn new(rects: Vec<FineDamageRect>) -> Self {
        let mut candidates: Vec<Candidate> = rects
            .into_iter()
            .map(|rect| Candidate {
                rect,
                area: area(rect),
                live: true,
                partner: usize::MAX,
                stale: false,
            })
            .collect();
        let mut costs = vec![u64::MAX; candidates.len()];
        // Each pair once: the cheapest partner of both ends at the same time.
        for index in 0..candidates.len() {
            let (head, tail) = candidates.split_at_mut(index.saturating_add(1));
            let (cost_head, cost_tail) = costs.split_at_mut(index.saturating_add(1));
            let (Some(first), Some(first_cost)) = (head.last_mut(), cost_head.last_mut()) else {
                continue;
            };
            for (offset, (second, second_cost)) in tail.iter_mut().zip(cost_tail).enumerate() {
                let cost = growth(first, second);
                if cost < *first_cost {
                    *first_cost = cost;
                    first.partner = index.saturating_add(1).saturating_add(offset);
                }
                if cost < *second_cost {
                    *second_cost = cost;
                    second.partner = index;
                }
            }
        }
        Self { candidates, costs }
    }

    /// The cheapest live partner of the candidate at `index`: (growth, partner).
    fn cheapest_partner(&self, index: usize) -> (u64, usize) {
        let Some(own) = self.candidates.get(index) else {
            return (u64::MAX, usize::MAX);
        };
        self.candidates
            .iter()
            .enumerate()
            .filter(|&(other, candidate)| other != index && candidate.live)
            .map(|(other, candidate)| (growth(own, candidate), other))
            .min()
            .unwrap_or((u64::MAX, usize::MAX))
    }

    /// The pair whose merge costs least, refreshing stale bounds on the way.
    fn cheapest_merge(&mut self) -> Option<(usize, usize)> {
        loop {
            let (index, &cost) = self
                .costs
                .iter()
                .enumerate()
                .min_by_key(|&(_, cost)| *cost)?;
            if cost == u64::MAX {
                return None;
            }
            let candidate = self.candidates.get(index)?;
            if !candidate.stale {
                return Some((index, candidate.partner));
            }
            let (cost, partner) = self.cheapest_partner(index);
            *self.costs.get_mut(index)? = cost;
            let candidate = self.candidates.get_mut(index)?;
            candidate.partner = partner;
            candidate.stale = false;
        }
    }

    /// Joins `gone` into `into`, then brings every other rectangle's cheapest partner up to
    /// date against the merged one: one pass, not a search over every pair.
    fn join(&mut self, into: usize, gone: usize) -> Option<()> {
        let a = *self.candidates.get(into)?;
        let b = *self.candidates.get(gone)?;
        let rect = damage_union(a.rect, b.rect);
        let merged = Candidate {
            rect,
            area: area(rect),
            live: true,
            partner: usize::MAX,
            stale: false,
        };
        *self.costs.get_mut(gone)? = u64::MAX;
        self.candidates.get_mut(gone)?.live = false;
        let mut own = (u64::MAX, usize::MAX);
        for (other, (candidate, cost)) in
            self.candidates.iter_mut().zip(&mut self.costs).enumerate()
        {
            if other == into || !candidate.live {
                continue;
            }
            let joined = growth(candidate, &merged);
            own = own.min((joined, other));
            if candidate.partner == into || candidate.partner == gone {
                // Its partner grew or went. Every other partner costs at least the old cost,
                // so the merged rectangle is cheapest whenever it costs no more than that.
                if joined <= *cost {
                    *cost = joined;
                    candidate.partner = into;
                    candidate.stale = false;
                } else {
                    candidate.stale = true;
                }
            } else if joined < *cost {
                *cost = joined;
                candidate.partner = into;
                candidate.stale = false;
            }
        }
        *self.costs.get_mut(into)? = own.0;
        *self.candidates.get_mut(into)? = Candidate {
            partner: own.1,
            ..merged
        };
        Some(())
    }

    fn into_rects(self) -> Vec<FineDamageRect> {
        self.candidates
            .into_iter()
            .filter(|candidate| candidate.live)
            .map(|candidate| candidate.rect)
            .collect()
    }
}

/// The area joining `a` and `b` into one rectangle adds beyond their own.
fn growth(a: &Candidate, b: &Candidate) -> u64 {
    area(damage_union(a.rect, b.rect)).saturating_sub(a.area.saturating_add(b.area))
}

/// `rects` without the ones another contains, merged down to at most `limit` rectangles when
/// there are more. Each merge joins the pair whose union adds the least area, so a caret blinking
/// at one place stays one small rectangle however many frames repeated it. Merging only ever
/// repaints more than the damage, which is correct; it never loses a pixel.
///
/// Every rectangle remembers its cheapest partner, so a merge costs one pass to update the
/// others against the merged rectangle instead of a search over every pair: quadratic overall
/// where the search was cubic. A rectangle whose partner was merged away and that finds the
/// merged one dearer keeps its old cost as a lower bound and looks again only when that bound is
/// the cheapest merge left.
pub(crate) fn coalesce_damage(rects: Vec<FineDamageRect>, limit: usize) -> Vec<FineDamageRect> {
    let kept = without_contained(rects);
    let limit = limit.max(1);
    if kept.len() <= limit {
        return kept;
    }
    let mut live = kept.len();
    let mut merge = Merge::new(kept);
    while live > limit {
        let Some((index, partner)) = merge.cheapest_merge() else {
            break;
        };
        if merge.join(index.min(partner), index.max(partner)).is_none() {
            break;
        }
        live = live.saturating_sub(1);
    }
    merge.into_rects()
}

#[cfg(test)]
#[allow(
    clippy::arithmetic_side_effects,
    reason = "fixture arithmetic on small canvas coordinates and a seeded generator"
)]
mod tests;
