//! A caret or selection in a page's editor, as the live plane's selection
//! anchors: how a presence frame carries a range the receiving editor can
//! place on its own keys.

use tilo_relay_protocol::live::{
    AnchorScope, LiveAnchor, read_selection_anchors, selection_anchors,
};
use ui::presence::PresenceKey;

/// A caret (`start == end`) or selection in the editor `key` as the live
/// plane's selection anchors. A key too long for the anchor (a framework
/// resource key naming a long item id) travels as its scope digest, which the
/// receiving editor resolves against its own keys
/// ([`PresenceKey::digest_form`]); `None` when even that does not fit.
pub fn presence_anchors(
    key: &PresenceKey,
    start: usize,
    end: usize,
) -> Option<(LiveAnchor, LiveAnchor)> {
    selection_anchors(
        &postcard::to_stdvec(key).ok()?,
        || key.scope_digest(),
        u64::try_from(start).ok()?,
        u64::try_from(end).ok()?,
    )
}

/// The editor key and range of selection anchors made by
/// [`presence_anchors`].
pub fn read_presence_anchors(
    anchor: &LiveAnchor,
    head: &LiveAnchor,
) -> Option<(PresenceKey, usize, usize)> {
    let (scope, start, end) = read_selection_anchors(anchor, head)?;
    let key = match scope {
        AnchorScope::Key(key) => postcard::from_bytes(key).ok()?,
        AnchorScope::Digest(digest) => PresenceKey::from_scope_digest(digest),
    };
    Some((
        key,
        usize::try_from(start).ok()?,
        usize::try_from(end).ok()?,
    ))
}
