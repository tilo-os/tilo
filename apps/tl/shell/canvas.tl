/// The canvases of shell.md §2 and §8: places at points on a surface that pans and zooms, each
/// drawn in a frame (`frames`), or as its own view shown as content and grouped by where it
/// is grouped by where it is, a cluster per task (`frameless`).
///
/// Pan and zoom are the map component's (`ui.Map`) camera: each person's lens, kept on their
/// device, never in the layout. What this file gives the map is where every place stands,
/// in canvas pixels. The document keeps a canvas place on each strip (`canvas_x`, `canvas_y`,
/// `canvas_width`, `canvas_height`); the places of a strip stand in a row from that corner, so a
/// strip of several windows is one run of work and a strip of one is one window where it was
/// dropped (canvas.md §6).
/// The canvas: every strip at its point, its places side by side from there.
/// Panning and zooming is the map's camera (a middle drag, Ctrl and a wheel, the arrow keys, two
/// fingers); it moves the lens and nothing in the layout, so a place stays where it was
/// while the person looks around, and the places keep their canvas points.
/// The map fills the shell: it cuts off what it holds at its edge, so a map only as tall as its
/// content would show none of it.
///
/// A resource dragged out of a view — a message's attachment chip, a search hit, a row of
/// another view — and let go over the canvas is placed at the point it was dropped: the map
/// reports the drop with the canvas point under the pointer, and the shell runs the system's
/// `place`, which is what an agent's `place_at` runs too. The shell never handles the drop
/// itself, exactly as it never handles an attachment's press.
///
/// Pointing is the canvas's one gesture (canvas.md §1): a click on free space puts a caret there
/// and a rectangle dragged on free space puts the caret inside it, and what is typed at the caret
/// is placed there: what it finds fills the rectangle, and what it asks stands as the turn's
/// answer there (`session.place_found`). The map reports both in canvas pixels (`press`,
/// `frame`), and a press on a place stays the place's. Escape takes the caret away.
///
/// Free space invites writing (`canvas.md` §7): over it the pointer is a text cursor, its ground
/// has a grain so faint it only says "surface", and until the person has asked once the empty
/// canvas says at its middle what it is for. That it has been said is the person's document
/// (`Shell.canvas_asked`), so it is said once, never on every empty canvas.
///
/// An answer arrives from where it was asked: each place grows out of the caret or the rectangle
/// the person pointed at (`grows`), and a turn hugs its words up to the rectangle it was asked in
/// (`hug`), where a find fills it.
view canvas(a: Layout, name: Text, framed: Bool, lists: Bool) {
    state typed: Text = ""
    // Where the person pointed, in canvas pixels: a point (`x`, `y`) for a click, a rectangle
    // (`x`, `y`, width, height) for a drag, and nothing while no caret stands.
    state pointed: List<Int> = []
    // The place a right press pointed at, by its key: what the words said there are about.
    // Empty for free space and for a click.
    state pointed_at: Text = ""
    // What the person saw of the canvas as they pointed, in canvas pixels (`x`, `y`, width,
    // height): the agent's views beside their answer stand in free room inside it, so what they
    // asked for stays in view (canvas.md §7).
    state seen: List<Int> = []
    ui.Map(
        name: name,
        ground: true,
        outline: pointed,
        hint: if shell.canvas_asked { "" } else { "Click anywhere and say what you want" },
        fill,
    ) {
        on drop(target: host.Resource, at: List<Int>): place_dropped(target, at)
        on seen(at: List<Int>): set(seen, at)
        on press(at: List<Int>): set(pointed, at).then(set(pointed_at, ""))
        on frame(at: List<Int>): set(pointed, at).then(set(pointed_at, ""))
        // A right press the canvas answers itself — free space, or a place whose content took
        // none — points there as a click does: the caret stands at it.
        on context(target: Text, at: List<Int>): set(pointed, at).then(set(pointed_at, target))
        on resize(target: Text, at: List<Int>): size_place(target, at)
        // The height a hugging place is drawn at, in canvas pixels, as the map lays it out: the
        // area under it is free for what the person asks there, though the place could grow into
        // it (canvas.md §1, §7). The session keeps it as view state, never a step of arranging.
        on drawn(target: Text, at: List<Int>): drew_place(target, at)
        for s in a.strips {
            if s.placed {
                for p in places_of(a, s.key) {
                    ui.MapPin(
                        name: p.key,
                        item: p.key,
                        x: pin_x(a, s, p),
                        y: s.canvas_y,
                        width: pin_width(s),
                        height: pin_height(s),
                        band: framed and composed(p) and not s.leaving,
                        hug: hugs(s, p) and not s.leaving,
                        grows: not s.leaving,
                        leaving: s.leaving,
                    ) {
                        // A place's handle shows in a band over its view: the pin stands that
                        // much higher, so the view's top is where the place was put. A window
                        // whose place just left stays at its spot and plays its departure.
                        if s.leaving {
                            leaving_place(s, p)
                        } else if framed {
                            canvas_place(a, p, lists and p.key == front_place(a), fill)
                        } else {
                            frameless_place(a, p)
                        }
                    }
                }
            }
        }
        if pointed.len() > 0 {
            ui.MapPin(
                name: "Caret",
                x: (pointed.at(0) ?? 0) + caret_inset(pointed) - composer_padding(),
                y: (pointed.at(1) ?? 0) + caret_inset(pointed) - composer_padding(),
                width: caret_room(
                    pointed.at(2) ?? 0,
                    (seen.at(0) ?? 0) + (seen.at(2) ?? 0) - (pointed.at(0) ?? 0),
                ),
                height: composer_height(),
            ) {
                // The card stands its padding up and to the left, so the caret in its field is
                // where the person pointed.
                composer(typed, pointed, seen)
            }
        }
    }
}

