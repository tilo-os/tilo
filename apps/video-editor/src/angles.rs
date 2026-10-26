//! Two angles of one moment in one frame.
//!
//! A cut of the same instant filmed twice — two cameras on one desk, two
//! machines side by side — reaches the frame through the track order: the
//! bottom-most visible video track takes the left half, the next one the
//! right, and nothing has to be named or tagged to make it work. The
//! placement is an ordinary [`Transform`], so a picture placed this way
//! moves and scales like any other afterwards.

use serde::{Deserialize, Serialize};

use crate::model::{ClipId, Project, Track, TrackKind, Transform};

/// How much of the frame each picture of a cut takes: the way two angles
/// filmed at the same time — two cameras on one desk, two machines —
/// reach the frame together instead of one covering the other.
///
/// The pictures share the frame evenly and keep their own aspect, so two
/// 16:9 sources on a 16:9 frame meet in the middle with the black bars a
/// full-frame cut would have shown. Back to front is [`Side::Left`],
/// [`Side::Right`] — the arrangement reads in the order the tracks were
/// put together.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// Nothing is placed: the clip keeps the picture it had.
    #[default]
    #[serde(rename = "none")]
    None,
    #[serde(rename = "left")]
    Left,
    #[serde(rename = "right")]
    Right,
}

impl Side {
    pub fn as_str(self) -> &'static str {
        match self {
            Side::None => "none",
            Side::Left => "left",
            Side::Right => "right",
        }
    }

    pub fn from_name(s: &str) -> Option<Self> {
        Some(match s {
            "none" => Side::None,
            "left" => Side::Left,
            "right" => Side::Right,
            _ => return None,
        })
    }

    /// The placement this side gives a clip: half the frame wide, the same
    /// height it was, with the anchor moved to that edge so the picture
    /// fills it. `None` for [`Side::None`] — a side places a picture, and
    /// that one says the clip is not part of the cut.
    pub fn placement(self) -> Option<Transform> {
        let (pos_x, anchor_x) = match self {
            Side::None => return None,
            Side::Left => (-0.25, 0.0),
            Side::Right => (0.25, 1.0),
        };
        Some(Transform {
            scale_x: 0.5,
            scale_y: 1.0,
            pos_x,
            pos_y: 0.0,
            anchor_x,
            anchor_y: 0.5,
            ..Transform::default()
        })
    }
}

/// Where a clip sits in a cut made by [`Project::place_angles`], and
/// how to put its picture back. What the editor's undo returns to.
#[derive(Clone, Debug, PartialEq)]
pub struct SidePlacement {
    pub clip: ClipId,
    pub side: Side,
    /// The transform the clip had before it was placed.
    pub before: Transform,
}

/// The clip of a cut that a given frame shows `side`.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutSides {
    pub left: Option<ClipId>,
    pub right: Option<ClipId>,
}

impl CutSides {
    /// The clip this cut places on `side`, if any.
    pub fn clip(self, side: Side) -> Option<ClipId> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
            Side::None => None,
        }
    }
}

impl Project {
    /// Place every clip covering `at` on the two sides of the frame: the
    /// bottom-most visible video track on the left, the next one on the
    /// right. That is what turns two angles of the same moment into one
    /// picture, and it needs whatever footage the person has rather than a
    /// named or tagged media: the track order is the arrangement.
    ///
    /// Only what is actually on screen at `at` is placed — a clip the
    /// timeline has not reached, a clip on a muted track or a clip left out
    /// by a solo keeps its picture — and a clip already on the side it
    /// belongs to keeps the other half of its placement, so pressing the
    /// action again neither drifts the picture nor shows the same take in
    /// the same place twice.
    ///
    /// Returns what it placed, with the transform each clip had, for
    /// [`Project::restore_angles`] and for undo.
    pub fn place_angles(&mut self, at: f64) -> Vec<SidePlacement> {
        let Some(sides) = self.cut_sides(at) else {
            return Vec::new();
        };
        let mut placed = Vec::new();
        for side in [Side::Left, Side::Right] {
            let Some(clip) = sides.clip(side) else {
                continue;
            };
            let (Some(placement), Some(c)) = (side.placement(), self.clip_mut(clip)) else {
                continue;
            };
            let before = c.transform;
            if before == placement {
                continue;
            }
            c.transform = placement;
            placed.push(SidePlacement { clip, side, before });
        }
        placed
    }

