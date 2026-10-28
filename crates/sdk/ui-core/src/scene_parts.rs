//! Keyed parts of a shell scene section, so a small change inside a section
//! damages that change's bounds instead of the whole display.
//!
//! The retained-damage producer proves a frame sparse by comparing each scene
//! section with the previous frame's. Without parts, any difference inside the
//! workspace (a caret, a peer's cursor, a hover) made the whole section opaque
//! to that proof. A part marks a stretch of a section's draw commands with a
//! key and the bounds those commands can paint. Every part boundary splits the
//! section's encoded streams into pieces; a piece belongs to the innermost part
//! open across it, or to the section itself. Comparing the previous and current
//! frame piece by piece then gives:
//!
//! - every piece equal: the section is unchanged;
//! - only pieces of parts differ: damage is each changed part's old and new
//!   bounds;
//! - a piece of the section itself differs, the parts are not the same keys in
//!   the same order, a changed part has no bounds, or anything cannot be
//!   proven: no authority, so the frame is dense.
//!
//! Correctness rests on two facts. Pixels outside every changed part's bounds
//! are produced by byte-identical commands in both frames, so they cannot have
//! changed (the sparse renderer re-renders the whole scene inside the damage,
//! so blend layers and filters above a part compose correctly there). And a
//! part's commands are independent of what precedes them: the encoder dedups a
//! transform or style against the last one encoded, so each boundary of a
//! tracked section forces the next transform and style to be encoded again.
//!
//! Only sections the producer registered ([`track`]) are split; a part drawn
//! into any other scene (a capture, a preview, a cached sub-scene) records
//! nothing and changes no encoding.

use std::cell::RefCell;
use std::hash::{Hash, Hasher};

use vello::Scene;
use vello::kurbo::Rect;
use vello_encoding::{Encoding, Patch};

/// Bounds what a section draws outside its parts ([`damage_with_copies`],
/// [`drawn_bounds`]); keep one across frames.
pub use tilo_cpu_raster::DrawBounds;

/// Identifies one part among the parts of a section. Two frames' parts are
/// matched by key and position, so a key must name the same thing in both.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct PartKey(u64, &'static str, bool);

impl std::fmt::Debug for PartKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}({})", self.1, self.0)
    }
}

impl PartKey {
    /// The key as a number: what a [`DamageHint::source`] of a part keyed
    /// like this is.
    pub const fn value(self) -> u64 {
        self.0
    }

    /// A key for `kind` (a fixed name for the draw site) and `id`, which tells
    /// apart several parts of the same kind (a window's id, a peer's index).
    pub fn new(kind: &'static str, id: impl Hash) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        kind.hash(&mut hasher);
        id.hash(&mut hasher);
        Self(hasher.finish(), kind, false)
    }

    /// [`Self::new`] for a part that is a layer of its own
    /// ([`partition`]): a host that keeps its pixels draws it into a target
    /// of its own and composites it, so a frame that only moves it
    /// rasterizes nothing.
    pub fn layer(kind: &'static str, id: impl Hash) -> Self {
        Self(Self::new(kind, id).0, kind, true)
    }

    /// Whether this part is a layer of its own ([`Self::layer`]).
    pub fn is_layer(self) -> bool {
        self.2
    }
}

/// Lengths of every encoded stream and resource list at one boundary.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct Marks {
    path_tags: usize,
    path_data: usize,
    draw_tags: usize,
    draw_data: usize,
    transforms: usize,
    styles: usize,
    patches: usize,
    color_stops: usize,
    glyphs: usize,
    glyph_runs: usize,
    normalized_coords: usize,
}

impl Marks {
    fn of(encoding: &Encoding) -> Self {
        Self {
            path_tags: encoding.path_tags.len(),
            path_data: encoding.path_data.len(),
            draw_tags: encoding.draw_tags.len(),
            draw_data: encoding.draw_data.len(),
            transforms: encoding.transforms.len(),
            styles: encoding.styles.len(),
            patches: encoding.resources.patches.len(),
            color_stops: encoding.resources.color_stops.len(),
            glyphs: encoding.resources.glyphs.len(),
            glyph_runs: encoding.resources.glyph_runs.len(),
            normalized_coords: encoding.resources.normalized_coords.len(),
        }
    }

    /// `self` as the marks of a scene that began where `start` was.
    fn rebased(self, start: Self) -> Self {
        Self {
            path_tags: self.path_tags.saturating_sub(start.path_tags),
            path_data: self.path_data.saturating_sub(start.path_data),
            draw_tags: self.draw_tags.saturating_sub(start.draw_tags),
            draw_data: self.draw_data.saturating_sub(start.draw_data),
            transforms: self.transforms.saturating_sub(start.transforms),
            styles: self.styles.saturating_sub(start.styles),
            patches: self.patches.saturating_sub(start.patches),
            color_stops: self.color_stops.saturating_sub(start.color_stops),
            glyphs: self.glyphs.saturating_sub(start.glyphs),
            glyph_runs: self.glyph_runs.saturating_sub(start.glyph_runs),
            normalized_coords: self
                .normalized_coords
                .saturating_sub(start.normalized_coords),
        }
    }

    /// `marks` of a scene appended where `self` was the length so far.
    fn offset(self, marks: Self) -> Self {
        Self {
            path_tags: self.path_tags.saturating_add(marks.path_tags),
            path_data: self.path_data.saturating_add(marks.path_data),
            draw_tags: self.draw_tags.saturating_add(marks.draw_tags),
            draw_data: self.draw_data.saturating_add(marks.draw_data),
            transforms: self.transforms.saturating_add(marks.transforms),
            styles: self.styles.saturating_add(marks.styles),
            patches: self.patches.saturating_add(marks.patches),
            color_stops: self.color_stops.saturating_add(marks.color_stops),
            glyphs: self.glyphs.saturating_add(marks.glyphs),
            glyph_runs: self.glyph_runs.saturating_add(marks.glyph_runs),
            normalized_coords: self
                .normalized_coords
                .saturating_add(marks.normalized_coords),
        }
    }
}

#[derive(Clone, Debug)]
enum Boundary {
    Begin(PartKey),
    /// The bounds, in section coordinates, of everything the part drew.
    End(Vec<Rect>),
    /// The end of a part with no bounds of its own ([`hinted`]): a change
    /// inside it damages what its hint says changed when the hint follows
    /// on from the compared frame's, and its enclosing part otherwise.
    EndHinted(Option<DamageHint>),
}