/// The composer a click or a rectangle opens where the person pointed: one quiet card holding the
/// line they type what they want on, the caret already in it, and a send that does what Enter
/// does. Escape takes it away; once sent it goes, and the turn stands where it was.
view composer(typed: State<Text>, pointed: State<List<Int>>, seen: State<List<Int>>) {
    ui.Card {
        row {
            ui.TextInput(
                bind: typed,
                placeholder: "Ask or find anything",
                autofocus: true,
                caret: true,
                fill,
            ) {
                on submit: session
                    .place_found(
                        text: typed,
                        view: .none,
                        strip: .none,
                        x: pointed.at(0) ?? 0,
                        y: pointed.at(1) ?? 0,
                        width: pointed.at(2) ?? 0,
                        height: pointed.at(3) ?? 0,
                        seen_x: seen.at(0) ?? 0,
                        seen_y: seen.at(1) ?? 0,
                        seen_width: seen.at(2) ?? 0,
                        seen_height: seen.at(3) ?? 0,
                    )
                    .then(set(typed, ""))
                    .then(set(pointed, []))
                    .then(asked_on_canvas(shell))
                on cancel: set(typed, "").then(set(pointed, []))
            }
            model_choice()
            ui.Button(
                "Send",
                icon: .send,
                icon_only: true,
                role: .primary,
                size: .small,
                enabled: typed != "",
                description: "Type what you want first",
            ) {
                on press: session
                    .place_found(
                        text: typed,
                        view: .none,
                        strip: .none,
                        x: pointed.at(0) ?? 0,
                        y: pointed.at(1) ?? 0,
                        width: pointed.at(2) ?? 0,
                        height: pointed.at(3) ?? 0,
                        seen_x: seen.at(0) ?? 0,
                        seen_y: seen.at(1) ?? 0,
                        seen_width: seen.at(2) ?? 0,
                        seen_height: seen.at(3) ?? 0,
                    )
                    .then(set(typed, ""))
                    .then(set(pointed, []))
                    .then(asked_on_canvas(shell))
            }
        }
    }
}

/// The model the agent answers with, beside the line the person types on: its name, and every
/// model of the providers the person lets agents use, to answer with another. It is the one
/// choice Settings keeps (`settings.agent_models`): what is picked here is the model there.
view model_choice() {
    let m = settings.agent_models
    if m.in_use.model_label != "" {
        ui.MenuButton(m.in_use.model_label, icon: .sparkles, role: .plain) {
            for p in m.providers {
                if p.enabled {
                    for each in p.models {
                        ui.MenuItem(
                            if m.providers.len() > 1 {
                                "{each.label} ({p.label})"
                            } else { each.label },
                        ) {
                            on choose: settings.use_model(m, p.id, each.id)
                        }
                    }
                }
            }
        }
    }
}

/// How tall the composer stands, in canvas pixels: its one line, at the target a finger presses
/// where the window is held in the hand, and the card's padding.
fn composer_height() -> Int {
    64
}

/// How far the composer's field stands inside its card, in canvas pixels: the card's padding.
fn composer_padding() -> Int {
    16
}

/// Whether the place `p` of the strip `s` hugs what it holds up to its box rather than filling
/// it (`canvas.md` §7, "The card hugs what it holds"): a turn, whose answer is as long as its
/// words, and a thing found where the person clicked, which named no size (`canvas_hugs`). What
/// a find placed in a rectangle the person dragged fills it.
fn hugs(s: LayoutStrip, p: LayoutPlace) -> Bool {
    p.shown_as == "turn" or s.canvas_hugs
}

