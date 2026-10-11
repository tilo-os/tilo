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

/// Identifies one part among the parts of a section. Two frames' parts are
/// matched by key and position, so a key must name the same thing in both.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct PartKey(u64);

impl PartKey {
    /// A key for `kind` (a fixed name for the draw site) and `id`, which tells
    /// apart several parts of the same kind (a window's id, a peer's index).
    pub fn new(kind: &'static str, id: impl Hash) -> Self {
        let mut hasher = std::collections::hash_map::DefaultHasher::new();
        kind.hash(&mut hasher);
        id.hash(&mut hasher);
        Self(hasher.finish())
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
    /// The generation the damage is measured from.
    pub from: u64,
    /// The generation painted.
    pub to: u64,
    /// Where the content is placed: content placed elsewhere has moved.
    pub placement: vello::kurbo::Affine,
    /// Every pixel that differs between generations `from` and `to`, in the
    /// section's coordinates, antialiasing, shadows and overhang included.
    pub rects: Vec<Rect>,
}

impl DamageHint {
    /// `self`'s damage when it describes the change from `previous`'s
    /// content: the same source placed in the same place, painting the
    /// generation that follows the one `previous` painted.
    fn follows(&self, previous: &Self) -> Option<&[Rect]> {
        // The generation this one was measured from is the one painted then.
        let next_generation = previous.to == self.from;
        (next_generation
            && self.source == previous.source
            && self.placement == previous.placement
            && !self.rects.is_empty())
        .then_some(self.rects.as_slice())
    }

    fn transformed(&self, transform: Option<vello::kurbo::Affine>) -> Self {
        let Some(transform) = transform else {
            return self.clone();
        };
        Self {
            placement: transform * self.placement,
            rects: self
                .rects
                .iter()
                .map(|rect| transform.transform_rect_bbox(*rect))
                .collect(),
            ..self.clone()
        }
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
/// that opened inside an overlay drew a part of its own), the outermost
/// parts are compared instead, each as a whole.
pub fn damage(
    previous: &Scene,
    previous_parts: &SectionParts,
    current: &Scene,
    current_parts: &SectionParts,
) -> Result<Vec<Rect>, Unproven> {
    let (old_events, new_events) = (&previous_parts.events, &current_parts.events);
    match compare(previous, old_events, current, new_events) {
        Err(Unproven::Structure) => compare(
            previous,
            &outermost(old_events)?,
            current,
            &outermost(new_events)?,
        ),
        proven => proven,
    }
}

/// [`damage`] for one list of boundaries per frame, compared piece by piece.
fn compare(
    previous: &Scene,
    old_events: &[Event],
    current: &Scene,
    new_events: &[Event],
) -> Result<Vec<Rect>, Unproven> {
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
    let mut changed = vec![false; old_events.len()];
    let pieces = old_events.len().saturating_add(1);
    for piece in 0..pieces {
        let old_range = piece_range(previous.encoding(), old_events, piece);
        let new_range = piece_range(current.encoding(), new_events, piece);
        let old = slice(previous.encoding(), old_range).ok_or(Unproven::Corrupt)?;
        let new = slice(current.encoding(), new_range).ok_or(Unproven::Corrupt)?;
        if !old.encoding_eq(&new) {
            let (owner, rects) = changed_owner(&open, &old_ends, &new_ends)?;
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
                // A part that changed in place has the same bounds twice.
                for rect in rects {
                    if !damage.contains(&rect) {
                        damage.push(rect);
                    }
                }
            }
        }
        // Advance past the boundary that ends this piece.
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
    Ok(damage)
}

/// The part a change inside the `open` parts (innermost last) damages, and
/// what it damages: the innermost part with bounds, its old and new bounds,
/// unless a hinted part inside it has a hint that follows on, whose rects
/// it damages instead.
fn changed_owner(
    open: &[usize],
    old_ends: &[PartEnd<'_>],
    new_ends: &[PartEnd<'_>],
) -> Result<(usize, Vec<Rect>), Unproven> {
    for &owner in open.iter().rev() {
        let old_end = old_ends.get(owner).ok_or(Unproven::Corrupt)?;
        let new_end = new_ends.get(owner).ok_or(Unproven::Corrupt)?;
        match (old_end, new_end) {
            (PartEnd::Bounds(old), PartEnd::Bounds(new)) => {
                return Ok((owner, old.iter().chain(new.iter()).copied().collect()));
            }
            (PartEnd::Hint(Some(old)), PartEnd::Hint(Some(new))) => {
                if let Some(rects) = new.follows(old) {
                    return Ok((owner, rects.to_vec()));
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

/// The boundaries of the outermost parts only: each compares as a whole,
/// whatever parts it draws inside.
fn outermost(events: &[Event]) -> Result<Vec<Event>, Unproven> {
    let mut depth = 0_usize;
    let mut outer = Vec::new();
    for event in events {
        match event.boundary {
            Boundary::Begin(_) => {
                if depth == 0 {
                    outer.push(event.clone());
                }
                depth = depth.saturating_add(1);
            }
            Boundary::End(_) | Boundary::EndHinted(_) => {
                depth = depth.checked_sub(1).ok_or(Unproven::Corrupt)?;
                if depth == 0 {
                    outer.push(event.clone());
                }
            }
        }
    }
    if depth == 0 {
        Ok(outer)
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
