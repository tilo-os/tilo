//! Damage certified by the actual retained draw, rather than consumed edit flags.

use std::borrow::Cow;
use std::collections::HashMap;
use tilo_sort::CompactSort as _;

use vello::Scene;
use vello::kurbo::{Affine, Rect};

use crate::Theme;

use std::cell::RefCell;
use std::mem;

// A widget paints its parts every frame it draws, each part into a scene of
// its own (`PaintParts::draw_bounded`). Those scenes are worth keeping: a part
// scene is appended to the frame and then no longer read, so the next frame's
// part of the same shape draws into it instead of allocating another encoding.
// A part that drew a great deal hands its scene to the allocator instead, so
// one large draw does not pin its memory.

/// How many drawn scenes one thread keeps for the parts of the frames after.
const SPARE_SCENES: usize = 1024;

/// How much encoded drawing all the kept scenes hold together, in entries
/// of their streams: a few megabytes, however many parts a desk paints.
const SPARE_ENTRIES: usize = 64 * 1024;

/// A part that drew more than this many draw commands was no row or cell.
const SPARE_PART_TAGS: usize = 4096;

/// Roughly the memory a scene's encoding holds, in entries of its streams.
fn held(scene: &Scene) -> usize {
    let encoding = scene.encoding();
    encoding.draw_tags.capacity()
        + encoding.path_tags.capacity()
        + encoding.path_data.capacity()
        + encoding.transforms.capacity()
        + encoding.styles.capacity()
        + encoding.resources.glyphs.capacity()
}

#[derive(Default)]
struct SpareScenes {
    scenes: Vec<Scene>,
    tags: usize,
}

thread_local! {
    static DRAW_SCENES: RefCell<SpareScenes> = const { RefCell::new(SpareScenes { scenes: Vec::new(), tags: 0 }) };
}

/// A scene to draw one part into, its last frame's drawing cleared.
///
/// A thread that is past its own cleanup keeps no spares: the pool belongs
/// to the thread, so the part takes a fresh scene rather than reaching for
/// one that is already gone.
fn take_scene() -> Scene {
    DRAW_SCENES
        .try_with(|pool| {
            let mut spare = pool.borrow_mut();
            let Some(scene) = spare.scenes.pop() else {
                return Scene::default();
            };
            spare.tags = spare.tags.saturating_sub(held(&scene));
            scene
        })
        .unwrap_or_default()
}

/// The part's drawing is in the frame; keep its scene for the next one.
///
/// A part dropped while its thread tears down (a whole tree going away at the
/// end of a thread) hands its scene to the allocator: the pool is that thread's
/// and may be gone already, and a drop that reached for it would panic.
fn spare_scene(mut scene: Scene) {
    let tags = held(&scene);
    if tags > SPARE_PART_TAGS {
        return;
    }
    scene.reset();
    let _ = DRAW_SCENES.try_with(|pool| {
        let mut spare = pool.borrow_mut();
        if spare.scenes.len() < SPARE_SCENES && spare.tags + tags <= SPARE_ENTRIES {
            spare.scenes.push(scene);
            spare.tags += tags;
        }
    });
}

impl Drop for PaintPart {
    fn drop(&mut self) {
        spare_scene(mem::take(&mut self.scene));
    }
}

/// Where a part's drawing can differ from the last frame's: the bounds it
/// was drawn in, or more than one region for a part that changed position.
enum Damage {
    One(Rect),
    Many(Vec<Rect>),
}

impl Damage {
    fn is_empty(&self) -> bool {
        match self {
            Self::One(_) => false,
            Self::Many(rects) => rects.is_empty(),
        }
    }

    fn each(&self, mut visit: impl FnMut(Rect)) {
        match self {
            Self::One(rect) => visit(*rect),
            Self::Many(rects) => rects.iter().copied().for_each(visit),
        }
    }
}

impl From<Vec<Rect>> for Damage {
    fn from(mut rects: Vec<Rect>) -> Self {
        if rects.len() == 1 {
            Self::One(rects.remove(0))
        } else {
            Self::Many(rects)
        }
    }
}

/// One stable draw partition, with the damage its owning widget certified.
pub struct PaintPart {
    /// Its name in the frame: a literal as it is, a made one as it is made.
    key: Cow<'static, str>,
    rect: Rect,
    scene: Scene,
    damage: Damage,
    /// Whether `damage` covers every pixel `scene` paints, so the part may
    /// come, go or move between frames ([`Self::placed`]).
    placed: bool,
}

impl PaintPart {
    /// Record the scene that was actually appended and its logical geometry.
    /// `damage` covers where the scene can differ from the last frame's part
    /// of the same key and rect; a part that comes, goes or changes its rect
    /// repaints the whole frame.
    pub fn new(
        key: impl Into<Cow<'static, str>>,
        rect: Rect,
        scene: Scene,
        damage: Vec<Rect>,
    ) -> Self {
        Self {
            key: key.into(),
            rect,
            scene,
            damage: damage.into(),
            placed: false,
        }
    }

    /// The scene, as drawn.
    pub fn scene(&self) -> &Scene {
        &self.scene
    }

