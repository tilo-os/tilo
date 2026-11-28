/// The shell: what this device shows. The person's bar across the top edge its settings name,
/// the dock on its own edge, and one surface over the windows at a time: the overview, the
/// switcher, search and launching, the notifications and the workspaces. Which
/// one is up is the session's (`device.overlays.here`), so the shell re-runs when it changes and nothing else
/// moves while the shell is at rest.
///
/// The bar and the dock are `ui.Chrome` bodies made of the theme's material for the style the
/// person picked (`bar_finish`, `dock_finish`): the theme's glass, let through and blurred over
/// the windows, or that same glass made calm, opaque with every light off — so a theme's material
/// and not the view decides what either is made of. Edge to edge and square at the `full` fit,
/// an inset rounded body at the floating one. Every surface is drawn
/// over them, so the bar stays reachable and a press outside a surface closes it, as the
/// session's chrome decides. Nothing here draws a window: the windows are the
/// compositor's, behind this view.
///
/// The shell, with the layout `front` filling the room between its bar and its foot, as the
/// style the person chose shows it (`screen`): the dock stands over that room on its own edge,
/// as the chrome's does, so a window takes the height the shell gives it rather than stopping
/// short of the dock. On a compact output that room is a phone's, too little for a window's bar to lose to a
/// floating dock, so the dock takes its own room at the foot instead and the window fills what
/// is left.
///
/// The switcher is a panel over the windows rather than a page of them: it takes that room and
/// centres itself there, where the surfaces that go on past a display are given the same room
/// as a page that scrolls. Until the session says its walk outlasted a flick
/// (`switcher_revealed`) the shell is the one at rest, so a flick between two windows shows
/// nothing over either.
///
/// `traditional` makes this the traditional desktop's chrome: its bar lists the window in
/// front's actions, and its own bar is drawn (`top_bar`). The desktop style passes it.
template view shell_around(front: Option<Layout>, traditional: Bool, content: View) {
    let s = shell
    // A dock that stays in sight takes its own room at the foot, so the windows stop at its top
    // edge rather than running under it. One that hides itself (auto-hide, or reached for at the
    // edge) floats over them until the person brings it out.
    let dock_hides_itself = s.dock_showing != .always
    let dock_takes_room = front != .none
        and dock_shows(s)
        and s.dock_edge == .bottom
        and (space.compact or not dock_hides_itself)
    // A dock up a side edge takes its own column the same way, so the page beside it is never
    // under it; its body keeps the band it rests in while its magnified items grow over it.
    let dock_side = dock_shows(s) and (s.dock_edge == .left or s.dock_edge == .right)
    let side_takes_room = front != .none and dock_side and (space.compact or not dock_hides_itself)
    layers {
        // The shell's wallpaper, covering the whole shell under the windows and the bar: the
        // picture the person chose, or the shell's own gradient where it shows none.
        ui.Backdrop()
        stack(gap: .tight, fill) {
            // The bar and the band stand apart as far as the band stands from a dock's band:
            // the dock's body keeps its `related` inset from the edge it rides plus the stack's
            // `tight`. A bar that rides edge to edge has no inset of its own, so the band takes
            // the whole `sibling` step from it; a floating bar already keeps its `related`
            // inset, and the band takes the rest.
            stack(
                gap: if bar_shows(s) and s.bar_fit != .full { .related } else { .sibling },
                fill,
            ) {
                if bar_shows(s) {
                    bar_on_edge(s, traditional)
                }
                if side_takes_room {
                    // The dock takes its own column beside the band, so nothing of the page
                    // stands under it. Its body keeps the band it rests in, so an item that
                    // magnifies grows over the page without moving it.
                    row(fill) {
                        if s.dock_edge == .left {
                            side_dock(s, .left)
                        }
                        band {
                            content
                        }
                        if s.dock_edge == .right {
                            side_dock(s, .right)
                        }
                    }
                } else {
                    band {
                        content
                    }
                }
            }
            if dock_takes_room {
                // The dock's own height, under the band: `fill` here would share the shell's
                // height between the dock and the windows.
                row(
                    align: .center,
                    distribute: .center,
                    padding: if s.dock_fit == .floating { .related } else { .none },
                ) {
                    dock_ground(s, .bottom)
                }
            }
        }
        // Quick settings beside the caret that asked for it: it stands over the whole shell, at
        // the room the session settled clear of the line the person types on.
        if device.overlays.here == "quick-settings" {
            if let p = device.overlays.place {
                stack(at: .top_leading, x: p.x, y: p.y, height: p.h * 1dp) {
                    quick_settings_panel(fill_height: true, compact: space.compact)
                }
            }
        }
        if dock_shows(s)
            and not dock_takes_room
            and (not side_takes_room or s.dock_edge == .bottom or s.dock_edge == .top) {
            if s.dock_edge == .bottom {
                row(
                    at: .bottom,
                    fill,
                    align: .center,
                    distribute: .center,
                    padding: if s.dock_fit == .floating { .related } else { .none },
                ) {
                    dock_ground(s, .bottom)
                }
            } else if s.dock_edge == .top {
                row(
                    at: .top,
                    fill,
                    align: .center,
                    distribute: .center,
                    padding: if s.dock_fit == .floating { .related } else { .none },
                ) {
                    dock_ground(s, .top)
                }
            } else if s.dock_edge == .left {
                // A dock at a leading or trailing edge hugs its own width: the layer
                // sits at that edge (`at: .leading`), centred up it, and a `fill` here would
                // span the whole screen and centre the dock on it instead.
                stack(
                    at: .leading,
                    align: .center,
                    padding: if s.dock_fit == .floating { .related } else { .none },
                ) {
                    dock_ground(s, .left)
                }
            } else {
                stack(
                    at: .trailing,
                    align: .center,
                    padding: if s.dock_fit == .floating { .related } else { .none },
                ) {
                    dock_ground(s, .right)
                }
            }
        }
        // The hardware keys' answer stands over all of it, at the foot of the screen, for the
        // moment after the last key. The call carries the anchor: a view call is a layer of its
        // own, so where its foot sits is the caller's to say, not the view's root.
        osd_readout(at: .bottom)
        // A tablet pad's map, while the person holds the button that shows it: over everything
        // and in the way of nothing, so the pen goes on drawing where it was.
        pad_map_card()
        // The keys a held Super reaches, said over everything and in the way of nothing: a
        // picture, so the chord the person is reaching for still runs.
        if let p = device.overlays.peek {
            peek_card(p)
        }
        // Reporting a problem takes the whole screen: the windows stand as cards and the person
        // picks what the report is about, so the picker takes the pointer until it is answered.
        if s.reporting {
            report_picker()
        }
        // `show-strips` asks for the strips list, as the strip switcher does: it stands at the
        // shell's leading edge while it is asked for, and choosing a strip closes it.
        if device.overlays.strips_open {
            stack(at: .leading, align: .center, padding: .related) {
                strips_list()
            }
        }
        // What just happened, said over everything and gone in a moment: a close said with the
        // Undo that reopens it, or why a press did nothing. It stands over the shell rather than
        // in it, so what the person was looking at does not move under a passing word.
        if device.said.text != "" {
            row(at: .bottom, fill, align: .center, distribute: .center) {
                pass_notice()
            }
        }
        // Which screen is which, on each screen, for the moment after Settings was asked.
        identify_card()
        // A question a press on the shell's own surfaces asked, beside the control it pressed:
        // over the bar, the dock, quick settings and whatever surface stands over them, so the
        // person answers it where they are looking.
        asked_over_shell()
    }
}