/// What changed in a part's content between two generations of it, as the
/// one who painted it knows: an embedded app's own widget damage.
#[derive(Clone, Debug, PartialEq)]
pub struct DamageHint {
    /// Who painted the content. A hint follows on only within one source.
    pub source: u64,
    pub from: u64,
    /// The generation painted.
    pub to: u64,
    /// Where the content is placed: content placed elsewhere has moved.
    pub placement: vello::kurbo::Affine,
    /// Every pixel that differs between generations `from` and `to`, in the
    /// section's coordinates, antialiasing, shadows and overhang included,
    /// except where `copies` land.
    pub rects: Vec<Rect>,
    /// What of generation `from` generation `to` shows moved, in the
    /// section's coordinates ([`crate::scroll_copy`]): a host that keeps the
    /// pixels it presented may move them instead of repainting where they
    /// land ([`damage_with_copies`]). Any other host repaints there.
    pub copies: Vec<crate::scroll_copy::MovedRegion>,
    /// What differs between each earlier generation still known and `to`,
    /// as `rects` does for `from`: a host that presented an older
    /// generation (the content was drawn more than once between two of its
    /// frames) repaints these instead of the whole part.
    pub earlier: Vec<(u64, Vec<Rect>)>,
}

impl DamageHint {
    /// The move `self` reports, when what it draws is the content of the
    /// frame before drawn somewhere else: the one copy it carries. A host
    /// that keeps this content's pixels in a target of its own
    /// ([`crate::cpu_layers`]) draws those pixels moved, instead of drawing
    /// the content again. The rects it also carries ([`Self::rects`]) are
    /// what a host drawing one target repaints around such a copy; a host
    /// drawing the layer alone has those pixels already.
    pub fn moved_by(&self) -> Option<(f64, f64)> {
        let [copy] = self.copies.as_slice() else {
            return None;
        };
        Some((copy.delta.x, copy.delta.y))
    }

    /// `self`'s damage when it describes the change from `previous`'s
    /// content: the same source placed in the same place, painting the
    /// generation that follows the one `previous` painted.
    fn follows(&self, previous: &Self) -> bool {
        // The generation this one was measured from is the one painted then.
        let next_generation = previous.to == self.from;
        next_generation
            && self.source == previous.source
            && self.placement == previous.placement
            && !(self.rects.is_empty() && self.copies.is_empty())
    }

    /// `self`'s damage since `previous` when that painted an earlier
    /// generation of the same source in the same place.
    fn reaches(&self, previous: &Self) -> Option<&[Rect]> {
        if self.source != previous.source || self.placement != previous.placement {
            return None;
        }
        self.earlier
            .iter()
            .find(|(from, rects)| *from == previous.to && !rects.is_empty())
            .map(|(_, rects)| rects.as_slice())
    }

    /// What changed for a host that makes no copy: the rects, and where
    /// every copy lands.
    fn repainted(&self) -> Vec<Rect> {
        self.rects
            .iter()
            .copied()
            .chain(self.copies.iter().map(|copy| copy.destination().expand()))
            .collect()
    }

    fn transformed(&self, transform: Option<vello::kurbo::Affine>) -> Self {
        let Some(transform) = transform else {
            return self.clone();
        };
        let mut rects: Vec<Rect> = self
            .rects
            .iter()
            .map(|rect| transform.transform_rect_bbox(*rect))
            .collect();
        let mut copies = Vec::with_capacity(self.copies.len());
        for copy in &self.copies {
            match copy.transformed(transform) {
                Some(moved) => copies.push(moved),
                // Moved under a rotation, the copy is repainted.
                None => rects.push(transform.transform_rect_bbox(copy.destination()).expand()),
            }
        }
        let earlier = self
            .earlier
            .iter()
            .map(|(from, rects)| {
                let rects = rects
                    .iter()
                    .map(|rect| transform.transform_rect_bbox(*rect))
                    .collect();
                (*from, rects)
            })
            .collect();
        Self {
            placement: transform * self.placement,
            rects,
            copies,
            earlier,
            ..self.clone()
        }
    }
}

/// A copy a hinted part's content made ([`DamageHint::copies`]), as a host
/// presenting the section over its last frame must check it: what it moves,
/// what the section draws over that content, and how it composites.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionCopy {
    /// What moved, in section coordinates.
    pub moved: crate::scroll_copy::MovedRegion,
    /// Where the section draws after the content the copy moves, over it:
    /// bounded parts' bounds and the draw bounds of everything else. `None`
    /// when some of it cannot be bounded.
    pub above: Option<Vec<Rect>>,
    /// Whether every layer open around the content is a plain clip or a
    /// source-over group at full opacity, so its pixels are its own.
    pub plain_layers: bool,
}

/// [`damage_with_copies`]: where a section changed, and the copies that
/// explain the rest.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct SectionDamage {
    /// Where the section changed, except where the copies land.
    pub rects: Vec<Rect>,
    pub copies: Vec<SectionCopy>,
}

impl SectionDamage {
    /// Where the section changed for a host that makes none of the copies.
    pub fn without_copies(self) -> Vec<Rect> {
        let mut rects = self.rects;
        for copy in self.copies {
            let landed = copy.moved.destination().expand();
            if !rects.contains(&landed) {
                rects.push(landed);
            }
        }
        rects
    }
}

/// How a host presents a frame over the pixels of the one before:
/// [`present_copies`].
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Presented {
    /// The copies to make within the target, in order, before anything is
    /// repainted.
    pub copies: Vec<crate::scroll_copy::DeviceCopy>,
    /// What to repaint after them, in device pixels.
    pub raster: Vec<Rect>,
}

impl Presented {
    /// Every pixel the frame changes: what is repainted and where the
    /// copies land, in device pixels.
    pub fn changed(&self) -> Vec<Rect> {
        self.raster
            .iter()
            .copied()
            .chain(
                self.copies
                    .iter()
                    .map(crate::scroll_copy::DeviceCopy::destination_rect),
            )
            .collect()
    }
}

