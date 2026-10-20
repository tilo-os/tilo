//! Damage certified by the actual retained draw, rather than consumed edit flags.

use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::Theme;

/// One stable draw partition, with the damage its owning widget certified.
pub struct PaintPart {
    key: String,
    rect: Rect,
    scene: Scene,
    damage: Vec<Rect>,
    /// Whether `damage` covers every pixel `scene` paints, so the part may
    /// come, go or move between frames ([`Self::placed`]).
    placed: bool,
}

impl PaintPart {
    /// Record the scene that was actually appended and its logical geometry.
    /// `damage` covers where the scene can differ from the last frame's part
    /// of the same key and rect; a part that comes, goes or changes its rect
    /// repaints the whole frame.
    pub fn new(key: String, rect: Rect, scene: Scene, damage: Vec<Rect>) -> Self {
        Self {
            key,
            rect,
            scene,
            damage,
            placed: false,
        }
    }

    /// [`Self::new`] for a part whose `damage` covers every pixel its scene
    /// paints (a paragraph's row, a table cell): it may come, go or move, and
    /// repaints where it was and where it is.
    pub fn placed(key: String, rect: Rect, scene: Scene, damage: Vec<Rect>) -> Self {
        Self {
            placed: true,
            ..Self::new(key, rect, scene, damage)
        }
    }
}

/// A complete draw partitioning: unchanged outside paint plus stable children.
pub struct PaintFrame {
    geometry: Vec<Rect>,
    transform: Affine,
    theme: Theme,
    outside: Scene,
    parts: Vec<PaintPart>,
}

impl PaintFrame {
    /// `geometry` includes all clips and layout bounds affecting these draws.
    pub fn new(
        geometry: Vec<Rect>,
        transform: Affine,
        theme: &Theme,
        outside: Scene,
        parts: Vec<PaintPart>,
    ) -> Self {
        Self {
            geometry,
            transform,
            theme: theme.clone(),
            outside,
            parts,
        }
    }

    fn finite(&self) -> bool {
        !self.geometry.is_empty()
            && self.geometry.iter().all(|rect| rect.is_finite())
            && self
                .transform
                .as_coeffs()
                .iter()
                .all(|value| value.is_finite())
            && self.transform.determinant() != 0.0
            && self.parts.iter().all(|part| {
                part.rect.is_finite()
                    && part.rect.width() > 0.0
                    && part.rect.height() > 0.0
                    && part.damage.iter().all(|rect| rect.is_finite())
            })
    }

    fn matches_geometry(&self, other: &Self) -> bool {
        self.finite()
            && other.finite()
            && self.geometry == other.geometry
            && self.transform == other.transform
            && self.theme == other.theme
    }

    /// Each part by its key; `None` when two parts share one.
    fn keyed(&self) -> Option<HashMap<&str, &PaintPart>> {
        let keyed: HashMap<_, _> = self
            .parts
            .iter()
            .map(|part| (part.key.as_str(), part))
            .collect();
        (keyed.len() == self.parts.len()).then_some(keyed)
    }
}