/// The person asked on the canvas: the hint the empty canvas says until then has been said
/// (`Shell.canvas_asked`), and stays away from now on.
action asked_on_canvas(s: Shell) (label: "Asked on the canvas") {
    s.canvas_asked = true
}

/// How wide the caret's line stands, in canvas pixels: the rectangle's width when the person
/// dragged one narrower than a line, and a line's measure otherwise, no wider than the room `room`
/// from the point to the edge of what the person sees, so a click near the edge puts the whole
/// field in view. The caret never moves the camera: it stands where the person pointed.
fn caret_room(width: Int, room: Int) -> Int {
    let most = if width > 0 and width < 444 { width - 2 * caret_step() } else { 420 }
    let fits = room - caret_step()
    if fits >= 120 and fits < most { fits } else { most }
}

/// How tall the caret's line stands, in canvas pixels: one line of text and its field's room. The
/// caret is where the person just pointed, so it is in view already: a pin as tall as a window
/// would have the canvas pan to show it.
fn caret_line() -> Int {
    40
}

/// How far inside a dragged rectangle the caret stands, in canvas pixels: clear of the
/// rectangle's outline, on the rhythm's sibling step.
fn caret_step() -> Int {
    12
}

/// Where the caret stands from the point the person pointed at: at a click's point, and a step
/// inside a dragged rectangle ([`caret_step`]).
fn caret_inset(pointed: List<Int>) -> Int {
    if pointed.len() > 2 { caret_step() } else { 0 }
}

/// Gives the place `key` the size a drag of its edge or corner left it at, `size` its width and
/// height in canvas pixels: one arranging step of the person's, its corner where it stood.
action size_place(key: Text, size: List<Int>) (label: "Size it") {
    session.size(place: the_place(key), width: size.at(0) ?? 720, height: size.at(1) ?? 520)
}

/// Keeps how tall the hugging place `key` is drawn now, `size` carrying its height in canvas
/// pixels as the map lays it out: the free area under it on the canvas is what the person sees
/// there (canvas.md §1, §7), though the place could grow into it. View state the person's own
/// canvas reports, never a step of arranging: Undo leaves it alone.
action drew_place(key: Text, size: List<Int>) {
    session.drawn(place: the_place(key), height: size.at(0) ?? 0)
}

/// Puts a resource dropped on the canvas where it was let go. A place that already stands on the
/// canvas — the place whose handle was dragged — moves to the point it was let go at, through
/// its own `position` action, so a drag that ends on the canvas moves the place and never stands
/// a second one. Any other resource — a message's attachment, a search hit, a row of another
/// view — is placed at that point through the system's own `place`, which is what an agent's
/// `place_at` runs. Either is one arranging step of the person's, attributed and undoable.
action place_dropped(target: host.Resource, at: List<Int>) (label: "Move it here") {
    let x = snap_x(at.at(0) ?? 0)
    let y = snap_y(at.at(1) ?? 0)
    if let held = place_showing(target.id()) {
        session.position(place: the_place(held.key), x: x, y: y)
    } else {
        session.place(resource: target, x: x, y: y)
    }
}

/// The style's own grid (`frames_snap`, `chrome.tl`): where a place let go at `at` lands on the
/// canvas, so frames line up as a person places them. It is the style's setting, not the map's,
/// so it is read here where the drop commits, and a person who turns it off gets the point they
/// let go at exactly. `grid` is a frame's own pitch, so a grid cell is one frame and its gap.
fn snap_x(at: Int) -> Int {
    if frames_style() and shell.frames_snap { snap(at, canvas_grid()) } else { at }
}

/// See [`snap_x`].
fn snap_y(at: Int) -> Int {
    if frames_style() and shell.frames_snap { snap(at, canvas_grid()) } else { at }
}

/// Whether the style the person shows is the framed canvas: the style whose own settings
/// the grid is.
fn frames_style() -> Bool {
    style_of(viewing) == "so.tilo.shell:frames"
}

/// The nearest multiple of `pitch` to `at`.
fn snap(at: Int, pitch: Int) -> Int {
    if pitch <= 0 {
        return at
    }
    return (at + pitch / 2) / pitch * pitch
}

/// The place a drag names, if the layout holds one: what a drag that ended on the canvas moves
/// rather than placing a second time. A handle carries its resource's address, and a window's bar
/// carries what its place shows — or the place itself (`shell:place:<key>`) where it shows no
/// resource the shell can draw, which is every window a page shows.
fn place_showing(carried: Text) -> Option<LayoutPlace> {
    let a = layout
    if let key = place_key_of(carried) {
        if let held = a.places.find(p -> p.key == key) {
            return held
        }
    }
    a.places.find(p -> p.resource == carried)
}