/// The band under the bar: the style's own places, or the surface standing over them.
template view band(content: View) {
    let resting = device.overlays.here == ""
        or device.overlays.here == "none"
        or device.overlays.here == "switcher" and not device.overlays.switcher_revealed
    if device.overlays.here == "emoji" {
        // The picker stands over the windows: this layer draws nothing of the style, so
        // the windows stay live behind its card, which is the only thing that takes the
        // keys and the pointer while it is up.
        layers(fill) {
            emoji_card()
        }
    } else if device.overlays.here == "about" {
        // The About sheet stands over the windows, a question the person asked.
        about_sheet()
    } else if device.overlays.here == "power" {
        // The power menu hangs under the bar's trailing end, over the windows the person was
        // working in — and the question a row asks rises over it, as over any other surface.
        over_band {
            power_sheet()
        }
    } else if device.overlays.here == "peek" {
        // The keys a held Super reaches stand over the windows. The shell's own work
        // stays behind them, so nothing of the style is drawn here; the card itself
        // stands over everything in `shell_around`.
        banners_over(roomy: not (space.compact or space.width < 44.0))
    } else if device.overlays.here == "people" {
        // Who is here is a panel over the windows the person works in: the style's places
        // stay where they were under it, so their windows stay in sight, and the others'
        // pointers over them say where each person is while the roster lists them. The
        // panel stands in the band's own scroll, as every surface of the shell's does: a
        // press on the bar is the bar's, never one off the panel.
        over_band {
            layers(fill) {
                content
                scroll(fill) {
                    surface(fill)
                }
            }
        }
    } else {
        // The band's page: what rests on it, or the surface standing over it. One stage,
        // so a surface opens and closes with the shared motion rather than appearing and
        // vanishing at once. What the style shows keeps the whole band under the bar, and
        // the banner floats over it: a notice rising never moves what the person had on
        // screen.
        over_band {
            layers(fill, transition: .cross_fade) {
                if resting {
                    content
                } else if device.overlays.here == "switcher" {
                    surface(fill)
                } else if device.overlays.here == "quick-settings" {
                    // Beside a caret, the panel is drawn over the whole shell, at the
                    // point the stack settled (below the bar too). Left at its own place
                    // it keeps the band under the bar's trailing end.
                    if device.overlays.place == .none {
                        quick_settings(fill)
                    }
                } else if device.overlays.here == "centre" or device.overlays.here == "history" {
                    // The centre and the history are panels at an edge: the band's own
                    // height reaches them and their bodies scroll what does not fit. A
                    // scroll around a panel would grow it to its content instead, past the
                    // band it stands in.
                    surface(fill)
                } else {
                    // A surface of the shell's own over the places, as a page: the launcher
                    // and the workspaces scroll as a page does.
                    scroll(fill) {
                        surface(fill)
                    }
                }
            }
        }
    }
}

