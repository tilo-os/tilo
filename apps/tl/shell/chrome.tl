/// The person's shell: which parts the top bar and the dock have, in what order, on which
/// edge, how they look and when they show. It is the person's own document, synced to their
/// devices with history and undo; every change is one of the actions below, so an agent
/// acting for the person changes it exactly as they do, and undo takes any of it back.
///
/// shell.md's model: a shell is a view of the person's layout. This document is the
/// layout's chrome — the bar and the dock — and the rest of the person's layout
/// (windows, strips, canvas places) is the shell's own layout beside it.
/// The edge of the screen a part rides.
enum Edge {
    /// Top
    top
    /// Bottom
    bottom
    /// Left
    left
    /// Right
    right
}

/// The corner of the display the cards of what is live dock in.
enum CardCorner {
    /// Top left
    top_leading
    /// Top right
    top_trailing
    /// Bottom left
    bottom_leading
    /// Bottom right
    bottom_trailing
}

/// How a bar or dock sits in its edge: inset with rounded corners, floating over what it
/// covers, or edge to edge and square.
enum Fit {
    floating
    full
}

/// What a bar or dock is made of: the theme's translucent material over what is behind it,
/// or its opaque bar ground.
enum Finish {
    translucent
    solid
}

/// When a bar or dock shows.
enum Showing {
    /// Always
    always
    /// While no window covers it
    ///
    /// It hides while a window would cover it, and comes back when that window goes.
    no_overlap
    /// At the screen's edge
    ///
    /// Hidden until the pointer bumps the edge it rides.
    edge_reveal
    /// After a moment
    ///
    /// Hidden once the pointer has not been over it for a while.
    auto_hide
}

/// How large the parts are.
enum Size {
    /// Small
    small
    /// Regular
    regular
    /// Large
    large
}

/// One part a person can put on the top bar, in their order. The case's icon is the glyph the
/// settings and the drag ghost show for it.
enum BarPartKind {
    /// The system's menu: Tilo, and the device's own commands.
    menu (icon: .tilo)
    /// The workspace in front, opening the workspaces to switch between.
    workspace (icon: .layers)
    /// The focused app's menus: what that app can do now, on its resource.
    app_menu (icon: .menu)
    /// The strip in front.
    strip (icon: .columns_2)
    /// The tasks running for the person.
    tasks (icon: .bot)
    /// The time.
    clock (icon: .clock)
    /// Who else is here, the person, and the roster of everyone.
    people (icon: .users)
    /// The device's status (the sound, the connection and the battery) and the notifications.
    status (icon: .bell)
    /// The battery on its own, retired: the status part shows it now.
    power (icon: .battery)
    /// The agent's own indicator: a task working for the person.
    agent (icon: .sparkles)
    /// A flexible gap: the bar's parts before it lead, those after it trail.
    spacer (icon: .minus)
}

/// A named set of the settings, which the person applies in one action: a chrome preset.
/// `Viewing.style` (layout.tl) is the *view* — how the same places are shown — and is
/// a different setting; this one names the bar and the dock.
enum ChromePreset {
    /// The dock on the bottom with the bar on the top: Tilo's own.
    tilo (icon: .layout_grid)
    /// A floating, rounded, translucent dock at the bottom that hides while a window covers it, and a top bar with the focused app's menus after the system's.
    macos (icon: .monitor)
    /// A full-width taskbar at the bottom, always shown.
    windows (icon: .panel_right)
    /// The top bar alone; the overview on demand.
    gnome (icon: .menu)
    /// No chrome at rest; the edges reveal what the person needs.
    minimal (icon: .minus)
    /// The traditional desktop: frames floating over the shell, the focused window's own commands in the top bar, and a floating dock at the foot, the shape macOS and GNOME people know.
    windows_desktop (icon: .app_window)
}