/// Decide which of a frame's section copies ([`damage_with_copies`]) a host
/// that keeps its target makes, and what it repaints around them. `damage`
/// is every other change of the frame, in device pixels; `above` is where
/// the scene draws over the section after it (later sections), in section
/// coordinates; `to_device` maps section coordinates into the `size`
/// target.
///
/// A copy is made only when its content composites on its own (plain layers
/// around it), everything drawn over it is bounded, its delta is a whole
/// number of device pixels and it meets no earlier copy. What is drawn over
/// it (in the section or above) is repainted where it is and, carried, where
/// the copy took its pixels ([`crate::scroll_copy::carry`]), as is every
/// other change a copy reads; a copy not made repaints where it lands.
pub fn present_copies(
    copies: &[SectionCopy],
    mut damage: Vec<Rect>,
    above: &[Rect],
    to_device: vello::kurbo::Affine,
    size: (u32, u32),
) -> Presented {
    let target = Rect::new(0.0, 0.0, f64::from(size.0), f64::from(size.1));
    let device = |rect: Rect| {
        to_device
            .transform_rect_bbox(rect)
            .expand()
            .intersect(target)
    };
    let mut made = Vec::new();
    let mut claimed: Vec<Rect> = Vec::new();
    for copy in copies {
        let reach = copy.moved.reach();
        let planned = copy
            .above
            .as_ref()
            .filter(|_| copy.plain_layers)
            .and_then(|drawn| Some((drawn, copy.moved.device(to_device, size)?)))
            .filter(|_| {
                claimed
                    .iter()
                    .all(|area| area.intersect(device(reach)).area() <= 0.0)
            });
        let Some((drawn, (made_copy, fringe))) = planned else {
            damage.push(device(copy.moved.destination()));
            continue;
        };
        claimed.push(device(reach));
        made.push(made_copy);
        damage.extend(fringe);
        for over in drawn.iter().chain(above) {
            let covered = over.intersect(reach);
            if covered.width() > 0.0 && covered.height() > 0.0 {
                damage.push(device(covered));
            }
        }
    }
    damage.retain(|rect| rect.width() > 0.0 && rect.height() > 0.0);
    crate::scroll_copy::carry(&made, &mut damage);
    Presented {
        copies: made,
        raster: damage,
    }
}

#[derive(Clone, Debug)]
struct Event {
    boundary: Boundary,
    marks: Marks,
}

/// The parts recorded for one section in one frame.
#[derive(Clone, Debug, Default)]
pub struct SectionParts {
    events: Vec<Event>,
}

struct Log {
    scene: *const Scene,
    events: Vec<Event>,
    open: usize,
    broken: bool,
}

thread_local! {
    static RECORDING: RefCell<Option<Vec<Log>>> = const { RefCell::new(None) };
}

thread_local! {
    /// Where each part's coordinates sit relative to the screen's, by part
    /// key: a strip drawn for a presenter that keeps its canvas
    /// ([`crate::retained_present::unbake_scroll`]) draws its windows where
    /// they rest, and the presenter moves the canvas. Noted while drawing
    /// ([`note_layer_offset`]) and read when the frame is split into layers
    /// ([`partition`]).
    static OFFSETS: RefCell<Vec<(u64, (f64, f64))>> = const { RefCell::new(Vec::new()) };
}

/// Note that the content part `key` draws is `offset` from where the screen
/// shows it: negative the shift a strip drawn in canvas coordinates adds to
/// its windows' places. A host that keeps each layer's pixels in a target of
/// its own composites it where the screen shows it.
pub fn note_layer_offset(key: PartKey, offset: (f64, f64)) {
    OFFSETS.with_borrow_mut(|offsets| {
        match offsets.iter_mut().find(|(held, _)| *held == key.value()) {
            Some((_, held)) => *held = offset,
            None => offsets.push((key.value(), offset)),
        }
    });
}

/// The offset noted for `key` ([`note_layer_offset`]), if one was.
fn layer_offset(key: PartKey) -> Option<(f64, f64)> {
    OFFSETS.with_borrow(|offsets| {
        offsets
            .iter()
            .find(|(held, _)| *held == key.value())
            .map(|(_, offset)| *offset)
    })
}

/// Records parts for the sections registered with [`track`] until finished.
pub struct Recording {
    previous: Option<Vec<Log>>,
    finished: bool,
}

impl Recording {
    pub fn begin() -> Self {
        Self {
            previous: RECORDING.with(|slot| slot.replace(Some(Vec::new()))),
            finished: false,
        }
    }

    pub fn finish(mut self) -> Recorded {
        self.finished = true;
        Recorded(
            RECORDING
                .with(|slot| slot.replace(self.previous.take()))
                .unwrap_or_default(),
        )
    }
}

impl Drop for Recording {
    fn drop(&mut self) {
        if !self.finished {
            RECORDING.with(|slot| slot.replace(self.previous.take()));
        }
    }
}

/// Every tracked section's parts from one finished [`Recording`].
pub struct Recorded(Vec<Log>);

impl Recorded {
    /// The parts of `scene`, or `None` when it was not tracked or its parts
    /// were not well nested (a part left open, an end without a begin).
    pub fn of(&self, scene: &Scene) -> Option<SectionParts> {
        let log = self.0.iter().find(|log| std::ptr::eq(log.scene, scene))?;
        (!log.broken && log.open == 0).then(|| SectionParts {
            events: log.events.clone(),
        })
    }
}

/// Split `scene` at its parts' boundaries for the active recording. The
/// section must stay at its address until the recording finishes.
pub fn track(scene: &Scene) {
    RECORDING.with(|slot| {
        if let Some(logs) = slot.borrow_mut().as_mut()
            && !logs.iter().any(|log| std::ptr::eq(log.scene, scene))
        {
            logs.push(Log {
                scene,
                events: Vec::new(),
                open: 0,
                broken: false,
            });
        }
    });
}

fn boundary(scene: &mut Scene, boundary: Boundary) {
    let tracked = RECORDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(log) = slot
            .as_mut()
            .and_then(|logs| logs.iter_mut().find(|log| std::ptr::eq(log.scene, scene)))
        else {
            return false;
        };
        match boundary {
            Boundary::Begin(_) => log.open = log.open.saturating_add(1),
            Boundary::End(_) | Boundary::EndHinted(_) => match log.open.checked_sub(1) {
                Some(open) => log.open = open,
                None => log.broken = true,
            },
        }
        log.events.push(Event {
            boundary,
            marks: Marks::of(scene.encoding()),
        });
        true
    });
    if tracked {
        // Neither side of a boundary may depend on the other's last
        // transform or style.
        scene.encoding_mut().force_next_transform_and_style();
    }
}

/// Draw one part of a section: `bounds`, in the section's coordinates (the
/// draw transform already applied), must cover every pixel `draw` can paint,
/// shadows and antialiasing included. An empty `bounds` claims `draw` paints
/// nothing; a part that then draws anything is treated as unbounded.
pub fn part<R>(
    scene: &mut Scene,
    key: PartKey,
    bounds: Vec<Rect>,
    draw: impl FnOnce(&mut Scene) -> R,
) -> R {
    boundary(scene, Boundary::Begin(key));
    let result = draw(scene);
    boundary(scene, Boundary::End(bounds));
    result
}

/// Whether `scene` is split into parts for the active recording.
pub fn is_tracked(scene: &Scene) -> bool {
    RECORDING.with(|slot| {
        slot.borrow()
            .as_ref()
            .is_some_and(|logs| logs.iter().any(|log| std::ptr::eq(log.scene, scene)))
    })
}