/// A dock up a side edge, in the column it takes beside the band: its body keeps the band it
/// rests in, so a magnified item grows over the page rather than moving it, and a floating dock
/// stands the theme's gap in from the screen's edge.
view side_dock(s: Shell, edge: Edge) {
    // Its own width, under the same rule as the dock on the foot's height: a row fills its
    // parent's width by default, so beside the band it would take half the display and leave
    // the page half its room. `fill: 0` keeps it at the dock's own band, and a full dock takes
    // the edge's width from `dock_ground` itself.
    // The room it keeps from the page is the room a dock on the foot keeps: one gap between the
    // two bands, its own inset counted once. `gap: .none` with the `tight` inset is that gap.
    row(
        fill: 0,
        gap: .none,
        align: .stretch,
        padding: if s.dock_fit == .floating { .tight } else { .none },
    ) {
        // The dock stands at the display's own middle, not at the middle of the band under the
        // bar: the bar's band stands under it in the same layer, so the pair is centred as one.
        layers(fill) {
            stack(at: .center) {
                dock_ground(s, edge)
                stack(height: device.room.y * 1dp) {
                }
            }
        }
    }
}

/// The passing notice: what happened, and the buttons that still act on it (Undo after a close),
/// in the floating material over the shell's foot. A press on a button is the session's own
/// notice action, so the shell, a script and an agent press the same one. Its glyph and colour say
/// what kind it is: an ordinary word, a success, a warning or a failure.
view pass_notice() {
    ui.Card {
        row(gap: .related, align: .center) {
            ui.Icon(
                if device.said.kind == "success" {
                    .circle_check
                } else if device.said.kind == "warning" {
                    .triangle_alert
                } else if device.said.kind == "error" { .circle_alert } else { .info },
                tone: if device.said.kind == "success" {
                    .success
                } else if device.said.kind == "warning" {
                    .warning
                } else if device.said.kind == "error" { .danger } else { .info },
            )
            ui.Text(device.said.text, role: .label)
            // A notice carries at most two buttons (Undo after a close, and one more): the same
            // cap the session keeps, written out so a style copy of the shell compiles the same.
            if let first = device.said.actions.first() {
                ui.Button(first.name, icon: first.icon, role: .plain) {
                    on press: device.press_notice(device.said, action: first.id)
                }
            }
            if let second = device.said.actions.at(1) {
                ui.Button(second.name, icon: second.icon, role: .plain) {
                    on press: device.press_notice(device.said, action: second.id)
                }
            }
            ui.Button("Dismiss", icon: .x, icon_only: true, role: .plain) {
                on press: device.hide(device.said)
            }
        }
    }
}

