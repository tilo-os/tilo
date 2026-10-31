/// The frame: what the shell draws around a place (shell.md §4).
///
/// A frame is a column of tabs over the content the place shows. The tabs are the window
/// bar's own strip (`ui.TabStrip`), so a frame in the shell and a window bar in the native
/// session look and behave alike: each pill as wide as its title, the one in front lifted off
/// the strip's ground, who is at a resource on its pill, and the close chip where the person
/// expects it. The strip squeezes its pills to fit, so a frame of twelve tabs on a narrow one
/// still shows every title it can.
///
/// Everything the frame says comes from the resource and its type: its title is the resource's
/// title field, its glyph the type's icon, its faces the resource's lenses, and its commands
/// the resource's own actions (never an app's). A frame of one place says nothing twice: its
/// title is its content's header, and no strip stands over it.
/// A frame of places in one column: the tab strip, the frame's commands and the place in front.
///
/// `place` is the resource of the frame's front place, which the view holding the frame reads
/// from the arrangement; `content` is that place's content, shown under the frame's head.
view frame<R>(f: Frame, place: R, content: View) {
    stack {
        frame_head(f, place)
        content
    }
}

/// A frame's head: its tabs, and the frame's own controls.
///
/// A frame of one place shortens to that place's title, without a strip: the second tab is
/// what makes a frame a frame of tabs. A frame too narrow for its tabs and controls on one row
/// wraps them, so neither is pressed into the other.
view frame_head<R>(f: Frame, place: R) {
    stack(gap: .related) {
        if f.places.len() > 1 {
            row(gap: .related, align: .center, wrap: true) {
                frame_tabs(f, place)
                frame_controls(f, place)
            }
        } else {
            row(gap: .related, align: .center, wrap: true) {
                frame_title(place, focused: f.focused)
                frame_controls(f, place)
            }
        }
    }
}

/// The title a frame of one place shows: the resource's glyph and title, as the frame's own.
view frame_title<R>(r: R, focused: Bool) {
    row(gap: .related, align: .center) {
        frame_glyph(r, focused: focused)
        ui.Text(meta(r).title, role: .subheading, secondary: not focused)
    }
}

/// The resource's glyph, from its type: the frame's mark for what it shows.
///
/// A resource whose type names no icon says its title alone; the frame never draws a glyph of
/// its own.
view frame_glyph<R>(r: R, focused: Bool) {
    ui.Icon(meta(r).icon, secondary: not focused)
}

/// The frame's tab strip: one pill per place, the one in front lifted, each saying who is at
/// its resource now.
///
/// Choosing a pill brings its place to the front; its close chip closes the place; a pill
/// dropped at a new place in the strip reorders it; one let go outside the strip hands its
/// place to the frame there. Each carries the place's resource and where it landed; the shell
/// finds the place of that resource in the frame, which is how a tab names its place.
view frame_tabs<R>(f: Frame, place: R) {
    ui.TabStrip(active: place, focused: f.focused) {
        for p in f.places {
            frame_tab(p)
        }
        on activate: choose_tab(f, place)
        on close: close_tab(f, place)
        on reorder: move_tab(f, place, after: place)
        on detach: detach_tab(f, place, onto: f)
    }
}

/// Chooses a tab: the tab of `shows` comes to the front of its frame.
///
/// The strip reports the resource the pill stands for; the frame's place of that resource is
/// the tab chosen, which `bring_to_front` then makes the front.
action choose_tab(f: Frame, shows: host.Resource) {
    if let place = f.places.find(x -> x.shows == shows) { bring_to_front(f, place) }
}

/// Closes the tab of `shows`: the place leaves the frame's strip.
action close_tab(f: Frame, shows: host.Resource) {
    if let place = f.places.find(x -> x.shows == shows) { close_place(f, place, next: place) }
}

/// Moves the tab of `shows` to the end of its frame's strip, or to the place another tab was
/// let go: the strip reports where, the shell finds the place.
action move_tab(f: Frame, shows: host.Resource, after: host.Resource) {
    let place = f.places.find(x -> x.shows == shows)
    if place == .none {
        return
    }
    let before = f.places.find(x -> x.shows == after)
    if let place = place, before = before {
        move_place(f, place, after: before, to_end: false)
    } else if let place = place {
        move_place(f, place, after: place, to_end: true)
    }
}

/// Hands the tab of `shows` to the frame it was let go on (`onto`), in front there.
action detach_tab(f: Frame, shows: host.Resource, onto: Frame) {
    if let place = f.places.find(x -> x.shows == shows) {
        move_place_to(f, place, onto: onto, next: place)
    }
}

/// One tab of a frame: the place's resource as a pill, with who is at it.
view frame_tab(p: Place) {
    ui.TabItem(p.shows, title: meta(p.shows).title, icon: meta(p.shows).icon) {
        for l in lenses(p.shows) {
            if not l.mine {
                ui.TabFace(l.who)
            }
        }
    }
}

/// The frame's own controls: the front place's actions as a menu bar, and what the arrangement
/// offers the frame.
view frame_controls<R>(f: Frame, place: R) {
    row(gap: .tight, align: .center) {
        frame_commands(place)
        frame_buttons(f)
    }
}

/// The resource's actions, as a menu bar: its primary action as a button, and every action in
/// the one More menu, so nothing is lost however narrow the frame is.
view frame_commands<R>(r: R) {
    let offered = actions(r)
    for a in offered {
        if a.primary {
            ui.Button(a.label, icon: a.icon, role: .primary) {
                on press: a.run
            }
        }
    }
    if not offered.is_empty() {
        ui.MenuButton("More", icon: .more_horizontal, icon_only: true) {
            for a in offered {
                ui.MenuItem(a.label, icon: a.icon, destructive: a.destructive) {
                    on choose: a.run
                }
            }
        }
    }
}

/// A frame's buttons: what the arrangement offers it, and close.
///
/// A maximized frame offers to come back to its strip; one in a strip offers to widen, narrow
/// and maximize. They stand in a toolbar, so a frame too narrow for them all keeps every one
/// reachable in its More menu, and each keeps its touch target however narrow the frame is.
view frame_buttons(f: Frame) {
    ui.Toolbar {
        if f.maximized {
            ui.ToolbarButton("Restore", icon: .minimize_2) {
                on press: restore(f)
            }
        } else {
            ui.ToolbarButton("Narrower", icon: .chevron_left) {
                on press: narrow(f)
            }
            ui.ToolbarButton("Wider", icon: .chevron_right) {
                on press: widen(f)
            }
            ui.ToolbarButton("Maximize", icon: .maximize_2) {
                on press: maximize(f)
            }
        }
        ui.ToolbarButton("Close the frame", icon: .x) {
            on press: close_frame(f)
        }
    }
}
