//! Where a text widget drew a peer in its own text this frame, and who that
//! peer is: the caret it paints in their name, or the band across their
//! selection. What a reader is told of it is published from here, so a screen
//! reader — and anything reading a frame's semantics — finds the peer writing
//! where the eye finds their caret.

use crate::permissions::UserId;
use vello::kurbo::Rect;

/// One peer's caret or selection a text widget drew, in scene coordinates:
/// the mark itself, whose it is, and the words drawn with it.
#[derive(Clone, Debug, PartialEq)]
pub struct PresenceMark {
    /// Whose caret or selection this is.
    pub user: UserId,
    /// The caret's bar, or one band of a selection.
    pub rect: Rect,
    /// Whether it spans a selection rather than resting as a caret.
    pub selection: bool,
    /// What the words drawn beside it say the peer is doing there, when they
    /// say it — an action's own label, "Claude Code: Appending paragraph". A
    /// reader is then told them rather than that the peer is writing.
    pub saying: Option<String>,
}