/// Begin a part keyed `key` around draws too long to pass to [`part`] as one
/// closure; [`end`] closes it, under the same contract. A part left open
/// gives the section no parts.
pub fn begin(scene: &mut Scene, key: PartKey) {
    boundary(scene, Boundary::Begin(key));
}

/// Close the part [`begin`] opened, with the bounds of what it painted.
pub fn end(scene: &mut Scene, bounds: Vec<Rect>) {
    boundary(scene, Boundary::End(bounds));
}

/// [`part`] for a draw that knows what it painted only once it has: `draw`
/// returns the bounds, under the same contract.
pub fn part_drawn(scene: &mut Scene, key: PartKey, draw: impl FnOnce(&mut Scene) -> Vec<Rect>) {
    boundary(scene, Boundary::Begin(key));
    let bounds = draw(scene);
    boundary(scene, Boundary::End(bounds));
}

/// Draw one part of a section with no bounds of its own: `draw` paints
/// content whose change since an earlier frame `hint` describes. A change
/// inside the part damages the hint's rects when the hint follows on from
/// the compared frame's (same source and placement, the next generation).
/// Otherwise, or with no hint, the change belongs to the enclosing part, and
/// outside every bounded part it is not proven.
pub fn hinted<R>(
    scene: &mut Scene,
    key: PartKey,
    hint: Option<DamageHint>,
    draw: impl FnOnce(&mut Scene) -> R,
) -> R {
    boundary(scene, Boundary::Begin(key));
    let result = draw(scene);
    boundary(scene, Boundary::EndHinted(hint));
    result
}

/// Close the part [`begin`] opened as a [`hinted`] part: for a draw that
/// knows what changed only once it has drawn (a widget notes its damage
/// while drawing).
pub fn end_hinted(scene: &mut Scene, hint: Option<DamageHint>) {
    boundary(scene, Boundary::EndHinted(hint));
}

/// Record the parts of a scene drawn on its own (a retained scene a widget
/// re-records only when it changes): the parts `draw` marks in `scene`, or
/// `None` when they are not well nested. Any recording around this one is
/// suspended while `draw` runs, so `draw` must paint only into `scene`.
pub fn record<R>(
    scene: &mut Scene,
    draw: impl FnOnce(&mut Scene) -> R,
) -> (R, Option<SectionParts>) {
    let recording = Recording::begin();
    track(scene);
    let result = draw(scene);
    let parts = recording.finish().of(scene);
    (result, parts)
}

/// Append `other`, whose parts were recorded as `parts` ([`record`]), so
/// that a tracked `scene` keeps them: each becomes a part of `scene` at the
/// same place, its bounds carried through `transform`. Without `parts`
/// everything `other` draws belongs to whatever part this append is in.
pub fn append(
    scene: &mut Scene,
    other: &Scene,
    parts: Option<&SectionParts>,
    transform: Option<vello::kurbo::Affine>,
) {
    append_with(scene, parts, transform, |scene| {
        scene.append(other, transform)
    });
}

/// [`append`] for a caller that appends the recorded scene itself (with
/// [`crate::backdrop::append_nested`], say): `append` must append exactly
/// that scene at `transform`.
pub fn append_with<R>(
    scene: &mut Scene,
    parts: Option<&SectionParts>,
    transform: Option<vello::kurbo::Affine>,
    append: impl FnOnce(&mut Scene) -> R,
) -> R {
    let base = Marks::of(scene.encoding());
    let result = append(scene);
    let tracked = RECORDING.with(|slot| {
        let mut slot = slot.borrow_mut();
        let Some(log) = slot
            .as_mut()
            .and_then(|logs| logs.iter_mut().find(|log| std::ptr::eq(log.scene, scene)))
        else {
            return false;
        };
        for event in parts.map_or(&[][..], |parts| &parts.events) {
            let boundary = match &event.boundary {
                Boundary::Begin(key) => Boundary::Begin(*key),
                Boundary::End(bounds) => Boundary::End(
                    bounds
                        .iter()
                        .map(|rect| transform.map_or(*rect, |t| t.transform_rect_bbox(*rect)))
                        .collect(),
                ),
                Boundary::EndHinted(hint) => {
                    Boundary::EndHinted(hint.as_ref().map(|hint| hint.transformed(transform)))
                }
            };
            log.events.push(Event {
                boundary,
                marks: base.offset(event.marks),
            });
        }
        true
    });
    if tracked {
        // What follows must not depend on the last transform or style
        // `other` encoded.
        scene.encoding_mut().force_next_transform_and_style();
    }
    result
}

/// Why two frames of a section cannot be compared piece by piece.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Unproven {
    /// The parts differ in number, kind or key.
    Structure,
    /// Commands outside every part differ.
    Section,
    /// A changed part has bounds that are missing, empty while it draws, or
    /// not finite.
    Unbounded,
    /// The recorded boundaries do not fit the encoding.
    Corrupt,
}

/// Damage, in section coordinates, between two frames of a section drawn
/// with parts: the old and new bounds of every part whose commands changed
/// (or what a [`hinted`] part's hint says changed). `Ok` with no rects means
/// the section is unchanged.
///
/// When the parts nested inside some part differ between the frames (a menu
/// that opened inside an overlay drew a part of its own), the parts down to
/// the deepest nesting both frames still have alike are compared instead,
/// each part at that depth as a whole: a window's menu damages its window,
/// a window a lane draws again damages that lane.
pub fn damage(
    previous: &Scene,
    previous_parts: &SectionParts,
    current: &Scene,
    current_parts: &SectionParts,
) -> Result<Vec<Rect>, Unproven> {
    let (old_events, new_events) = (&previous_parts.events, &current_parts.events);
    let mut proven = compare(previous, old_events, current, new_events, false);
    let mut depth = nesting(old_events).max(nesting(new_events));
    while matches!(proven, Err(Unproven::Structure)) && depth > 1 {
        depth = depth.saturating_sub(1);
        proven = compare(
            previous,
            &to_depth(old_events, depth)?,
            current,
            &to_depth(new_events, depth)?,
            false,
        );
    }
    proven.map(|(rects, _)| rects)
}

