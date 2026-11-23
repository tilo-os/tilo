/// The person's layout: what is open where. It is a row across a workspace, a strip of places in
/// a row, a place showing a resource through a view, which the session keeps with the layout the
/// workspace shares, with history and undo.
///
/// The session reads it off the live layout and publishes its reading as
/// `session.layout`. The shell's views read it and change it only through the arranging
/// actions on `shell:layout` (`so.tilo.session`'s), so a person, an agent and a script
/// arrange the same way, attributed and undoable.
///
/// Every entry names the shell that made it (`shell`): a shell that does not know an entry
/// another shell made keeps it and never changes or removes it.
/// The person's layout as the session reads it off the layout: the workspaces it holds, its
/// rows, its strips, every place, and the window this device has in front. Which style it shows
/// is the shell's own view of it (`viewing`), this package's document, so a shell is picked per
/// person and device without changing what the layout holds.
type Layout (origin: session.layout) {
    /// The person at this device, by their name.
    person: Text merge register
    /// The strip whose window has the keyboard on this device, by its key; empty for none. The
    /// session reads it off this device's shell, so it is never a change of the layout, and a press
    /// that brings a window forward asks the session (`session.activate`).
    front: Text merge register
    /// The others here in the workspace in front.
    people: Collection<LayoutPerson>
    workspaces: Collection<LayoutWorkspace>
    rows: Collection<LayoutRow>
    strips: Collection<LayoutStrip>
    /// Every place, each naming the strip it is in, so the views need not walk the tree.
    places: Collection<LayoutPlace>
    /// The queries the canvas stands while an app answers them, by the place that shows each:
    /// what the person typed, what it found, or why nothing did (shell.md §8).
    queries: Collection<LayoutQuery>
    /// The resources those queries found, each naming the query it is of.
    refs: Collection<LayoutRef>
}

/// A query a place shows while its app answers it: what the person typed, in which app, and what
/// came of it. Its place is the one whose key this is (`LayoutPlace.key`), and a press on a
/// row shows that match in the same place.
type LayoutQuery (origin: session.layout_query) {
    /// Its id: the place showing it.
    key: Text merge register
    /// What the person typed.
    text: Text merge register
    /// The app asked, by its canonical id.
    in_app: Text merge register
    /// The kind of resource the query names, as its app publishes it (`tube.channel`).
    kind: Text merge register
    /// How the search stands: `searching`, `one`, `several`, `none` or `failed`.
    state: Text merge register
    /// Why it failed, in the app's own words; empty while it does better.
    reason: Text merge register
}

/// A resource a query found: its address, the name it goes by, and its kind, the same three words a
/// place's document is named by.
type LayoutRef (origin: session.layout_ref) {
    key: Text merge register
    /// The query it is of, by its key.
    query: Text merge register
    address: Text merge register
    title: Text merge register
    kind: Text merge register
}

/// A workspace the person's layout shows.
type LayoutWorkspace (origin: session.layout_workspace) {
    key: Text merge register
    name: Text merge register
    /// Whether it is the workspace in front, which the shell shows.
    current: Bool
    /// Where the workspace opens as a page on the web (`/w/<workspace>/desktop`), when the shell
    /// is served there: what its card links to. Empty on a device, where switching shows the
    /// workspace in the same layout.
    home: Text merge register
}

/// Someone else here in the workspace in front: a person or an agent.
type LayoutPerson (origin: session.layout_person) {
    key: Text merge register
    name: Text merge register
}

/// A row of the layout: a strip across a workspace's width, holding columns.
type LayoutRow (origin: session.layout_row) {
    key: Text merge register
    workspace: Text merge register
    name: Text merge register
    /// Whether it is the row in front in its workspace.
    current: Bool
    /// The shell that made it; empty reads as this shell's.
    shell: Text merge register
}

/// A strip: a column of places, with a width, in a row of a workspace.
type LayoutStrip (origin: session.layout_strip) {
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
    /// Whether `canvas_height` is the most it takes: what it shows is as tall as what it holds
    /// up to it (a thing found where the person clicked).
    canvas_hugs: Bool
    shell: Text merge register
}

/// A place: one window, showing a resource through a view. A place is the shell's view of a
/// resource, so two places may show one resource, each with its own state.
type LayoutPlace (origin: session.layout_place) {
    key: Text merge register
    /// The strip it is in.
    strip: Text merge register
    /// The app showing the resource, by its canonical id (`so.tilo.terminal`).
    shows: Text merge register
    /// The view it shows the resource through; empty for the app's default.
    shown_as: Text merge register
    /// The document it shows, when it shows one.
    resource: Text merge register
    /// The title it is shown under.
    title: Text merge register
    /// What this place is called, on every surface: its window's title and its app's own name
    /// (`Groceries` in `Notes`, the one the person picked), or the app's name alone when the
    /// window has no title. The session resolves it, so the bar, the chips, the switcher, the
    /// overview and search never disagree, and none of them falls back to an app id.
    name: Text merge register
    /// Whether the person pinned it: an agent's layout leaves it alone.
    pinned: Bool
    /// The shell that made it; empty reads as this shell's.
    shell: Text merge register
}

/// How wide a strip stands within its row, in per-mille of the row's width.
enum Width {
    /// Three tenths of the row: a list, a set of options.
    narrow
    /// Two thirds of the row: a note, a conversation.
    normal
    /// The row's whole usable width.
    wide
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

/// What the person shows of their layout: the shell's own view of it, in their own workspace
/// (`resources: .own`, `root viewing: Viewing`). The layout is the session's and the same
/// whichever way it is shown; this is what changes when the person picks a style, and switching
/// keeps every place where it was.
type Viewing {
    /// The style the person shows, as a view reference: a package and a view
    /// (`so.tilo.shell:strips`, `me.styles:timeline`). A style is a view of the
    /// layout any installed package declares; empty shows this shell's
    /// strips.
    style: Text
}

root viewing: Viewing

default view viewing_settings for Viewing

/// The shell's own settings: the style the person shows, and where to pick another. The
/// styles every installed package offers are listed by their own words; picking one
/// switches the shell, and the style's own settings stand under the list, from its own document.
view viewing_settings(v: Viewing) {
    page {
        section("Shell") {
            ui.Text("Switching a style keeps every window where it is.")
            style_switch(v)
        }
    }
}

/// Shows the shell through the style `style`: a package and a view (`so.tilo.shell:strips`).
/// Switching keeps the layout: the same places stand where they were, drawn another way.
action show_style(v: Viewing, style: Text) (icon: .layout_grid, label: "Show as") {
    require v.style != style "It is shown that way already"
    v.style = style
}