/// The person's own shell document, reached as `shell` (`root shell: Shell`).
type Shell {
    /// The preset the settings were last made from; `tuned` says whether the person has
    /// changed a setting since.
    preset: ChromePreset = .tilo
    /// The preset the settings were last made from, kept while the person tunes one thing,
    /// so the picker says "Custom, based on …" rather than losing where they started.
    based_on: ChromePreset = .tilo
    /// Whether the person has changed a setting since choosing a preset.
    tuned: Bool = false
    /// The top bar.
    bar_shown: Bool = true
    bar_edge: Edge = .top
    bar_fit: Fit = .full
    bar_finish: Finish = .solid
    bar_showing: Showing = .always
    bar_size: Size = .regular
    bar_parts: Collection<BarPart>
    /// The dock.
    dock_shown: Bool = true
    dock_edge: Edge = .bottom
    dock_fit: Fit = .floating
    dock_finish: Finish = .translucent
    dock_showing: Showing = .always
    dock_size: Size = .regular
    dock_items: Collection<DockItem>
    /// The corner of the display in use the cards of what is live dock in: the person's choice,
    /// on every device they use.
    cards_corner: CardCorner = .bottom_trailing
    /// The dock shows the apps running even when nobody pinned them.
    dock_running: Bool = true
    /// The dock has a launcher entry.
    dock_launcher: Bool = true
    /// A click on the dock's icons magnifies the item under the pointer, as macOS's Dock
    /// does.
    dock_zoom: Bool = false
    /// How much larger a dock item is under the pointer, when it magnifies: a little over half
    /// again as large at its middle (`0.6`), falling away to its own size a few items off.
    dock_zoom_amount: Float = 0.6
    /// Whether the chosen style's own settings stand over the shell (`style_settings`), opened
    /// from the bar's style menu: the one place a person changes the style they show without an
    /// agent, as `shell.md` §6 says. It is the person's document, so
    /// it survives a switch and an agent opens and closes it as they do.
    settings_open: Bool = false
    /// Strips: whether a strip of several places shows its tab bar. Off, the strip shows the
    /// place in front alone and the others are reached by the window's own menu.
    strips_tabs: Bool = true
    /// Frames: whether a place let go on the canvas lands on the style's grid, so frames line
    /// up as they are placed. Off, a place stays exactly where it was let go.
    frames_snap: Bool = true
    /// The canvas: whether the person has asked on it yet. Until they have, the empty canvas
    /// says once, quietly, what it is for ("Click anywhere and say what you want"); the first
    /// ask puts that away for good, on every device the person uses.
    canvas_asked: Bool = false
    /// Whether the report picker stands over the windows: the person is choosing the window a
    /// report about a problem is about (`feedback.tl`).
    reporting: Bool = false
    /// The windows the report being made is about, in the order they were picked: the shell's own
    /// record of what the person pointed at, so a report is about something they can read back.
    report_picks: List<ReportPick>
    /// The outline of the rectangle the person dragged over the picker, as `x, y, width, height`
    /// in canvas pixels: empty while no drag stands.
    report_area: List<Int>
}

/// One part of the top bar, in the person's order.
type BarPart {
    /// What the part is; the bar draws it from this, so a part moved is a part the same, and its
    /// name is the one that part goes by.
    kind: BarPartKind = .menu
    /// Whether it shows on the bar; a part off keeps its place in the document.
    shown: Bool = true
}

/// What a part of the bar is called, on the bar and in the settings, from what it is: this
/// package's words, so a shell shows them in the person's language.
fn part_name(kind: BarPartKind) -> Text {
    if kind == .menu {
        std.say("Tilo")
    } else if kind == .workspace {
        std.say("Workspace")
    } else if kind == .app_menu {
        std.say("App menus")
    } else if kind == .strip {
        std.say("Strip")
    } else if kind == .tasks {
        std.say("Tasks")
    } else if kind == .clock {
        std.say("Clock")
    } else if kind == .people {
        std.say("People")
    } else if kind == .status {
        std.say("Status")
    } else if kind == .power {
        std.say("Battery")
    } else if kind == .agent { std.say("Agents") } else { std.say("Gap") }
}

/// One thing on the dock: a pinned app or resource, the launcher, or an app running now.
type DockItem {
    /// What the item stands for: an app id, or a resource's reference as text.
    key: Text
    /// Its name on the dock, and what assistive technology reads: the app's or the resource's own,
    /// and empty for the launcher, which the dock names from this package's words.
    title: Text = ""
    /// The glyph it shows, by its icon's name.
    icon: Text
    /// Whether the person pinned it; a running app that is not pinned is there while it runs.
    pinned: Bool = true
    /// Whether it runs now.
    running: Bool = false
    /// How many things it has new, as its badge shows.
    badge: Int = 0
    /// Whether it wants the person: a pulsing badge, and an attention mark.
    attention: Bool = false
}

root shell: Shell