/// [`damage`] for a host that presents the section over the pixels of the
/// frame it compares with and can move them: a hinted part whose hint
/// follows on and carries copies ([`DamageHint::copies`]) damages its rects
/// only, and each copy comes with what the section draws over it and how
/// its content composites, which the host must check before making it
/// (and otherwise repaint where it lands, [`SectionDamage::without_copies`]).
/// `bounds` bounds the draws of the section outside every bounded part.
///
/// # Errors
/// As [`damage`].
pub fn damage_with_copies(
    previous: &Scene,
    previous_parts: &SectionParts,
    current: &Scene,
    current_parts: &SectionParts,
    bounds: &mut tilo_cpu_raster::DrawBounds,
) -> Result<SectionDamage, Unproven> {
    let (old_events, new_events) = (&previous_parts.events, &current_parts.events);
    let (rects, copied) = match compare(previous, old_events, current, new_events, true) {
        // Compared as wholes at a shallower depth, hints say nothing.
        Err(Unproven::Structure) => (
            damage(previous, previous_parts, current, current_parts)?,
            Vec::new(),
        ),
        proven => proven?,
    };
    let ends = part_ends(new_events)?;
    let mut copies = Vec::new();
    for begin in copied {
        let Some(PartEnd::Hint(Some(hint))) = ends.get(begin) else {
            continue;
        };
        let Some(end) = end_of(new_events, begin) else {
            continue;
        };
        let above = drawn_from(current, new_events, end.saturating_add(1), bounds);
        let plain_layers = new_events.get(begin).is_some_and(|event| {
            tilo_cpu_raster::plain_layers_open_at(current.encoding(), event.marks.draw_tags)
        });
        copies.extend(hint.copies.iter().map(|moved| SectionCopy {
            moved: *moved,
            above: above.clone(),
            plain_layers,
        }));
    }
    Ok(SectionDamage { rects, copies })
}

/// Where `scene`, drawn with `parts` (if it was), paints anything: inside a
/// bounded part by that part's bounds, anywhere else by its draw bounds.
/// `None` when some of it cannot be bounded.
pub fn drawn_bounds(
    scene: &Scene,
    parts: Option<&SectionParts>,
    bounds: &mut tilo_cpu_raster::DrawBounds,
) -> Option<Vec<Rect>> {
    if scene.encoding().draw_tags.is_empty() {
        return Some(Vec::new());
    }
    match parts {
        Some(parts) => drawn_from(scene, &parts.events, 0, bounds),
        None => bounds.of(scene.encoding()),
    }
}

/// One layer of a section ([`partition`]): a stretch of its draws that stands
/// on its own, in section (device) coordinates.
pub struct SectionLayer {
    /// What made this stretch a layer: the [`PartKey::value`] of a layer part,
    /// or a number of its own for the draws between them ([`Self::is_run`]).
    /// Layers are matched by key from frame to frame.
    pub key: u64,
    /// The layer's own commands, at the place the section draws them.
    pub scene: Scene,
    /// The damage parts inside it, rebased onto `scene`
    /// ([`Self::parts`]): what a host that keeps the layer's pixels compares
    /// frame to frame.
    pub parts: Option<SectionParts>,
    /// What of the section it paints, in section coordinates: inside the
    /// bounds of every part it is drawn in, and inside its own part's bounds.
    /// Empty when it paints nothing.
    pub bounds: Rect,
    /// This layer is the section's own draws between layer parts — the
    /// desktop ground and chrome, the overlays above the windows — rather
    /// than a layer part.
    pub run: bool,
    /// What the layer's drawing said of itself, when it said anything
    /// ([`DamageHint`]): a window that only moved offers the pixels it drew
    /// in the frame before ([`DamageHint::moved_by`]).
    pub hint: Option<DamageHint>,
    /// How far the layer's own coordinates are from where the screen shows
    /// it, in section coordinates: zero unless the section was drawn for a
    /// presenter that keeps its canvas ([`crate::retained_present::unbake_scroll`])
    /// and this layer sits in a strip drawn that way.
    pub offset: (f64, f64),
}

/// Keys of the runs [`partition`] makes start here, above any part's key.
const RUN_KEY: u64 = 1 << 63;

/// The key of the run drawn after the layer part keyed `after` (`0` for the
/// section's own draws before the first of them).
fn run_key(after: u64) -> u64 {
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    after.hash(&mut hasher);
    RUN_KEY | (hasher.finish() & (RUN_KEY - 1))
}

/// Split a section into the layers it is drawn as, in draw order: each layer
/// part ([`PartKey::layer`]) whose own content stands on its own, and the
/// stretches of the section's own draws between them. A host that keeps
/// each layer's pixels in a target of its own rasterizes a frame only where a
/// layer's content changed, and composites the others where they are: a
/// frame that only panned the strip draws no pixels at all.
///
/// `to_device` maps the section's draw coordinates onto the target (a page
/// drawing at a device pixel ratio), `bounds` is the scratch the draw bounds
/// need. `None` when the parts do not fit the encoding, or a layer's extent
/// cannot be bounded: the caller then keeps a single target.
pub fn partition(
    scene: &Scene,
    parts: &SectionParts,
    to_device: vello::kurbo::Affine,
    bounds: &mut tilo_cpu_raster::DrawBounds,
) -> Option<Vec<SectionLayer>> {
    let events = &parts.events;
    let ends = part_ends(events).ok()?;
    let encoding = scene.encoding();
    // A layer part is drawn into a target of its own only where nothing but a
    // plain clip is open around it: an alpha layer or another blend applies to
    // what it draws, which a composite of its own pixels would lose.
    let extractable: Vec<bool> = events
        .iter()
        .map(|event| match &event.boundary {
            Boundary::Begin(key) => {
                key.is_layer()
                    && tilo_cpu_raster::plain_layers_open_at(encoding, event.marks.draw_tags)
            }
            Boundary::End(_) | Boundary::EndHinted(_) => false,
        })
        .collect();
    // What each piece of the section belongs to: the outermost layer part open
    // across it, or the section's own draws.
    let pieces = events.len().saturating_add(1);
    let mut owners: Vec<Option<usize>> = Vec::with_capacity(pieces);
    let mut open: Vec<usize> = Vec::new();
    for piece in 0..pieces {
        owners.push(open.iter().copied().find(|index| extractable[*index]));
        match events.get(piece).map(|event| &event.boundary) {
            Some(Boundary::Begin(_)) => open.push(piece),
            Some(Boundary::End(_) | Boundary::EndHinted(_)) => {
                open.pop();
            }
            None => {}
        }
    }
    let mut layers = Vec::new();
    let mut first = 0;
    // The layer part the run being read follows: a run keeps its target
    // while the window before it comes and goes (`None` for the leading run).
    let mut after = 0_u64;
    while first < pieces {
        let owner = owners[first];
        let mut last = first;
        while last + 1 < pieces && owners[last + 1] == owner {
            last += 1;
        }
        let drawn = layer_of(
            scene,
            events,
            &ends,
            &Stretch {
                owner,
                first,
                last,
                after,
                enclosing: open_at(events, first),
            },
            to_device,
            bounds,
        );
        if let Some(index) = owner
            && let Some(Boundary::Begin(key)) = events.get(index).map(|event| &event.boundary)
        {
            after = key.value();
        }
        if let Some(layer) = drawn {
            layers.push(layer);
        }
        first = last + 1;
    }
    Some(layers)
}