    /// Undo [`Project::place_angles`]: give each clip back the
    /// transform it had, where it still sits on that side and the person
    /// has not placed it somewhere else since.
    pub fn restore_angles(&mut self, placements: &[SidePlacement]) {
        for p in placements {
            let Some(c) = self.clip_mut(p.clip) else {
                continue;
            };
            if p.side.placement() == Some(c.transform) {
                c.transform = p.before;
            }
        }
    }

    /// The clips a side-by-side cut shows at `at`: the bottom-most visible
    /// video track's clip on the left, the next on the right. `None` when
    /// two pictures do not meet at that instant, which is also what a cut
    /// of one angle cannot be.
    pub fn cut_sides(&self, at: f64) -> Option<CutSides> {
        let soloing = self
            .tracks
            .iter()
            .any(|t| t.kind == TrackKind::Video && t.solo);
        let mut video: Vec<&Track> = self
            .tracks
            .iter()
            .filter(|t| t.kind == TrackKind::Video && !t.muted && (!soloing || t.solo))
            .collect();
        video.sort_by_key(|t| t.index);
        let mut covering = video
            .into_iter()
            .filter_map(|t| self.clips_on(t.id).into_iter().find(|c| c.covers(at)))
            .map(|c| c.id);
        let left = covering.next()?;
        let right = covering.next()?;
        Some(CutSides {
            left: Some(left),
            right: Some(right),
        })
    }