/// What a new shell starts with: the bar's parts and the dock's, in the order the default shell
/// shows them. The person arranges them from then on.
initial action seed(s: Shell) {
    s.bar_parts.insert(BarPart(kind: .menu))
    s.bar_parts.insert(BarPart(kind: .workspace))
    s.bar_parts.insert(BarPart(kind: .strip))
    s.bar_parts.insert(BarPart(kind: .agent, shown: false))
    s.bar_parts.insert(BarPart(kind: .spacer))
    s.bar_parts.insert(BarPart(kind: .clock))
    s.bar_parts.insert(BarPart(kind: .spacer))
    s.bar_parts.insert(BarPart(kind: .people))
    s.bar_parts.insert(BarPart(kind: .status))
    if s.dock_launcher {
        s.dock_items.insert(DockItem(key: "launcher", icon: "layout-grid"))
    }
}

/// A shell that kept the retired `power` part beside the status loses it.
migration action drop_the_power_part(s: Shell) {
    s.bar_parts.remove_where(p -> p.kind == BarPartKind.power)
}

/// A shell that kept the retired `tasks` part loses it: what an agent does is the turn of a
/// conversation, which Chat shows, so the bar has no running-tasks cluster.
migration action drop_the_tasks_part(s: Shell) {
    s.bar_parts.remove_where(p -> p.kind == BarPartKind.tasks)
}

/// Uses the preset `layout`: every setting it names at once. The result is exactly that preset,
/// so the picker shows it chosen and "custom" is off.
primary action apply(s: Shell, layout: ChromePreset) (
    icon: .layout_grid,
    label: "Use this layout",
) {
    s.preset = layout
    s.based_on = layout
    s.tuned = false
    if layout != .macos and layout != .windows_desktop {
        s.bar_parts.remove_where(p -> p.kind == BarPartKind.app_menu)
    }
    if layout == .tilo {
        s.bar_shown = true
        s.bar_edge = .top
        s.bar_fit = .full
        s.bar_finish = .solid
        s.bar_showing = .always
        s.bar_size = .regular
        s.dock_shown = true
        s.dock_edge = .bottom
        s.dock_fit = .floating
        s.dock_finish = .translucent
        s.dock_showing = .always
        s.dock_size = .regular
        s.dock_running = true
        s.dock_launcher = true
        s.dock_zoom = false
    } else if layout == .macos {
        s.bar_shown = true
        s.bar_edge = .top
        s.bar_fit = .full
        s.bar_finish = .translucent
        s.bar_showing = .always
        s.bar_size = .small
        s.dock_shown = true
        s.dock_edge = .bottom
        s.dock_fit = .floating
        s.dock_finish = .translucent
        s.dock_showing = .no_overlap
        s.dock_size = .regular
        s.dock_running = true
        s.dock_launcher = true
        s.dock_zoom = true
    } else if layout == .windows {
        s.bar_shown = false
        s.dock_shown = true
        s.dock_edge = .bottom
        s.dock_fit = .full
        s.dock_finish = .solid
        s.dock_showing = .always
        s.dock_size = .regular
        s.dock_running = true
        s.dock_launcher = true
        s.dock_zoom = false
    } else if layout == .windows_desktop {
        s.bar_shown = true
        s.bar_edge = .top
        s.bar_fit = .full
        s.bar_finish = .translucent
        s.bar_showing = .always
        s.bar_size = .small
        s.dock_shown = true
        s.dock_edge = .bottom
        s.dock_fit = .floating
        s.dock_finish = .translucent
        s.dock_showing = .always
        s.dock_size = .regular
        s.dock_running = true
        s.dock_launcher = true
        s.dock_zoom = true
    } else if layout == .gnome {
        s.bar_shown = true
        s.bar_edge = .top
        s.bar_fit = .full
        s.bar_finish = .solid
        s.bar_showing = .always
        s.bar_size = .small
        s.dock_shown = false
        s.dock_showing = .always
        s.dock_running = true
        s.dock_launcher = true
    } else {
        s.bar_shown = true
        s.bar_edge = .top
        s.bar_fit = .full
        s.bar_finish = .translucent
        s.bar_showing = .edge_reveal
        s.bar_size = .small
        s.dock_shown = false
        s.dock_showing = .edge_reveal
        s.dock_running = true
        s.dock_launcher = true
    }
    if (layout == .macos or layout == .windows_desktop)
        and not s.bar_parts.any(p -> p.kind == BarPartKind.app_menu) {
        s.bar_parts.insert(BarPart(kind: .app_menu))
        if let brand = s.bar_parts.find(p -> p.kind == BarPartKind.app_menu),
            after = s.bar_parts.find(p -> p.kind == BarPartKind.menu) {
            s.bar_parts.move(brand, after: after)
        }
    }
}