/// The parts open across piece `piece` of a section.
fn open_at(events: &[Event], piece: usize) -> Vec<usize> {
    let mut open = Vec::new();
    for (index, event) in events.iter().enumerate().take(piece) {
        match event.boundary {
            Boundary::Begin(_) => open.push(index),
            Boundary::End(_) | Boundary::EndHinted(_) => {
                open.pop();
            }
        }
    }
    open
}

/// One stretch of a section's draws that a layer is made of.
struct Stretch {
    /// The layer part the stretch belongs to, or `None` for the section's
    /// own draws.
    owner: Option<usize>,
    /// The pieces it covers, and what the run before it follows.
    first: usize,
    last: usize,
    after: u64,
    /// The parts open across it.
    enclosing: Vec<usize>,
}

/// The layer a section's `stretch` is drawn as, or `None` when it paints
/// nothing.
fn layer_of(
    scene: &Scene,
    events: &[Event],
    ends: &[PartEnd<'_>],
    stretch: &Stretch,
    to_device: vello::kurbo::Affine,
    bounds: &mut tilo_cpu_raster::DrawBounds,
) -> Option<SectionLayer> {
    let Stretch {
        owner,
        first,
        last,
        after,
        enclosing,
    } = stretch;
    let (owner, first, last, after) = (*owner, *first, *last, *after);
    let encoding = scene.encoding();
    let (start, end) = (
        piece_range(encoding, events, first).0,
        piece_range(encoding, events, last).1,
    );
    let sliced = slice(encoding, (start, end))?;
    // The events strictly inside the stretch: the boundary that ends it is
    // the next stretch's, or the end of the part it is.
    let mut parts = None;
    if let Some(inside) = events.get(first..last) {
        let events: Vec<Event> = inside
            .iter()
            .map(|event| Event {
                boundary: event.boundary.clone(),
                marks: event.marks.rebased(start),
            })
            .collect();
        if !events.is_empty() {
            parts = Some(SectionParts { events });
        }
    }
    // A stretch that only opens or closes a layer paints nothing: a strip's
    // lane clip is one, and a target of its own for it would hold nothing.
    let paints = sliced.encoding().draw_tags.iter().any(|tag| {
        *tag != vello_encoding::DrawTag::BEGIN_CLIP && *tag != vello_encoding::DrawTag::END_CLIP
    });
    if !paints {
        return None;
    }
    let mut reach = match owner.and_then(|index| ends.get(index)) {
        Some(PartEnd::Bounds(rects)) => union(rects)?,
        // A hinted layer has no bounds of its own: what it draws is its own
        // extent, which is what a target of its own must hold.
        _ => union(&bounds.of(sliced.encoding())?)?,
    };
    // What a part it is drawn in bounds it by, and what clipping that part
    // would do to it: a stretch drawn inside a lane's clip paints nothing
    // outside it, and a target of its own must not either.
    for index in enclosing {
        if owner == Some(*index) {
            continue;
        }
        if let Some(PartEnd::Bounds(rects)) = ends.get(*index) {
            reach = reach.intersect(union(rects)?);
        }
    }
    let key = match owner {
        Some(index) => match events.get(index).map(|event| &event.boundary) {
            Some(Boundary::Begin(key)) => key.value(),
            _ => return None,
        },
        // The section's own draws between layer parts, keyed by the layer
        // part they follow: a run keeps its target while the windows around
        // it are drawn as they were.
        None => run_key(after),
    };
    // A drawing that handed over its content's change as a hint explains the
    // whole of it: whatever the hint says is what the layer did.
    let hint = parts
        .as_ref()
        .and_then(|parts| match parts.events.as_slice() {
            [
                Event {
                    boundary: Boundary::Begin(_),
                    ..
                },
                Event {
                    boundary: Boundary::EndHinted(hint),
                    ..
                },
            ] => hint.clone(),
            _ => None,
        });
    // Where the layer's coordinates sit on the screen: what the part it is
    // drawn in (or the part itself) noted while drawing.
    let offset = enclosing
        .iter()
        .rev()
        .find_map(
            |index| match events.get(*index).map(|event| &event.boundary) {
                Some(Boundary::Begin(key)) => layer_offset(*key),
                _ => None,
            },
        )
        .unwrap_or((0.0, 0.0));
    let from_origin = to_device * vello::kurbo::Point::ORIGIN;
    let to_offset = to_device * vello::kurbo::Point::new(offset.0, offset.1);
    let offset = (to_offset.x - from_origin.x, to_offset.y - from_origin.y);
    Some(SectionLayer {
        key,
        scene: sliced,
        parts,
        bounds: to_device.transform_rect_bbox(reach),
        run: owner.is_none(),
        hint,
        offset,
    })
}

/// The rectangle covering `rects`, or `None` when none of them does.
fn union(rects: &[Rect]) -> Option<Rect> {
    rects
        .iter()
        .filter(|rect| rect.width() > 0.0 && rect.height() > 0.0)
        .copied()
        .reduce(|all, rect| all.union(rect))
}

fn end_of(events: &[Event], begin: usize) -> Option<usize> {
    let mut depth = 0_usize;
    for (index, event) in events.iter().enumerate().skip(begin) {
        match event.boundary {
            Boundary::Begin(_) => depth = depth.saturating_add(1),
            Boundary::End(_) | Boundary::EndHinted(_) => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    return Some(index);
                }
            }
        }
    }
    None
}

