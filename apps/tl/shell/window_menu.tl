/// The window menu the shortcut and the `desktop:window-menu` "open" action put up: the rows the
/// session publishes for the window in front, drawn where the session says the menu stands, each
/// choosing through the menu's own verb. The view keeps no menu of its own — what is up, for which
/// window and what it offers are the reading — so the menu a person opens with Super+F10, the one
/// a script opens and the one a window's bar offers are one menu.
///
/// The rows read in runs of what they do, a line between one run and the next. The rows of a
/// submenu (the shares of its strip) stand under the row that names them, so every row the reading
/// offers is drawn, reachable with the keyboard, and chosen the same way. A row the window cannot
/// choose says why.
view window_menu(m: desktop.WindowMenu) {
    if m.tab != .none and not m.rows.is_empty() {
        layers(fill) {
            if let a = m.anchor {
                // The point the session settled, in the layer's own coordinates: the press that
                // opened the menu, or the window's corner when the verb named no place. A menu at
                // an edge moves in from it so its rows stay on the screen.
                let x = a.x.min((space.width - 308.0).max(8.0)).max(8.0)
                let y = a.y.min((space.height - 120.0).max(8.0)).max(8.0)
                stack(at: .top_leading, x: x, y: y) {
                    window_menu_card(m)
                }
            } else {
                stack(at: .top_leading, padding: .related) {
                    window_menu_card(m)
                }
            }
        }
    }
}

/// The menu's card: its rows in order, under the floating material, as the window's bar menu shows
/// them.
view window_menu_card(m: desktop.WindowMenu) {
    ui.FloatingPanel(width: 300.0) {
        body: {
            stack(gap: .tight) {
                for row in m.rows {
                    if row.separator {
                        divider()
                    } else if not row.children.is_empty() {
                        ui.Text(row.label, role: .caption, secondary: true)
                        for child in row.children {
                            window_menu_row(m, child)
                        }
                    } else {
                        window_menu_row(m, row)
                    }
                }
            }
        }
    }
}

/// One row of the menu: what it does, and the reason it cannot when it cannot. Choosing it sends
/// the row's own verb, which closes the menu where the session holds it.
view window_menu_row(m: desktop.WindowMenu, row: desktop.Row) {
    ui.Button(
        row.label,
        icon: row.icon,
        role: .plain,
        enabled: row.enabled,
        description: row.reason,
    ) {
        on press: desktop.choose(m, row: row.id)
    }
}