    /// Whether [`Project::place_angles`] would change anything at `at`: two
    /// pictures meet there, on tracks that may be edited, and at least one
    /// of them is not already in its half. A control that could only fail
    /// or do nothing is left out rather than drawn disabled.
    pub fn side_by_side_available(&self, at: f64) -> bool {
        let Some(sides) = self.cut_sides(at) else {
            return false;
        };
        let mut moved = false;
        for side in [Side::Left, Side::Right] {
            let Some(clip) = sides.clip(side) else {
                return false;
            };
            let (Some(c), Some(placement)) = (self.clip(clip), side.placement()) else {
                return false;
            };
            if !self.clip_editable(clip) {
                return false;
            }
            moved |= c.transform != placement;
        }
        moved
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::{MediaItem, MediaKind};
    use uuid::Uuid;

    /// Three video tracks, one 20 s source each, all starting at t=0: the
    /// same ten seconds of two angles, stacked bottom to top.
    fn two_angles() -> (Project, [ClipId; 3]) {
        let mut p = Project::new("two angles");
        let mid = Uuid::new_v4();
        p.media.push(MediaItem {
            id: mid,
            name: "angle.mp4".into(),
            kind: MediaKind::Video,
            duration: 20.0,
            width: 1920,
            height: 1080,
            fps: 30.0,
            ..Default::default()
        });
        let mut clips = Vec::new();
        for index in 1..=3 {
            let tid = Uuid::new_v4();
            p.tracks.push(Track {
                id: tid,
                name: format!("V{index}"),
                index,
                ..Default::default()
            });
            clips.push(p.add_clip(tid, mid, 0.0).expect("media exists"));
        }
        let [a, b, c] = <[ClipId; 3]>::try_from(clips).expect("three tracks");
        (p, [a, b, c])
    }

    #[test]
    #[expect(
        clippy::float_cmp,
        reason = "the placement is a stored literal, compared as the value it was set to"
    )]
    fn a_side_puts_a_picture_in_its_half_of_the_frame() {
        assert!(Side::None.placement().is_none(), "a picture untransformed");
        let left = Side::Left.placement().expect("a left half");
        assert_eq!(left.scale_x, 0.5, "half as wide");
        assert_eq!(left.scale_y, 1.0, "full height, so nothing is squashed");
        assert_eq!(left.pos_x, -0.25, "its own centre, half a frame left");
        assert_eq!(left.anchor_x, 0.0, "hung from its frame's left edge");
        let right = Side::Right.placement().expect("a right half");
        assert_eq!(right.pos_x, 0.25, "the other half of the frame");
        assert_eq!(right.anchor_x, 1.0, "hung from its frame's right edge");
        assert_eq!(left.scale_y, right.scale_y, "both the same height");
        assert_eq!(Side::from_name(Side::Left.as_str()), Some(Side::Left));
        assert_eq!(Side::from_name(Side::Right.as_str()), Some(Side::Right));
        assert_eq!(Side::from_name("none"), Some(Side::None));
        assert_eq!(Side::from_name("elsewhere"), None, "no side is named that");
    }

    #[test]
    fn a_cut_takes_the_two_bottom_visible_tracks() {
        let (mut p, [bottom, middle, top]) = two_angles();
        let sides = p.cut_sides(1.0).expect("two pictures meet");
        assert_eq!(sides.clip(Side::Left), Some(bottom), "the lowest track");
        assert_eq!(sides.clip(Side::Right), Some(middle), "the one above it");
        assert_ne!(
            sides.clip(Side::Right),
            Some(top),
            "a third angle is not in a two-way cut"
        );

        // A muted track shows nothing, so the next one up takes its side.
        let track = p.clip(middle).expect("placed").track;
        p.tracks.iter_mut().find(|t| t.id == track).unwrap().muted = true;
        let sides = p.cut_sides(1.0).expect("muted tracks are not pictures");
        assert_eq!(sides.clip(Side::Left), Some(bottom));
        assert_eq!(sides.clip(Side::Right), Some(top));

        // A solo leaves only its own tracks in the cut: the bottom one and
        // the one above it are then the only two pictures, in that order.
        p.tracks.iter_mut().find(|t| t.id == track).unwrap().muted = false;
        for t in &mut p.tracks {
            t.solo = t.index <= 2;
        }
        let sides = p.cut_sides(1.0).expect("both soloed tracks cover it");
        assert_eq!(
            sides.clip(Side::Left),
            Some(bottom),
            "the lowest soloed one"
        );
        assert_eq!(sides.clip(Side::Right), Some(middle), "and the next");

        // One picture alone is not a cut.
        for t in &mut p.tracks {
            t.solo = t.index == 1;
        }
        assert_eq!(p.cut_sides(1.0), None, "one picture is not a cut");
    }

    #[test]
    fn a_cut_needs_two_pictures_meeting_at_one_instant() {
        let (mut p, [bottom, middle, _]) = two_angles();
        // The lower angle now covers 10 s to 14 s, so for the first ten
        // seconds two of the three pictures meet and the lowest of those two
        // is the one on V2.
        if let Some(c) = p.clip_mut(bottom) {
            c.start = 10.0;
            c.in_point = 4.0;
            c.out_point = 8.0;
        }
        let early = p.cut_sides(1.0).expect("the two upper pictures cover t=1");
        assert_eq!(
            early.clip(Side::Left),
            Some(middle),
            "the lowest picture left"
        );
        assert_ne!(
            early.clip(Side::Right),
            Some(bottom),
            "the lower angle is not there yet"
        );
        let sides = p.cut_sides(11.0).expect("both angles cover t=11");
        assert_eq!(sides.clip(Side::Left), Some(bottom), "the lower angle");
        assert_eq!(sides.clip(Side::Right), Some(middle), "and the upper");
        assert_eq!(p.cut_sides(30.0), None, "past the end of both");

        let none = Project::new("empty");
        assert_eq!(none.cut_sides(0.0), None, "no tracks at all");
    }

    #[test]
    fn placing_angles_scales_both_to_their_halves_and_undoes_cleanly() {
        let (mut p, [bottom, middle, _]) = two_angles();
        let before = p.clone();
        let placed = p.place_angles(1.0);
        assert_eq!(placed.len(), 2, "both pictures were placed");
        assert_eq!(
            p.clip(bottom).unwrap().transform,
            Side::Left.placement().unwrap(),
            "the lower angle on the left"
        );
        assert_eq!(
            p.clip(middle).unwrap().transform,
            Side::Right.placement().unwrap(),
            "the upper one on the right"
        );

        // Pressing it again moves nothing: each picture is already home.
        assert!(p.place_angles(1.0).is_empty(), "already beside each other");

        p.restore_angles(&placed);
        assert_eq!(p.clip(bottom).unwrap().transform, Transform::default());
        assert_eq!(p.clip(middle).unwrap().transform, Transform::default());
        assert_eq!(p.clip(middle).unwrap(), before.clip(middle).unwrap());

        // A picture the person has since moved elsewhere is left where
        // they put it.
        let placed = p.place_angles(1.0);
        if let Some(c) = p.clip_mut(middle) {
            c.transform = Transform::pip(0.3, (1.0, -1.0), 0.04);
        }
        p.restore_angles(&placed);
        assert_eq!(
            p.clip(bottom).unwrap().transform,
            Transform::default(),
            "the untouched side comes back"
        );
        assert_eq!(
            p.clip(middle).unwrap().transform,
            Transform::pip(0.3, (1.0, -1.0), 0.04),
            "the one placed by hand stays"
        );
    }
}