/// Where `scene` paints in its pieces from `first` on: a piece inside a
/// bounded part by that part's bounds, any other by its draw bounds. The
/// parts still open at `first` (those around the part that ended there)
/// cover that part too, so their bounds would say everything after it is
/// over it: a piece only they bound counts by its draw bounds, and by
/// theirs only where those cannot bound it.
fn drawn_from(
    scene: &Scene,
    events: &[Event],
    first: usize,
    bounds: &mut tilo_cpu_raster::DrawBounds,
) -> Option<Vec<Rect>> {
    let ends = part_ends(events).ok()?;
    let bounds_of = |owner: &usize| match ends.get(*owner) {
        Some(PartEnd::Bounds(rects)) => Some(*rects),
        _ => None,
    };
    let mut open: Vec<usize> = Vec::new();
    let mut around: Option<Vec<usize>> = None;
    let mut drawn = Vec::new();
    for piece in 0..=events.len() {
        if piece >= first {
            let around = around.get_or_insert_with(|| open.clone());
            let range = piece_range(scene.encoding(), events, piece);
            let piece_scene = slice(scene.encoding(), range)?;
            // A stretch that only opens or closes a layer paints nothing.
            let paints = piece_scene.encoding().draw_tags.iter().any(|tag| {
                *tag != vello_encoding::DrawTag::BEGIN_CLIP
                    && *tag != vello_encoding::DrawTag::END_CLIP
            });
            if paints {
                let inside = open
                    .iter()
                    .rev()
                    .filter(|owner| !around.contains(owner))
                    .find_map(bounds_of);
                match inside {
                    // A part claiming to paint nothing must not have drawn.
                    Some([]) => return None,
                    Some(rects) => drawn.extend_from_slice(rects),
                    None => match bounds.of(piece_scene.encoding()) {
                        Some(rects) => drawn.extend(rects),
                        None => match open.iter().rev().find_map(bounds_of) {
                            Some([]) | None => return None,
                            Some(rects) => drawn.extend_from_slice(rects),
                        },
                    },
                }
            }
        }
        match events.get(piece).map(|event| &event.boundary) {
            Some(Boundary::Begin(_)) => open.push(piece),
            Some(Boundary::End(_) | Boundary::EndHinted(_)) => {
                open.pop();
            }
            None => {}
        }
    }
    drawn.iter().all(Rect::is_finite).then_some(drawn)
}

/// [`damage`] for one list of boundaries per frame, compared piece by piece.
/// With `copies`, a hinted part whose hint carries copies damages its rects
/// alone and is listed, by the index of its begin, beside the damage;
/// without, it damages where its copies land too.
fn compare(
    previous: &Scene,
    old_events: &[Event],
    current: &Scene,
    new_events: &[Event],
    copies: bool,
) -> Result<(Vec<Rect>, Vec<usize>), Unproven> {
    if old_events.len() != new_events.len()
        || old_events
            .iter()
            .zip(new_events)
            .any(|(old, new)| !same_boundary_kind(&old.boundary, &new.boundary))
    {
        return Err(Unproven::Structure);
    }
    // How each part ends, found at its end, by the index of its begin.
    let (old_ends, new_ends) = (part_ends(old_events)?, part_ends(new_events)?);
    let mut open: Vec<usize> = Vec::new();
    let mut damage = Vec::new();
    let mut copied = Vec::new();
    let mut changed = vec![false; old_events.len()];
    let pieces = old_events.len().saturating_add(1);
    for piece in 0..pieces {
        let old_range = piece_range(previous.encoding(), old_events, piece);
        let new_range = piece_range(current.encoding(), new_events, piece);
        let old = slice(previous.encoding(), old_range).ok_or(Unproven::Corrupt)?;
        let new = slice(current.encoding(), new_range).ok_or(Unproven::Corrupt)?;
        if !old.encoding_eq(&new) {
            let (owner, rects, copying) = changed_owner(&open, &old_ends, &new_ends, copies)?;
            // A part claiming to paint nothing must not have drawn anything.
            if let (Some(PartEnd::Bounds(old_part)), Some(PartEnd::Bounds(new_part))) =
                (old_ends.get(owner), new_ends.get(owner))
                && ((old_part.is_empty() && !old.encoding().draw_tags.is_empty())
                    || (new_part.is_empty() && !new.encoding().draw_tags.is_empty()))
            {
                return Err(Unproven::Unbounded);
            }
            let flag = changed.get_mut(owner).ok_or(Unproven::Corrupt)?;
            if !*flag {
                *flag = true;
                if copying {
                    copied.push(owner);
                }
                // A part that changed in place has the same bounds twice.
                for rect in rects {
                    if !damage.contains(&rect) {
                        damage.push(rect);
                    }
                }
            }
        }
        match new_events.get(piece).map(|event| &event.boundary) {
            Some(Boundary::Begin(_)) => open.push(piece),
            Some(Boundary::End(_) | Boundary::EndHinted(_)) => {
                open.pop();
            }
            None => {}
        }
    }
    if damage
        .iter()
        .any(|rect| !rect.is_finite() || rect.width() < 0.0 || rect.height() < 0.0)
    {
        return Err(Unproven::Unbounded);
    }
    Ok((damage, copied))
}