/// The room between the bar and the foot that the layout gets: how wide the shell's
/// bar lists actions decides whether each frame's own bar gives way to it (`bar_lists_actions`).
fn shell_lists(traditional: Bool, roomy: Bool) -> Bool {
    bar_lists_actions_for(shell, roomy, traditional)
}

/// The shell the person's style draws its places through, with `traditional` for the desktop
/// style's chrome and `body` the places: every style that shows the layout beside the
/// shell's bar and dock draws through this, so no style repeats the chrome.
template view style_shell(a: Layout, traditional: Bool, body: View) {
    shell_around(a, traditional, body)
}

/// Strips
///
/// The person's places as windows in rows and strips, scrolled sideways (`strips_view`).
view strips(a: Layout) {
    let lists = shell_lists(false, space.width_class == .expanded or space.width_class == .wide)
    layers(fill) {
        style_shell(a, false) {
            layers(fill) {
                strips_view(a, lists, fill)
                // Where the person is among the strips, each card at its own edge of the band.
                // The stack is placed here, not in the views: a view call is a boundary of its
                // own, and a stack placed inside one never reaches this layer — it stands at the
                // band's top.
                row(at: .leading, align: .center, padding: .tight) {
                    strip_before(a)
                }
                row(at: .trailing, align: .center, padding: .tight) {
                    strip_count(a)
                }
            }
        }
        // The floating windows stand on the display where the layout keeps them, over the rows.
        for s in floating_strips(a) {
            stack(
                at: .top_leading,
                x: s.float_x.to_float(),
                y: s.float_y.to_float(),
                min: s.float_width.to_float() * 1dp,
                max: s.float_width.to_float() * 1dp,
                height: s.float_height.to_float() * 1dp,
            ) {
                strip_window(a, s, lists, fill)
            }
        }
    }
}

/// Frames
///
/// The places at points on a pannable, zoomable canvas, each in its own frame.
view frames(a: Layout) {
    let lists = shell_lists(false, space.width_class == .expanded or space.width_class == .wide)
    style_shell(a, false) {
        canvas(a, "Canvas", true, lists, fill)
    }
}

/// Frameless
///
/// The places at points, each drawn as its own view shown as content.
view frameless(a: Layout) {
    style_shell(a, false) {
        canvas(a, "Frameless", false, false, fill)
    }
}

/// Desktop
///
/// Frames floating over the shell under the traditional bar, the focused window's commands in it,
/// on a roomy display; a compact output shows the window in front alone in the shell's own
/// chrome, as a phone does.
view desktop(a: Layout) {
    let lists = shell_lists(true, space.width_class == .expanded or space.width_class == .wide)
    if not space.compact {
        layers(fill) {
            traditional_desktop(a, lists, fill)
            // The shell under the frames, its bar the traditional one, which lists the window in
            // front's commands and offers no style switch of its own: the style the person
            // switches by is the bar's of every other style, and the desktop's own bar is this
            // style's.
            shell_around(.none, true) {
            }
        }
    } else {
        style_shell(a, true) {
            front_window(a, lists, fill)
        }
    }
}