/// Where `current` differs from `previous`, part by part, matched by key: a
/// part whose scene changed in place damages what it certified; a placed
/// part ([`PaintPart::placed`]) that came damages where it is, one that went
/// where it was, and one that moved or changed where it was and is. `None`
/// when that cannot be said: a key repeats, the parts both frames hold are
/// painted in another order, a part that is not placed came, went or moved,
/// or a part that changed certified no damage.
fn changed_parts(previous: &PaintFrame, current: &PaintFrame) -> Option<Vec<Rect>> {
    let old = previous.keyed()?;
    let new = current.keyed()?;
    let kept_before = previous
        .parts
        .iter()
        .filter(|part| new.contains_key(part.key.as_str()));
    let kept_after = current
        .parts
        .iter()
        .filter(|part| old.contains_key(part.key.as_str()));
    if !kept_before
        .map(|part| part.key.as_str())
        .eq(kept_after.map(|part| part.key.as_str()))
    {
        return None;
    }
    // A placed part that paints nothing changes no pixel where it is or was.
    // Compared with an empty scene: text alone encodes no paths.
    let nothing = Scene::new();
    let painting = |part: &PaintPart| !part.scene.encoding_eq(&nothing);
    let mut changed: Vec<&PaintPart> = Vec::new();
    for part in &current.parts {
        match old.get(part.key.as_str()).copied() {
            Some(was) if was.rect == part.rect && was.scene.encoding_eq(&part.scene) => {}
            Some(was) if was.placed && part.placed => {
                changed.extend([was, part].into_iter().filter(|part| painting(part)));
            }
            Some(was) if was.rect == part.rect => changed.push(part),
            None if part.placed => changed.extend(painting(part).then_some(part)),
            // Moved, came or went without saying where it painted.
            Some(_) | None => return None,
        }
    }
    for part in &previous.parts {
        if !new.contains_key(part.key.as_str()) {
            if !part.placed {
                return None;
            }
            changed.extend(painting(part).then_some(part));
        }
    }
    if changed.iter().any(|part| part.damage.is_empty()) {
        return None;
    }
    // A part that changed in place damages the same rows it was and is:
    // each once.
    let mut damage: Vec<Rect> = changed
        .into_iter()
        .flat_map(|part| part.damage.iter().copied())
        .collect();
    let bits = |rect: &Rect| [rect.x0, rect.y0, rect.x1, rect.y1].map(f64::to_bits);
    damage.compact_sort_by_key(bits);
    damage.dedup_by_key(|rect| bits(rect));
    Some(damage)
}

/// Retains only draw caches; no document state, history or input is stored here.
#[derive(Default)]
pub struct PaintDamage {
    previous: Option<PaintFrame>,
    current: Option<PaintFrame>,
    recorded: bool,
    previous_bounds: Option<Rect>,
    current_bounds: Option<Rect>,
}

impl PaintDamage {
    /// A missing proof explicitly requests conservative full bounds.
    pub fn record(&mut self, bounds: Rect, frame: Option<PaintFrame>) {
        self.current_bounds = Some(bounds);
        self.current = frame;
        self.recorded = true;
    }

    /// Whether this draw supplied a proof or explicitly requested full bounds.
    pub fn is_recorded(&self) -> bool {
        self.recorded
    }

    /// Collect after drawing and before `mark_drawn`, as the native host does.
    pub fn collect(&self, fallback: Rect, regions: &mut Vec<Rect>) {
        let Some((previous, current)) = self.previous.as_ref().zip(self.current.as_ref()) else {
            regions.push(self.full_bounds(fallback));
            return;
        };
        if !previous.matches_geometry(current) || !previous.outside.encoding_eq(&current.outside) {
            regions.push(self.full_bounds(fallback));
            return;
        }
        match changed_parts(previous, current) {
            Some(changed) => regions.extend(changed),
            None => regions.push(self.full_bounds(fallback)),
        }
    }

    /// Whether this frame's paint was recorded, so [`Self::collect`] covers
    /// every pixel it changed: where its parts were and are, or its whole
    /// extent then and now.
    pub fn covers_own_bounds(&self) -> bool {
        self.recorded
    }

    fn full_bounds(&self, fallback: Rect) -> Rect {
        self.previous_bounds
            .filter(|bounds| bounds.is_finite())
            .map_or(fallback, |bounds| fallback.union(bounds))
    }

    /// Drop duplicate encoded scenes when an outer cache owns the draw.
    /// Bounds remain available to clear old ink on the next full rebuild.
    pub fn release_scene_cache(&mut self) {
        self.previous = None;
        self.current = None;
        self.recorded = false;
    }

