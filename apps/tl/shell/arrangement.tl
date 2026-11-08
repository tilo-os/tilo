/// The person's arrangement: what is open where (docs/designs/app-language/shell.md §2,
/// workspace-model.md §9). It is the desk itself — a row across a workspace, a strip of places in
/// a row, a place showing a resource through a view — which the session keeps with the desk the
/// workspace shares, with history and undo.
///
/// The session reads it off the live desk tree and publishes its reading as
/// `session.arrangement`. The shell's views read it (§9) and change it only through the arranging
/// actions on `shell:arrangement` (`so.tilo.session`'s), so a person, an agent and a script
/// arrange the same way, attributed and undoable.
///
/// Every entry names the shell that made it (`shell`): a shell that does not know an entry
/// another shell made keeps it and never changes or removes it (§2).
/// The person's desk as the session reads it off the desk: the workspaces it holds, its
/// rows, its strips, and every place. Which preset it shows and which strip stands in front are
/// the shell's own view of it (`viewing`), this package's record, so a shell is picked per
/// person and device without changing what the arrangement holds.
type Arrangement (origin: session.arrangement) {
    workspaces: Collection<ArrangementWorkspace>
    rows: Collection<ArrangementRow>
    strips: Collection<ArrangementStrip>
    /// Every place, each naming the strip it is in, so the views need not walk the tree.
    places: Collection<ArrangementPlace>
}

/// A workspace the person's desk shows.
type ArrangementWorkspace (origin: session.arrangement_workspace) {
    key: Text merge register
    name: Text merge register
    /// Whether it is the workspace in front, which the desk shows.
    current: Bool
}

/// A row of the desk: a strip across a workspace's width, holding columns.
type ArrangementRow (origin: session.arrangement_row) {
    key: Text merge register
    workspace: Text merge register
    name: Text merge register
    /// The shell that made it; empty reads as this shell's.
    shell: Text merge register
}

/// A strip: a column of places, with a width, in a row of a workspace.
type ArrangementStrip (origin: session.arrangement_strip) {
    key: Text merge register
    workspace: Text merge register
    row: Text merge register
    /// Its width in per-mille of its row's usable width (333 narrow, 667 normal, 1000 wide).
    width: Int
    maximized: Bool
    /// The place in front, by its id; empty for none.
    active: Text merge register
    /// Whether it has a place on its workspace's canvas.
    placed: Bool
    canvas_x: Int
    canvas_y: Int
    canvas_width: Int
    canvas_height: Int
    shell: Text merge register
}

/// A place: one window, showing a resource through a view. A place is the shell's view of a
/// resource, so two places may show one resource ("another view", §3), each with its own state.
type ArrangementPlace (origin: session.arrangement_place) {
    key: Text merge register
    /// The strip it is in.
    strip: Text merge register
    /// The app showing the resource, by its canonical id (`so.tilo.terminal`).
    shows: Text merge register
    /// The view it shows the resource through; empty for the app's default.
    shown_as: Text merge register
    /// The record it shows, when it shows one.
    record: Text merge register
    /// The title it is shown under.
    title: Text merge register
    /// What this place is called, on every surface: its window's title and its app's own name
    /// (`Groceries` in `Notes`, the one the person picked), or the app's name alone when the
    /// window has no title. The session resolves it, so the bar, the chips, the switcher, the
    /// overview and search never disagree, and none of them falls back to an app id.
    name: Text merge register
    /// Whether the person pinned it: an agent's arrangement leaves it alone (§5).
    pinned: Bool
    /// The shell that made it; empty reads as this shell's.
    shell: Text merge register
}

/// How wide a strip stands within its row, as the record keeps it in per-mille (§2's strip
/// widths: the presets and free widths).
enum Width {
    /// Three tenths of the row: a list, a set of options.
    narrow
    /// Two thirds of the row: a note, a conversation.
    normal
    /// The row's whole usable width.
    wide
}

/// A way of laying out the same resources and actions, which the person picks: a preset of
/// shell.md §6, whose finer settings live underneath (§8). Each is a shell's root view.
enum Preset {
    /// Columns of places, scrolled horizontally, with strips stacked and scrolled vertically.
    strips
    /// Places at points on a pannable, zoomable surface, each drawn in a frame.
    frames
    /// Places at points, each drawn as its own view: resources standing as content on one
    /// surface, grouped by where they are rather than by a frame around them.
    spaces
    /// The traditional desktop: frames on a floating desk, the focused window's own commands in the
    /// top bar, and a floating dock at the foot, the shape macOS and GNOME people know.
    desktop
}

/// The width a strip's per-mille stands for, as the nearest of the three.
fn width_of(permille: Int) -> Width {
    if permille <= 500 { .narrow } else if permille < 900 { .normal } else { .wide }
}

/// The per-mille one of the widths stands for.
fn permille_of(w: Width) -> Int {
    match w {
        .narrow => 333,
        .normal => 667,
        .wide => 1000,
    }
}

/// What the person shows of their arrangement, and which strip stands in front: the shell's own
/// view of it, in their own record (`records: .own`, `root viewing: Viewing`). The arrangement
/// is the session's and the same whichever way it is shown; this is what changes when the
/// person picks a preset or brings a strip forward, and switching keeps every place where it was.
type Viewing {
    /// Which preset the person shows.
    showing: Preset = .strips
    /// The strip in front, by its key; empty for none.
    focused: Text
}

root viewing: Viewing

default view viewing_settings for Viewing

/// The shell's own settings: the preset the desk shows, and where to pick another (§6, §8).
view viewing_settings(v: Viewing) {
    page {
        section("Desk") {
            ui.Text("The same places, shown four ways. Switching keeps every window where it is.")
            preset_switch(v)
        }
    }
}

/// The preset the person shows, as the string the session publishes it as (`strips`, `frames`,
/// `spaces`).
fn showing_text(v: Viewing) -> Text {
    match v.showing {
        .strips => "strips",
        .frames => "frames",
        .spaces => "spaces",
        .desktop => "desktop",
    }
}

/// The preset `text` names, or the strips for one this build does not know: a record from a
/// newer shell still opens.
fn preset_named(text: Text) -> Preset {
    if text == "frames" {
        .frames
    } else if text == "spaces" { .spaces } else if text == "desktop" { .desktop } else { .strips }
}

/// Shows the desk through `shown` (§6): the scrolling strips, the framed canvas or the spaces
/// canvas. Switching keeps the arrangement: the same places stand where they were, drawn
/// another way.
action show_preset(v: Viewing, shown: Preset) (icon: .layout_grid, label: "Show as") {
    require v.showing != shown "It is shown that way already"
    v.showing = shown
}

/// Makes the strip `strip` the one in front: what a press on a column's header does.
action focus_strip(v: Viewing, strip: Text) (icon: .columns_2, label: "In front") {
    v.focused = strip
}