/// The style the person shows, as the session found it (`Viewing.style`, a package and a
/// view): the strips when their document names none, so a shell that has never picked one opens.
fn style_of(v: Viewing) -> Text {
    if v.style != "" { v.style } else { "so.tilo.shell:strips" }
}

/// The dock's ground on `edge`: the theme's material for the style the person picked, edge to
/// edge along a top or a foot and inset with rounded corners on a floating dock, which `edge`
/// as `ui.Chrome` names it. What stands on it is the dock's own run (`dock`).
view dock_ground(s: Shell, edge: Edge) {
    if edge == .top or edge == .bottom {
        ui.Chrome(
            name: "Dock",
            shape: if s.dock_fit == .full { .square } else { .rounded },
            edge: if edge == .top { .top } else { .bottom },
            material: if s.dock_finish == .translucent { .glass } else { .calm },
            reserve: true,
            fill: if s.dock_fit == .full { 1 } else { 0 },
        ) {
            dock(s)
        }
    } else {
        ui.Chrome(
            name: "Dock",
            shape: if s.dock_fit == .full { .square } else { .rounded },
            edge: if edge == .left { .leading } else { .trailing },
            material: if s.dock_finish == .translucent { .glass } else { .calm },
            reserve: true,
        ) {
            dock(s)
        }
    }
}

/// The top bar on the edge it rides: the whole width with its parts spread along it at the `full`
/// fit, or an inset run centred on the edge at the floating one, where it covers less of what is
/// under it. Its ground is the theme's material for the bar's style either way; how much of the
/// output it spans is what its fit says. `traditional` draws the desktop style's bar, which lists
/// the window in front's commands (`top_bar`).
view bar_on_edge(s: Shell, traditional: Bool) {
    let on_top = s.bar_edge == .top
    // The stack holds the bar at its own height: the bar fills the shell's width, and takes no
    // share of its height, so the shell's windows get the room the bar leaves.
    stack {
        if s.bar_fit == .full and on_top {
            row(at: .top, fill, align: .center) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .square,
                    edge: .top,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                    fill,
                ) {
                    top_bar(traditional)
                }
            }
        } else if s.bar_fit == .full {
            row(at: .bottom, fill, align: .center) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .square,
                    edge: .bottom,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                    fill,
                ) {
                    top_bar(traditional)
                }
            }
        } else if on_top {
            row(at: .top, align: .center, padding: .related) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .rounded,
                    edge: .top,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                ) {
                    top_bar(traditional)
                }
            }
        } else {
            row(at: .bottom, align: .center, padding: .related) {
                ui.Chrome(
                    name: "Top bar",
                    shape: .rounded,
                    edge: .bottom,
                    material: if s.bar_finish == .translucent { .glass } else { .calm },
                ) {
                    top_bar(traditional)
                }
            }
        }
    }
}

/// The bar's parts in the person's order, as the run the person reorders by dragging a part:
/// what the shell's bar and a window's own bar both draw. The traditional desktop shows its
/// own bar instead (`traditional.tl`), where the focused window's commands stand beside the
/// parts the person chose, so a person picks between the two by picking the style.
///
/// `traditional` is the desktop style's chrome: its bar lists the focused window's commands
/// whatever the chrome's own preset, so `top_bar` draws `traditional_bar`.
view top_bar(traditional: Bool) {
    // A display that is not the one in use carries the compact bar, whichever style the person
    // picked: its traditional bar lists the focused window's commands, which are the commands of
    // the window the person is working in, on the display they are working on.
    if (traditional or bar_is_traditional(shell)) and on_primary() {
        traditional_bar()
    } else {
        bar()
    }
}