/// Puts the focused app's menus on the bar, after the system's: what the app in front can do
/// now, on its resource.
action show_app_menus(s: Shell) (icon: .menu, label: "Show the app's menus", group: "Top bar") {
    require not s.bar_parts.any(p -> p.kind == BarPartKind.app_menu) "The app's menus are on the bar already"
    s.bar_parts.insert(BarPart(kind: .app_menu))
    if let menus = s.bar_parts.find(p -> p.kind == BarPartKind.app_menu),
        brand = s.bar_parts.find(p -> p.kind == BarPartKind.menu) {
        s.bar_parts.move(menus, after: brand)
    }
    s.tuned = true
}

/// Takes the focused app's menus off the bar.
action hide_app_menus(s: Shell) (icon: .menu, label: "Hide the app's menus", group: "Top bar") {
    s.bar_parts.remove_where(p -> p.kind == BarPartKind.app_menu)
    s.tuned = true
}

/// Returns the shell to the preset it was based on, dropping the person's tuning.
action reset(s: Shell) (icon: .rotate_ccw, label: "Back to the preset") {
    require s.tuned "Nothing was changed from the preset"
    apply(s, s.based_on)
}

/// Puts the bar on `edge`.
action bar_on(s: Shell, edge: Edge) (
    icon: .panel_right,
    label: "Move the bar to the {edge}",
    group: "Top bar",
) {
    s.bar_edge = edge
    s.tuned = true
}

action set_bar_fit(s: Shell, fit: Fit) (label: "Bar: {fit}", group: "Top bar") {
    s.bar_fit = fit
    s.tuned = true
}

action set_bar_finish(s: Shell, finish: Finish) (label: "Bar: {finish}", group: "Top bar") {
    s.bar_finish = finish
    s.tuned = true
}

action set_bar_showing(s: Shell, showing: Showing) (label: "Bar: {showing}", group: "Top bar") {
    s.bar_showing = showing
    s.tuned = true
}

action set_bar_size(s: Shell, size: Size) (label: "Bar size: {size}", group: "Top bar") {
    s.bar_size = size
    s.tuned = true
}

/// Shows or hides the top bar: the one control that turns it on and off, so a person sees
/// whether it is on where they press it.
action show_bar(s: Shell, on: Bool (label: "Shown") = not s.bar_shown) (
    icon: .eye,
    label: "Show the top bar",
    on: s.bar_shown,
) {
    s.bar_shown = on
    s.tuned = true
}

/// Moves the dock to `edge`, keeping how it is built.
action dock_on(s: Shell, edge: Edge) (
    icon: .panel_right,
    label: "Move the dock to the {edge}",
    group: "Dock",
) {
    s.dock_edge = edge
    s.tuned = true
}

/// Docks the cards of what is live in the corner `corner` of the display in use.
action dock_cards(s: Shell, corner: CardCorner) (
    icon: .layout_grid,
    label: "Cards at the {corner}",
    group: "Cards",
) {
    s.cards_corner = corner
}

action set_dock_fit(s: Shell, fit: Fit) (label: "Dock: {fit}", group: "Dock") {
    s.dock_fit = fit
    s.tuned = true
}

action set_dock_finish(s: Shell, finish: Finish) (label: "Dock: {finish}", group: "Dock") {
    s.dock_finish = finish
    s.tuned = true
}

action set_dock_showing(s: Shell, showing: Showing) (label: "Dock: {showing}", group: "Dock") {
    s.dock_showing = showing
    s.tuned = true
}

action set_dock_size(s: Shell, size: Size) (label: "Dock size: {size}", group: "Dock") {
    s.dock_size = size
    s.tuned = true
}

/// Shows or hides the dock: the one control that turns it on and off, so a person sees
/// whether it is on where they press it.
action show_dock(s: Shell, on: Bool (label: "Shown") = not s.dock_shown) (
    icon: .eye,
    label: "Show the dock",
    on: s.dock_shown,
) {
    s.dock_shown = on
    s.tuned = true
}

