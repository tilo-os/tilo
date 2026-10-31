/// Window frames, their tabs and their stacks (shell.md §4).
///
/// A frame is the shell's view of a place: a row of tabs over the resource's content. Every
/// part of that row says what the resource and its type say, never what an app says: the
/// title is the resource's title field, the glyph its type's icon, the faces on a tab who else
/// is at that resource now, and the frame's commands the resource's own actions.
///
/// A frame holds one place or several. Several places in one frame are its tabs: the strip
/// squeezes them to fit however narrow the frame is, and a tab dragged out of the strip hands
/// its resource to the frame under the pointer (or a new one) at the gesture's end.
///
/// The arrangement a shell of frames keeps is its own record: places, and the frames they are
/// in. The views here read it; its actions change it.
/// A place: one resource shown in a frame, through a view of its type.
///
/// A place shows one resource and nothing else. Its view is the type's, unless the person
/// chose another; what the place remembers of how it shows the resource (its sort, its
/// filter, an outline opened) is its window state, personal as the arrangement is.
type Place {
    /// What the place shows: a note, a conversation, a task, of any app (shell.md §3: a
    /// reference is a resource, and which app's it is does not matter here).
    shows: host.Resource
    /// The view it shows the resource through, by its id; empty for the type's own.
    shows_as: Text
    /// What the frame keeps of how this place shows its resource, as that view names it.
    window_state: Text
}

/// A frame: the places it holds, in tab order, and which of them is in front.
///
/// One place is a frame with no strip; several make a tab strip. A frame a person closed
/// leaves its column; undo brings it back as it was.
type Frame {
    places: Collection<Place>
    /// The place whose content shows: its tab in front. Unset for the first tab.
    front: Option<Place> = .none
    /// Whether it is the frame the person works in: its edge and its tab say so.
    focused: Bool
    /// What the frame is called when the person names it; empty for its front place's title.
    named: Text
    /// The frame's width in its strip, in permille of what the strip offers: a narrow reading,
    /// a normal working width, a wide one. The arrangement's, never an app's.
    width: Int = 500
    /// Whether the frame is maximized over its strip.
    maximized: Bool = false
}

/// A stack: frames standing in one column, their headers visible and one expanded.
///
/// A stack is how this arrangement shows several frames in one column: each frame keeps its
/// tab strip as its header, so its places stay reachable, and the expanded frame shows its
/// content under it. Activating a frame's header makes it the expanded one.
type Stack {
    frames: Collection<Frame>
    /// The frame whose content shows. Unset for the first.
    expanded: Option<Frame> = .none
    /// The stack's name, when the person names it.
    named: Text
}

/// A strip: the columns in front of one workspace.
type Strip {
    stacks: Collection<Stack>
    /// The column the person works in. Unset for the first.
    front: Option<Stack> = .none
    /// The strip's name, as the top bar shows it.
    named: Text
}

/// The person's arrangement: the strips they had open, and the strip in front.
///
/// The arrangement is the person's own record, synced across their devices that use this
/// shell, with history and undo (workspace-model.md §9). Nothing in it is shared with the other
/// members of the workspace.
type Arrangement {
    strips: Collection<Strip>
    /// The strip the person is in. Unset for the first.
    front: Option<Strip> = .none
    /// The documents this package's scenarios place in frames (a real desktop's places hold
    /// the apps' own resources; the frame draws either the same way).
    documents: Collection<Document>
}

root Arrangement

default view desk for Arrangement
