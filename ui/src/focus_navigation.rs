//! Spatial focus policy shared by native and portable hosts.
use taffy::NodeId;
use vello::kurbo::Rect;
use winit::keyboard::NamedKey;

#[derive(Debug, Clone, Copy)]
pub(crate) enum Direction {
    Up,
    Down,
    Left,
    Right,
}
impl Direction {
    pub(crate) fn from_key(key: NamedKey) -> Option<Self> {
        match key {
            NamedKey::ArrowUp => Some(Self::Up),
            NamedKey::ArrowDown => Some(Self::Down),
            NamedKey::ArrowLeft => Some(Self::Left),
            NamedKey::ArrowRight => Some(Self::Right),
            _ => None,
        }
    }
}
/// Keep tree order for ties, as the native host always has.
pub(crate) fn next(
    current: (NodeId, Rect),
    candidates: impl IntoIterator<Item = (NodeId, Rect)>,
    direction: Direction,
) -> Option<NodeId> {
    let center = current.1.center();
    let mut best = None;
    let mut best_score = f64::MAX;
    for (id, rect) in candidates {
        if id == current.0 {
            continue;
        }
        let delta = rect.center() - center;
        let (primary, cross, eligible) = match direction {
            Direction::Up => (-delta.y, delta.x.abs(), delta.y < -1.0),
            Direction::Down => (delta.y, delta.x.abs(), delta.y > 1.0),
            Direction::Left => (-delta.x, delta.y.abs(), delta.x < -1.0),
            Direction::Right => (delta.x, delta.y.abs(), delta.x > 1.0),
        };
        let score = primary + cross * 2.0;
        if eligible && score < best_score {
            best_score = score;
            best = Some(id);
        }
    }
    best
}