    /// Adopt only the frame the ordinary draw lifecycle accepted.
    pub fn mark_drawn(&mut self) {
        if self.recorded {
            self.previous = self.current.take();
            self.previous_bounds = self.current_bounds.take();
            self.recorded = false;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A scene that fills `rect` with an opaque colour.
    fn filled(rect: Rect) -> Scene {
        let mut scene = Scene::new();
        scene.fill(
            vello::peniko::Fill::NonZero,
            Affine::IDENTITY,
            vello::peniko::Color::from_rgba8(200, 100, 50, 255),
            None,
            &rect,
        );
        scene
    }

    fn row(top: f64) -> Rect {
        Rect::new(0.0, top, 100.0, top + 20.0)
    }

    /// A placed part painting its row.
    fn placed(key: &str, top: f64) -> PaintPart {
        PaintPart::placed(key.into(), row(top), filled(row(top)), vec![row(top)])
    }

    /// A part that is not placed, painting its row.
    fn unplaced(key: &str, top: f64) -> PaintPart {
        PaintPart::new(key.into(), row(top), filled(row(top)), vec![row(top)])
    }

    /// Where the editor painted, the whole of it.
    const BOUNDS: Rect = Rect::new(0.0, 0.0, 100.0, 400.0);

    /// What a frame of `next` repaints after a frame of `parts`.
    fn damage(parts: Vec<PaintPart>, next: Vec<PaintPart>) -> Vec<Rect> {
        let theme = Theme::dark();
        let geometry = vec![Rect::new(0.0, 0.0, 100.0, 0.0)];
        let mut damage = PaintDamage::default();
        for parts in [parts, next] {
            damage.mark_drawn();
            damage.record(
                BOUNDS,
                Some(PaintFrame::new(
                    geometry.clone(),
                    Affine::IDENTITY,
                    &theme,
                    Scene::new(),
                    parts,
                )),
            );
        }
        let mut regions = Vec::new();
        damage.collect(BOUNDS, &mut regions);
        regions
    }

    #[test]
    fn a_placed_part_that_came_damages_where_it_is_and_moves_the_parts_below() {
        // "b" came between "a" and "c"; "c" moved down under it.
        let repainted = damage(
            vec![placed("a", 0.0), placed("c", 20.0)],
            vec![placed("a", 0.0), placed("b", 20.0), placed("c", 40.0)],
        );
        assert_eq!(repainted, vec![row(20.0), row(40.0)]);
    }

    #[test]
    fn a_placed_part_that_went_damages_where_it_was() {
        let repainted = damage(
            vec![placed("a", 0.0), placed("b", 20.0), placed("c", 40.0)],
            vec![placed("a", 0.0), placed("c", 20.0)],
        );
        assert_eq!(repainted, vec![row(20.0), row(40.0)]);
    }

    #[test]
    fn a_placed_part_that_paints_nothing_damages_nothing_where_it_goes() {
        let empty = |top| PaintPart::placed("handles".into(), row(top), Scene::new(), Vec::new());
        let repainted = damage(
            vec![placed("a", 0.0), empty(0.0)],
            vec![placed("a", 0.0), empty(20.0)],
        );
        assert!(repainted.is_empty(), "{repainted:?}");
    }

    #[test]
    fn parts_that_are_not_placed_still_repaint_whole_when_they_move_come_or_go() {
        // Moved.
        assert_eq!(
            damage(vec![unplaced("a", 0.0)], vec![unplaced("a", 20.0)]),
            vec![BOUNDS]
        );
        // Came.
        assert_eq!(
            damage(
                vec![unplaced("a", 0.0)],
                vec![unplaced("a", 0.0), unplaced("b", 20.0)]
            ),
            vec![BOUNDS]
        );
        // Went.
        assert_eq!(
            damage(
                vec![unplaced("a", 0.0), unplaced("b", 20.0)],
                vec![unplaced("a", 0.0)]
            ),
            vec![BOUNDS]
        );
        // Changed in place: what it certified.
        let changed = PaintPart::new("a".into(), row(0.0), filled(row(1.0)), vec![row(0.0)]);
        assert_eq!(
            damage(vec![unplaced("a", 0.0)], vec![changed]),
            vec![row(0.0)]
        );
    }

    #[test]
    fn repeated_keys_or_reordered_parts_repaint_whole() {
        assert_eq!(
            damage(
                vec![placed("a", 0.0), placed("b", 20.0)],
                vec![placed("b", 0.0), placed("a", 20.0)]
            ),
            vec![BOUNDS]
        );
        assert_eq!(
            damage(
                vec![placed("a", 0.0)],
                vec![placed("a", 0.0), placed("a", 20.0)]
            ),
            vec![BOUNDS]
        );
    }
}