/// What the shell shows in its band, with the banners a notice rose as over it: the style's own
/// places, or the surface standing over them. A notice reaches a person wherever they are, so a
/// surface does not hold one back.
template view over_band(body: View) {
    layers(fill) {
        body
        // While the person follows someone, their colour runs round the room the windows stand
        // in, over the windows: whose view this is, all the way round.
        ui.FollowFrame(followed_colour(), fill)
        // What is live and stands as a card, over the windows and under a banner rising.
        // The band is the display's width, which the cards' own view cannot see: a phone takes
        // one card across under the bar.
        live_cards(phone: space.compact, fill)
        // Who is here, kept on screen at an edge of the band, where the person works: never on a
        // phone, where it would stand over the one window there is.
        if not space.compact {
            kept_people()
        }
        banners_over(roomy: not (space.compact or space.width < 44.0))
    }
}

/// The banners a notice rose as, floated over the band: the style's own places at rest, or the
/// surface standing over them (the launcher, the switcher, quick settings, the centre). The emoji
/// picker and the About sheet keep the screen: they are the answer to something the person just
/// asked, and stand over this in `shell_around`.
///
/// A person with windows sees them through the compositor behind this, and who is here is in the
/// bar's own menu, so this adds nothing to the bar's band: with nothing to show it lays nothing
/// out and draws nothing. A banner is the display in use's: it rises in the corner of the screen
/// the person works on, a banner's width in from the edge the transient panels keep, and another
/// display shows none of it. A display too narrow for the corner (a phone) takes the card across
/// it, a phone's gutter in from either edge: `roomy` says the display keeps the corner, read
/// from the room the caller has, never this view's own.
view banners_over(roomy: Bool) {
    // A question that waits rises as its card whether or not a notice stands beside it.
    let rising = (device.notifications.banner ?? -1) != -1 or not asked_elsewhere().is_empty()
    if rising and on_primary() {
        if not roomy {
            stack(at: .top, fill, padding: .sibling) {
                banner(roomy: false)
            }
        } else {
            stack(at: .top_trailing, padding: .related) {
                banner(roomy: true)
            }
        }
    }
    // A question put to the person about themselves, open, stands as its card under their own
    // avatar: what it asks, what answering brings and the two ways to put it off. Folded, it is
    // the dot on their face instead, and their card is where the question waits.
    if device.overlays.ask != "" and device.overlays.ask_open {
        stack(at: .top_trailing, padding: .group) {
            // A display too narrow for a card at a banner's width takes it across, as the
            // questions that rise with the banners do, so it never stands off the display.
            ui.FloatingPanel(width: if not space.compact { 360.0 }, fill: space.compact) {
                ask_card()
            }
        }
    }
}

/// The surface the person is in, at the place its kind shows: the launcher fills the room
/// under the bar, the overview and the workspaces fill it, and the centre sits at the
/// trailing edge. Every one of them stands the shell's gutter in from the
/// window's edge, where that window is narrow enough to have no page gutter of its own: a
/// surface is a panel over the windows, not a band of the window.
view surface() {
    // Who is here places itself: a panel at the bar's end on a desktop, a sheet at a phone's foot.
    // It stands in the shell's own room rather than in this padding, so its layer runs to the
    // edges the person reaches for.
    if device.overlays.here == "people" {
        people(fill)
    } else {
        stack(fill, padding: if space.compact { .section } else { .none }) {
            if device.overlays.here == "overview" {
                overview(fill)
            } else if device.overlays.here == "switcher" {
                switcher(fill)
            } else if device.overlays.here == "launcher" {
                launcher(fill)
            } else if device.overlays.here == "centre" {
                notices(fill)
            } else if device.overlays.here == "quick-settings" {
                quick_settings(fill)
            } else if device.overlays.here == "workspaces" {
                workspaces(fill)
            } else if device.overlays.here == "history" {
                history(fill)
            }
        }
    }
}