/// The part a change inside the `open` parts (innermost last) damages, and
/// what it damages: the innermost part with bounds, its old and new bounds,
/// unless a hinted part inside it has a hint that follows on, whose rects
/// it damages instead.
fn changed_owner(
    open: &[usize],
    old_ends: &[PartEnd<'_>],
    new_ends: &[PartEnd<'_>],
    copies: bool,
) -> Result<(usize, Vec<Rect>, bool), Unproven> {
    for &owner in open.iter().rev() {
        let old_end = old_ends.get(owner).ok_or(Unproven::Corrupt)?;
        let new_end = new_ends.get(owner).ok_or(Unproven::Corrupt)?;
        match (old_end, new_end) {
            (PartEnd::Bounds(old), PartEnd::Bounds(new)) => {
                let rects = old.iter().chain(new.iter()).copied().collect();
                return Ok((owner, rects, false));
            }
            (PartEnd::Hint(Some(old)), PartEnd::Hint(Some(new))) => {
                if new.follows(old) {
                    return Ok(if copies && !new.copies.is_empty() {
                        (owner, new.rects.clone(), true)
                    } else {
                        (owner, new.repainted(), false)
                    });
                }
                if let Some(rects) = new.reaches(old) {
                    return Ok((owner, rects.to_vec(), false));
                }
            }
            (PartEnd::Hint(_), PartEnd::Hint(_)) => {}
            _ => return Err(Unproven::Structure),
        }
    }
    Err(Unproven::Section)
}

fn same_boundary_kind(old: &Boundary, new: &Boundary) -> bool {
    match (old, new) {
        (Boundary::Begin(old), Boundary::Begin(new)) => old == new,
        (Boundary::End(_), Boundary::End(_)) | (Boundary::EndHinted(_), Boundary::EndHinted(_)) => {
            true
        }
        _ => false,
    }
}

/// How a part ends: with its bounds, or with a damage hint ([`hinted`]).
#[derive(Clone, Copy)]
enum PartEnd<'a> {
    Bounds(&'a [Rect]),
    Hint(Option<&'a DamageHint>),
}

/// How every part ends, indexed by the event that begins it.
fn part_ends(events: &[Event]) -> Result<Vec<PartEnd<'_>>, Unproven> {
    let mut ends = vec![PartEnd::Bounds(&[]); events.len()];
    let mut open = Vec::new();
    for (index, event) in events.iter().enumerate() {
        let end = match &event.boundary {
            Boundary::Begin(_) => {
                open.push(index);
                continue;
            }
            Boundary::End(rects) => PartEnd::Bounds(rects),
            Boundary::EndHinted(hint) => PartEnd::Hint(hint.as_ref()),
        };
        let begin = open.pop().ok_or(Unproven::Corrupt)?;
        *ends.get_mut(begin).ok_or(Unproven::Corrupt)? = end;
    }
    if open.is_empty() {
        Ok(ends)
    } else {
        Err(Unproven::Corrupt)
    }
}

/// How deep the parts of a section nest: 1 when none is inside another, 0
/// with no parts.
fn nesting(events: &[Event]) -> usize {
    let (mut depth, mut deepest) = (0_usize, 0_usize);
    for event in events {
        match event.boundary {
            Boundary::Begin(_) => {
                depth = depth.saturating_add(1);
                deepest = deepest.max(depth);
            }
            Boundary::End(_) | Boundary::EndHinted(_) => depth = depth.saturating_sub(1),
        }
    }
    deepest
}

/// The boundaries of the parts nested at most `max` deep (1: the outermost
/// parts only): each part at that depth compares as a whole, whatever parts
/// it draws inside. Below the outermost depth a kept hinted part keeps no
/// hint: a hint says what changed in content drawn alike, and parts inside
/// it may have come or gone, so a change in it belongs to the bounded part
/// around it. An outermost hinted part has no bounded part around it and
/// keeps its hint, as the outermost comparison always has.
fn to_depth(events: &[Event], max: usize) -> Result<Vec<Event>, Unproven> {
    let mut depth = 0_usize;
    let mut kept = Vec::new();
    for event in events {
        match event.boundary {
            Boundary::Begin(_) => {
                if depth < max {
                    kept.push(event.clone());
                }
                depth = depth.saturating_add(1);
            }
            Boundary::End(_) => {
                depth = depth.checked_sub(1).ok_or(Unproven::Corrupt)?;
                if depth < max {
                    kept.push(event.clone());
                }
            }
            Boundary::EndHinted(_) => {
                depth = depth.checked_sub(1).ok_or(Unproven::Corrupt)?;
                if depth < max {
                    kept.push(if depth == 0 {
                        event.clone()
                    } else {
                        Event {
                            boundary: Boundary::EndHinted(None),
                            marks: event.marks,
                        }
                    });
                }
            }
        }
    }
    if depth == 0 {
        Ok(kept)
    } else {
        Err(Unproven::Corrupt)
    }
}

/// The marks bounding piece `piece`: from the boundary before it (or the
/// start) to the boundary after it (or the end).
fn piece_range(encoding: &Encoding, events: &[Event], piece: usize) -> (Marks, Marks) {
    let start = piece
        .checked_sub(1)
        .and_then(|before| events.get(before))
        .map_or_else(Marks::default, |event| event.marks);
    let end = events
        .get(piece)
        .map_or_else(|| Marks::of(encoding), |event| event.marks);
    (start, end)
}

/// One piece of an encoding as an encoding of its own, every offset into the
/// parent rebased onto the piece, so pieces at different positions compare.
fn slice(encoding: &Encoding, (start, end): (Marks, Marks)) -> Option<Scene> {
    fn range<T: Clone>(items: &[T], start: usize, end: usize) -> Option<Vec<T>> {
        items.get(start..end).map(<[T]>::to_vec)
    }
    let resources = &encoding.resources;
    let mut piece = Encoding::new();
    piece.path_tags = range(&encoding.path_tags, start.path_tags, end.path_tags)?;
    piece.path_data = range(&encoding.path_data, start.path_data, end.path_data)?;
    piece.draw_tags = range(&encoding.draw_tags, start.draw_tags, end.draw_tags)?;
    piece.draw_data = range(&encoding.draw_data, start.draw_data, end.draw_data)?;
    piece.transforms = range(&encoding.transforms, start.transforms, end.transforms)?;
    piece.styles = range(&encoding.styles, start.styles, end.styles)?;
    piece.resources.color_stops =
        range(&resources.color_stops, start.color_stops, end.color_stops)?;
    piece.resources.glyphs = range(&resources.glyphs, start.glyphs, end.glyphs)?;
    piece.resources.normalized_coords = range(
        &resources.normalized_coords,
        start.normalized_coords,
        end.normalized_coords,
    )?;
    piece.resources.patches = resources
        .patches
        .get(start.patches..end.patches)?
        .iter()
        .map(|patch| rebase_patch(patch, &start))
        .collect::<Option<_>>()?;
    piece.resources.glyph_runs = resources
        .glyph_runs
        .get(start.glyph_runs..end.glyph_runs)?
        .iter()
        .map(|run| {
            let mut run = run.clone();
            run.glyphs = run.glyphs.start.checked_sub(start.glyphs)?
                ..run.glyphs.end.checked_sub(start.glyphs)?;
            run.normalized_coords = run
                .normalized_coords
                .start
                .checked_sub(start.normalized_coords)?
                ..run
                    .normalized_coords
                    .end
                    .checked_sub(start.normalized_coords)?;
            let offsets = &mut run.stream_offsets;
            offsets.path_tags = offsets.path_tags.checked_sub(start.path_tags)?;
            offsets.path_data = offsets.path_data.checked_sub(start.path_data)?;
            offsets.draw_tags = offsets.draw_tags.checked_sub(start.draw_tags)?;
            offsets.draw_data = offsets.draw_data.checked_sub(start.draw_data)?;
            offsets.transforms = offsets.transforms.checked_sub(start.transforms)?;
            offsets.styles = offsets.styles.checked_sub(start.styles)?;
            Some(run)
        })
        .collect::<Option<_>>()?;
    Some(Scene::from(piece))
}

fn rebase_patch(patch: &Patch, start: &Marks) -> Option<Patch> {
    Some(match patch {
        Patch::Ramp {
            draw_data_offset,
            stops,
            extend,
        } => Patch::Ramp {
            draw_data_offset: draw_data_offset.checked_sub(start.draw_data)?,
            stops: stops.start.checked_sub(start.color_stops)?
                ..stops.end.checked_sub(start.color_stops)?,
            extend: *extend,
        },
        Patch::GlyphRun { index } => Patch::GlyphRun {
            index: index.checked_sub(start.glyph_runs)?,
        },
        Patch::Image {
            draw_data_offset,
            image,
        } => Patch::Image {
            draw_data_offset: draw_data_offset.checked_sub(start.draw_data)?,
            image: image.clone(),
        },
    })
}

#[cfg(test)]
#[path = "scene_parts_tests.rs"]
mod tests;