/// Whether the dock shows the apps running that nobody pinned.
action set_dock_running(s: Shell, on: Bool (label: "In the dock")) (
    label: "Running apps",
    group: "Dock",
) {
    s.dock_running = on
    s.tuned = true
}

/// Whether the dock has a launcher entry.
action set_dock_launcher(s: Shell, on: Bool (label: "In the dock")) (
    label: "Launcher entry",
    group: "Dock",
) {
    s.dock_launcher = on
    if on and not s.dock_items.any(i -> i.key == "launcher") {
        s.dock_items.insert(DockItem(key: "launcher", icon: "layout-grid"))
    }
    if not on {
        s.dock_items.remove_where(i -> i.key == "launcher")
    }
    s.tuned = true
}

/// Whether the dock magnifies the item under the pointer.
action set_dock_zoom(s: Shell, on: Bool (label: "Under the pointer")) (
    label: "Magnify the dock",
    group: "Dock",
) {
    s.dock_zoom = on
    s.tuned = true
}

/// How much larger the item under the pointer stands: more is a bolder dock, and nothing
/// leaves every item its size.
action set_dock_zoom_amount(s: Shell, amount: Float (label: "Magnification")) (
    label: "How much the dock magnifies",
    group: "Dock",
) {
    s.dock_zoom_amount = amount
    s.tuned = true
}

/// Puts a bar part with the one it comes before, wherever the part stands now: the one action
/// a drag commits, and the same change from the settings, a shortcut or an agent.
action move_part(s: Shell, part: BarPart, before: BarPart) (icon: .arrow_up_down, label: "Move") {
    require part != before "A part cannot come before itself"
    s.bar_parts.move(part, before: before)
    s.tuned = true
}

/// Puts a bar part at `position`, counted from the leading edge: what a drag that names a
/// place gives.
action place_part(s: Shell, part: BarPart, position: Int) (icon: .arrow_up_down, label: "Move") {
    s.bar_parts.move(part, to: position)
    s.tuned = true
}

/// Shows or hides one part of the bar.
action show_part(s: Shell, part: BarPart, on: Bool (label: "On the bar")) (
    icon: .eye,
    label: "Shown on the bar",
) {
    part.shown = on
    s.tuned = true
}

/// Puts a dock item with the one it comes before: the one action a drag commits.
action move_item(s: Shell, item: DockItem, before: DockItem) (icon: .arrow_up_down, label: "Move") {
    require item != before "An item cannot come before itself"
    s.dock_items.move(item, before: before)
    s.tuned = true
}

/// Puts a dock item at `position`, counted from the leading edge: what a drag that names a
/// place gives. An item the person's document does not hold yet — an app running on the dock
/// that they never pinned, which the dock drew from the shell — is pinned where it lands, so
/// moving an app they already run keeps it on the dock after it closes.
action place_item(s: Shell, item: DockItem, position: Int) (icon: .arrow_up_down, label: "Move") {
    if s.dock_items.any(i -> i.key == item.key) {
        let held = s.dock_items.find(i -> i.key == item.key)
        if let known = held { s.dock_items.move(known, to: position) }
    } else {
        let made = DockItem(
            key: item.key,
            title: item.title,
            icon: item.icon,
            pinned: true,
            running: item.running,
            badge: item.badge,
            attention: item.attention,
        )
        s.dock_items.insert(made)
        s.dock_items.move(made, to: position)
    }
}

/// Pins a resource or an app to the dock, before `next` (or at the end when no neighbour is
/// named): what dragging one onto the dock does, and what the person does from a menu.
action pin(
    s: Shell,
    key: Text (label: "App"),
    title: Text (label: "Title"),
    icon: Text (label: "Icon"),
    next: Text (label: "Before") = "",
) (icon: .pin, label: "Pin to the dock") {
    require not s.dock_items.any(i -> i.key == key) "{title} is on the dock already"
    let item = DockItem(key: key, title: title, icon: icon, pinned: true)
    s.dock_items.insert(item)
    if let before = s.dock_items.find(i -> i.key == next) {
        s.dock_items.move(item, before: before)
    }
    s.tuned = true
}

/// Takes an item off the dock.
action unpin(s: Shell, item: DockItem) (icon: .pin, label: "Remove from the dock") {
    s.dock_items.remove(item)
    s.tuned = true
}

