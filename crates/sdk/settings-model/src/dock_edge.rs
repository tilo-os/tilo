//! The edge a panel kept at the screen's edges docks to, as the person's settings name it.

use serde::{Deserialize, Serialize};

/// Where something kept at the screen's edges waits (the pinned people
/// overlay). Corners and the side and bottom edges; never the top edge.
#[cfg_attr(feature = "model", derive(tilo_resource::Model))]
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DockEdge {
    Left,
    Right,
    Bottom,
    BottomLeft,
    BottomRight,
}

impl DockEdge {
    pub const ALL: [Self; 5] = [
        Self::Left,
        Self::Right,
        Self::Bottom,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Left => "Left edge",
            Self::Right => "Right edge",
            Self::Bottom => "Bottom edge",
            Self::BottomLeft => "Bottom-left corner",
            Self::BottomRight => "Bottom-right corner",
        }
    }
}