/// The key a place's own address (`shell:place:<key>`) names; `.none` for a resource address.
fn place_key_of(carried: Text) -> Option<Text> {
    let parts = carried.split(":")
    if parts.len() == 3 and (parts.at(0) ?? "") == "shell" and (parts.at(1) ?? "") == "place" {
        parts.at(2)
    } else { .none }
}

/// How far apart the canvas's grid lines stand, in canvas pixels: a frame's own width (720) and
/// a tenth of it, so a place let go on the grid stands beside its neighbour rather than over it.
/// The framed canvas's own setting (`frames_snap`, `chrome.tl`), so a person who wants their
/// places exactly where they let go turns it off.
fn canvas_grid() -> Int {
    792
}

/// How wide a strip's places stand on the canvas: the width the document keeps for it, so a window
/// that was dropped wide stays wide, and 720 when it kept none.
fn pin_width(s: LayoutStrip) -> Int {
    if s.canvas_width > 0 { s.canvas_width } else { 720 }
}

/// Where the place `p` stands on the canvas: its strip's corner, then the places before it in the
/// strip laid end to end. The document keeps one canvas point on a strip, and the places of the
/// strip stand in a row from it (canvas.md §6), so a strip of several places is a run of work and
/// none stands over another.
fn pin_x(a: Layout, s: LayoutStrip, p: LayoutPlace) -> Int {
    let place = 0
    for q in places_of(a, s.key) {
        if q.key == p.key {
            return s.canvas_x + place * (pin_width(s) + pin_width(s) / 10)
        }
        place += 1
    }
    s.canvas_x
}

/// A place on the canvas: its resource's view on the canvas as content (canvas.md §2),
/// on the theme's window surface with no frame and no title bar. Hovering it, or working in it,
/// shows its handle over its corner: what it is, its resource's actions, pin and close. Picking
/// the handle up carries the place's resource: let go over the canvas, the map reports the drop
/// and the place stands where it was let go; over a composer, it attaches what the place shows.
///
/// A foreign window — an app's own surface the shell cannot compose — keeps its frame
/// (`framed_place`): it is the one thing on the canvas that is a window.
view canvas_place(a: Layout, p: LayoutPlace, lists: Bool) {
    if composed(p) {
        ui.Place(
            name: place_own_title(p),
            carries: p.resource,
            target: placed_resource(p.resource),
            fill,
        ) {
            controls: {
                place_controls(a, p, false)
            }
            body: {
                place_content(p)
            }
        }
    } else {
        framed_place(a, p, lists)
    }
}

/// Whether the place `p` draws its view itself — its resource's view, or the query it stands for
/// while an app answers it — rather than showing an app's window.
fn composed(p: LayoutPlace) -> Bool {
    if let q = query_of(p.key) {
        true
    } else if let r = placed_resource(p.resource) {
        p.shown_as != "" or meta(r).opens
    } else { false }
}

/// What a framed place's bar carries when a person lifts it: the resource the place shows, or the
/// place itself where it shows no resource the shell can draw (a foreign window, and every
/// window a page shows). Let go over the canvas, either moves the place there through the
/// session's own `position`, as one arranging step.
fn place_carries(p: LayoutPlace) -> Text {
    if p.resource != "" { p.resource } else { the_place(p.key).id() }
}

/// A foreign window on the canvas: the shell's frame around the well its window shows in, its
/// controls at the end of the bar. The bar is its handle: the frame carries the place's resource
/// (or the place itself), so a press on the bar's empty area and a drag lifts the whole place.
view framed_place(a: Layout, p: LayoutPlace, lists: Bool) {
    ui.WindowFrame(
        name: place_title(p),
        carries: place_carries(p),
        lists_actions: lists_the_place(p),
        fill,
    ) {
        // A press on the window's bar brings it forward, the keyboard in it.
        on press: session.activate(place: the_place(p.key))
        controls: {
            place_controls(a, p, lists)
        }
        body: {
            place_content(p)
        }
    }
}

/// A place on the frameless canvas: its resource's view shown as content (or its window's
/// well), with a line naming what it is rather than a frame around it. The controls stand
/// outside it, and the content itself is `place_content`'s, as every style draws it.
view frameless_place(a: Layout, p: LayoutPlace) {
    stack(gap: .related, fill) {
        // A frameless place has no frame bar: its content's own header row carries the window's actions,
        // so the shell's controls keep theirs off (`listed`), and each command stands once.
        place_controls(a, p, true)
        ui.Text(place_title(p), role: .label, secondary: true)
        place_content(p)
    }
}

/// How tall a strip's places stand on the canvas: the height the document keeps for it, so a view
/// that fills its place (an editor, a mailbox) has the room it was placed with. A strip placed
/// before it kept one stands as tall as a desktop frame does.
fn pin_height(s: LayoutStrip) -> Int {
    if s.canvas_height > 0 { s.canvas_height } else { 480 }
}