    /// [`Self::new`] for a part whose `damage` covers every pixel its scene
    /// paints (a paragraph's row, a table cell): it may come, go or move, and
    /// repaints where it was and where it is.
    pub fn placed(
        key: impl Into<Cow<'static, str>>,
        rect: Rect,
        scene: Scene,
        damage: Vec<Rect>,
    ) -> Self {
        Self {
            key: key.into(),
            rect,
            scene,
            damage: damage.into(),
            placed: true,
        }
    }

    /// [`Self::placed`] for a part whose damage is the bounds it was drawn
    /// in, as [`PaintParts`] draws them.
    fn placed_at(key: impl Into<Cow<'static, str>>, rect: Rect, scene: Scene) -> Self {
        Self {
            key: key.into(),
            rect,
            scene,
            damage: Damage::One(rect),
            placed: true,
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
                let mut finite_damage = true;
                part.damage.each(|rect| finite_damage &= rect.is_finite());
                part.rect.is_finite()
                    && part.rect.width() > 0.0
                    && part.rect.height() > 0.0
                    && finite_damage
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
            .map(|part| (part.key.as_ref(), part))
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
    // The common frame: the same parts, each where it was. Only their
    // drawings can differ, and comparing them in order answers that without
    // a map or a name lookup.
    if previous.parts.len() == current.parts.len()
        && previous
            .parts
            .iter()
            .zip(&current.parts)
            .all(|(was, is)| was.key == is.key && was.rect == is.rect)
    {
        let mut damage = Vec::new();
        for (was, is) in previous.parts.iter().zip(&current.parts) {
            if was.scene.encoding_eq(&is.scene) {
                continue;
            }
            // A part that changed and certified no damage paints the same
            // pixels: the drawing it made differs, where it shows does not.
            is.damage.each(|rect| damage.push(rect));
        }
        return Some(tidied(damage));
    }
    let old = previous.keyed()?;
    let new = current.keyed()?;
    let kept_before = previous
        .parts
        .iter()
        .filter(|part| new.contains_key(part.key.as_ref()));
    let kept_after = current
        .parts
        .iter()
        .filter(|part| old.contains_key(part.key.as_ref()));
    if !kept_before
        .map(|part| part.key.as_ref())
        .eq(kept_after.map(|part| part.key.as_ref()))
    {
        return None;
    }
    // A placed part that paints nothing changes no pixel where it is or was.
    // Compared with an empty scene: text alone encodes no paths.
    let nothing = Scene::new();
    let painting = |part: &PaintPart| !part.scene.encoding_eq(&nothing);
    let mut changed: Vec<&PaintPart> = Vec::new();
    for part in &current.parts {
        match old.get(part.key.as_ref()).copied() {
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
        if !new.contains_key(part.key.as_ref()) {
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
    let mut damage: Vec<Rect> = Vec::new();
    for part in changed {
        part.damage.each(|rect| damage.push(rect));
    }
    Some(tidied(damage))
}

/// `damage` with each region once.
fn tidied(mut damage: Vec<Rect>) -> Vec<Rect> {
    let bits = |rect: &Rect| [rect.x0, rect.y0, rect.x1, rect.y1].map(f64::to_bits);
    damage.compact_sort_by_key(bits);
    damage.dedup_by_key(|rect| bits(rect));
    damage
}

/// A self-drawn widget's draw, split into the parts of a [`PaintFrame`] as
/// it paints: each part is drawn on its own, appended to the scene the
/// widget paints in, and kept for the comparison with the last frame. A
/// widget that paints its regions by hand (a timeline, a composer, a side
/// panel) thereby reports the regions whose drawing changed, without
/// tracking what each event touched.
///
/// Everything the widget paints must be in a part: ink added to the scene
/// between parts is not compared, so the frame then proves nothing
/// ([`Self::finish`] gives no frame).
pub struct PaintParts {
    parts: Vec<PaintPart>,
    /// The scene's draw commands when the last part was appended.
    drawn: usize,
    outside: bool,
}

impl PaintParts {
    /// Start splitting the draw into `scene`, whatever it holds already.
    pub fn new(scene: &Scene) -> Self {
        Self {
            parts: Vec::new(),
            drawn: scene.encoding().draw_tags.len(),
            outside: false,
        }
    }

    /// Draw the part `key`, whose `bounds` (logical, as the widget's rect)
    /// cover every pixel `draw` paints, antialiasing and shadows included:
    /// a change inside it repaints where it was and is.
    pub fn draw<R>(
        &mut self,
        scene: &mut Scene,
        key: impl Into<Cow<'static, str>>,
        bounds: Rect,
        draw: impl FnOnce(&mut Scene) -> R,
    ) -> R {
        self.draw_bounded(scene, key, |scene| (bounds, draw(scene)))
    }

    /// [`Self::draw`] for a part whose bounds its drawing decides (a bar
    /// that measures itself as it paints): `draw` returns them with its
    /// result.
    pub fn draw_bounded<R>(
        &mut self,
        scene: &mut Scene,
        key: impl Into<Cow<'static, str>>,
        draw: impl FnOnce(&mut Scene) -> (Rect, R),
    ) -> R {
        self.outside |= scene.encoding().draw_tags.len() != self.drawn;
        let mut part = take_scene();
        let (bounds, result) = draw(&mut part);
        scene.append(&part, None);
        self.drawn = scene.encoding().draw_tags.len();
        self.parts.push(PaintPart::placed_at(key, bounds, part));
        result
    }

    /// The frame drawn, with the `geometry`, `transform` and `theme` it was
    /// drawn with; `None` when something was painted outside every part.
    pub fn finish(
        self,
        scene: &Scene,
        geometry: Vec<Rect>,
        transform: Affine,
        theme: &Theme,
    ) -> Option<PaintFrame> {
        let outside = self.outside || scene.encoding().draw_tags.len() != self.drawn;
        (!outside).then(|| PaintFrame::new(geometry, transform, theme, Scene::new(), self.parts))
    }
}

/// Retains only draw caches; no document state, history or input is stored here.
#[derive(Default)]
pub struct PaintDamage {
    previous: Option<Box<PaintFrame>>,
    current: Option<Box<PaintFrame>>,
    recorded: bool,
    previous_bounds: Option<Rect>,
    current_bounds: Option<Rect>,
}

impl PaintDamage {
    /// A missing proof explicitly requests conservative full bounds.
    pub fn record(&mut self, bounds: Rect, frame: Option<PaintFrame>) {
        self.current_bounds = Some(bounds);
        self.current = frame.map(Box::new);
        self.recorded = true;
    }

    /// Whether this draw supplied a proof or explicitly requested full bounds.
    pub fn is_recorded(&self) -> bool {
        self.recorded
    }

    /// Collect after drawing and before `mark_drawn`, as the native host does.
    pub fn collect(&self, fallback: Rect, regions: &mut Vec<Rect>) {
        let Some((previous, current)) = self.previous.as_deref().zip(self.current.as_deref())
        else {
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

    fn placed(key: &'static str, top: f64) -> PaintPart {
        PaintPart::placed(key, row(top), filled(row(top)), vec![row(top)])
    }

    /// A part that is not placed, painting its row.
    fn unplaced(key: &'static str, top: f64) -> PaintPart {
        PaintPart::new(key, row(top), filled(row(top)), vec![row(top)])
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
        let empty = |top| PaintPart::placed("handles", row(top), Scene::new(), Vec::new());
        let repainted = damage(
            vec![placed("a", 0.0), empty(0.0)],
            vec![placed("a", 0.0), empty(20.0)],
        );
        assert!(repainted.is_empty(), "{repainted:?}");
    }

    #[test]
    fn parts_that_are_not_placed_still_repaint_whole_when_they_move_come_or_go() {
        assert_eq!(
            damage(vec![unplaced("a", 0.0)], vec![unplaced("a", 20.0)]),
            vec![BOUNDS]
        );
        assert_eq!(
            damage(
                vec![unplaced("a", 0.0)],
                vec![unplaced("a", 0.0), unplaced("b", 20.0)]
            ),
            vec![BOUNDS]
        );
        assert_eq!(
            damage(
                vec![unplaced("a", 0.0), unplaced("b", 20.0)],
                vec![unplaced("a", 0.0)]
            ),
            vec![BOUNDS]
        );
        // Changed in place: what it certified.
        let changed = PaintPart::new("a", row(0.0), filled(row(1.0)), vec![row(0.0)]);
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

    /// A hand-drawn widget's frame through [`PaintParts`]: `rows` filled,
    /// each its own part, and `ink` painted between parts when set.
    fn hand_drawn(damage: &mut PaintDamage, rows: &[f64], ink: bool) {
        const KEYS: [&str; 3] = ["a", "b", "c"];
        let theme = Theme::dark();
        let mut scene = Scene::new();
        let mut parts = PaintParts::new(&scene);
        for (key, top) in KEYS.iter().zip(rows) {
            parts.draw(&mut scene, *key, row(*top), |scene| {
                scene.append(&filled(row(*top)), None);
            });
            if ink {
                scene.append(&filled(row(*top)), None);
            }
        }
        let frame = parts.finish(&scene, vec![BOUNDS], Affine::IDENTITY, &theme);
        damage.mark_drawn();
        damage.record(BOUNDS, frame);
    }

    #[test]
    fn hand_drawn_parts_damage_the_part_that_changed_and_ink_outside_them_all() {
        let mut damage = PaintDamage::default();
        hand_drawn(&mut damage, &[0.0, 40.0], false);
        hand_drawn(&mut damage, &[0.0, 60.0], false);
        let mut regions = Vec::new();
        damage.collect(BOUNDS, &mut regions);
        assert_eq!(regions, vec![row(40.0), row(60.0)]);

        hand_drawn(&mut damage, &[0.0, 60.0], true);
        let mut regions = Vec::new();
        damage.collect(BOUNDS, &mut regions);
        assert_eq!(
            regions,
            vec![BOUNDS],
            "ink outside the parts proves nothing"
        );
    }
}