/// Whether a bar or dock of `when` shows now, given whether the pointer is at its edge
/// (`reached`) and whether a window stands where it rides (`covered`). The view draws it only
/// while this holds; the four settings read as a person reads them: always; while no window
/// stands where it rides; when the pointer comes to its edge; and, hiding after the pointer
/// leaves, back when it comes to the edge or to the bar itself.
///
/// `auto_hide` and `edge_reveal` both show on a reach, so one gesture — the pointer bumping the
/// screen edge — reveals either, and neither shows while a window covers it and the pointer is
/// elsewhere.
fn shows(when: Showing, reached: Bool, covered: Bool) -> Bool {
    if when == .always { true } else if when == .no_overlap { not covered } else { reached }
}

/// What one display says of its own edges: whether the pointer is at its top or bottom edge, and
/// whether a window stands where a bar or a dock there would ride. The device publishes a row for
/// every display (`device.displays.displays`), so a bar that hides until it is reached for shows
/// on the display reached and on no other.
type Edges {
    bar_reached: Bool
    bar_covered: Bool
    dock_reached: Bool
    dock_covered: Bool
}

/// The edges of the display this part draws on: its own row of the device's displays, found by the
/// connector the part's screen names. A surface that draws on no display of the device (a page,
/// a shot) has nothing of the device's pointer or its windows on it: no pointer reaches its edge
/// and no window stands where its bar or its dock rides, so its chrome shows as the person set
/// it. The device's summary is no answer there — it is what some other display says, and a page
/// has no share of it.
fn edges_here() -> Edges {
    let connector = device.screen.connector
    if connector != "" {
        let displays = device.displays
        if let display = displays.displays.find(d -> d.connector == connector) {
            return Edges(
                bar_reached: display.bar_reached,
                bar_covered: display.bar_covered,
                dock_reached: display.dock_reached,
                dock_covered: display.dock_covered,
            )
        }
    }
    return Edges(bar_reached: false, bar_covered: false, dock_reached: false, dock_covered: false)
}

/// Whether the top bar shows now: the person's settings, and what this display says of its edge.
fn bar_shows(s: Shell) -> Bool {
    let edges = edges_here()
    s.bar_shown and shows(s.bar_showing, edges.bar_reached, edges.bar_covered)
}

/// Whether the shell's bar lists the window in front's actions now: the traditional desktop's
/// bar always does (its window menu, at every width), and another bar does while it shows the
/// app's menus part with the room to keep it (`roomy`: the output is expanded or wide, where
/// the part does not give way). Each window's frame is told this fact
/// (`ui.WindowFrame(lists_actions:)`), so the window's own bar keeps its title and the shell's
/// controls alone: each action stands once, in the menu the bar lists it in, never twice.
///
/// The desktop style's chrome (`traditional`), and a shell whose chrome is the traditional
/// desktop's (`bar_is_traditional`), count as one: that bar lists them, so its frames let their
/// bar's actions give way to it.
fn bar_lists_actions(s: Shell, roomy: Bool) -> Bool {
    bar_lists_actions_for(s, roomy, false)
}

/// [`bar_lists_actions`] with `traditional`: the desktop style passes it, so its frames give way
/// to the shell's bar whatever the chrome's own preset.
fn bar_lists_actions_for(s: Shell, roomy: Bool, traditional: Bool) -> Bool {
    if not bar_shows(s) {
        return false
    }
    if traditional or bar_is_traditional(s) {
        return true
    }
    // On the canvas the bar keeps no window menu (`canvas_keeps`), so its frames keep theirs.
    roomy
        and s.bar_parts.any(
            p -> p.kind == BarPartKind.app_menu
                and p.shown
                and (not frames_style() or canvas_keeps(p)),
        )
}

/// Whether the bar is the traditional desktop's own (`traditional_bar`): its preset, unchanged
/// in what it is based on.
fn bar_is_traditional(s: Shell) -> Bool {
    s.based_on == .windows_desktop and s.preset == .windows_desktop
}

/// Whether the dock shows now. Never on the canvas, which is the whole screen (`canvas.md` §7):
/// what a person launches there they ask for where they point. A person who laid the canvas's
/// chrome out as the traditional desktop chose its dock with it, and keeps it.
fn dock_shows(s: Shell) -> Bool {
    let edges = edges_here()
    s.dock_shown
        and (not frames_style() or bar_is_traditional(s))
        and shows(s.dock_showing, edges.dock_reached, edges.dock_covered)
}
