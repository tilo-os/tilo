//! Two angles of one moment in one frame: the bottom-most visible video track
//! takes the left half, the next one the right, so nothing has to be named to
//! pair them. The placement is an ordinary [`Transform`].

use serde::{Deserialize, Serialize};

use crate::model::{ClipId, Project, Track, TrackKind, Transform};

/// Which half of the frame a clip of a cut takes. Back to front is
/// [`Side::Left`], [`Side::Right`], in track order.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum Side {
    /// The clip keeps the picture it had.
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

    /// Half the frame wide and full height, the anchor moved to that edge so
    /// the picture fills it. `None` for [`Side::None`].
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

/// Where a clip sits in a cut by [`Project::place_angles`], and how to put its
/// picture back.
#[derive(Clone, Debug, PartialEq)]
pub struct SidePlacement {
    pub clip: ClipId,
    pub side: Side,
    /// The transform the clip had before it was placed.
    pub before: Transform,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CutSides {
    pub left: Option<ClipId>,
    pub right: Option<ClipId>,
}

impl CutSides {
    pub fn clip(self, side: Side) -> Option<ClipId> {
        match side {
            Side::Left => self.left,
            Side::Right => self.right,
            Side::None => None,
        }
    }
}

impl Project {
    /// Place the two clips on screen at `at` on the sides of the frame. Only
    /// what is visible is placed, and a clip already on its side is left alone,
    /// so pressing the action again changes nothing.
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

    /// Give each clip back the transform it had, unless it has been placed
    /// elsewhere since.
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

    /// `None` when two pictures do not meet at `at`.
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

    /// Whether [`Project::place_angles`] would change anything at `at`. A
    /// control that could only fail is left out rather than drawn disabled.
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
