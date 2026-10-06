//! Damage certified by the actual retained draw, rather than consumed edit flags.

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::Theme;

/// One stable draw partition, with the damage its owning widget certified.
pub struct PaintPart {
    key: String,
    rect: Rect,
    scene: Scene,
    damage: Vec<Rect>,
}

impl PaintPart {
    /// Record the scene that was actually appended and its logical geometry.
    pub fn new(key: String, rect: Rect, scene: Scene, damage: Vec<Rect>) -> Self {
        Self {
            key,
            rect,
            scene,
            damage,
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
            && self.parts.len() == other.parts.len()
            && self
                .parts
                .iter()
                .zip(&other.parts)
                .all(|(left, right)| left.key == right.key && left.rect == right.rect)
    }
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
        let changed: Vec<_> = previous
            .parts
            .iter()
            .zip(&current.parts)
            .filter_map(|(old, new)| (!old.scene.encoding_eq(&new.scene)).then_some(new))
            .collect();
        if changed.iter().any(|part| part.damage.is_empty()) {
            regions.push(self.full_bounds(fallback));
            return;
        }
        for part in changed {
            regions.extend_from_slice(&part.damage);
        }
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
