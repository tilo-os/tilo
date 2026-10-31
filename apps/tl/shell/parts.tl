/// The keys a surface shows under it, as `ui.KeyHints`: the drawn keycaps of each chord and
/// what pressing them does, in the person's own keyboard legends. The shell never writes a
/// chord as a string, so a German keyboard's legends and a Mac's Command key read right.

/// The keys of the launcher: typing searches, the arrows walk, Enter opens, Esc closes.
view key_hints_launcher() {
    stack(gap: .tight) {
        ui.InputHint(["Up", "Down"])
        ui.Text("Choose", role: .caption, secondary: true)
        ui.InputHint(["Enter"])
        ui.Text("Open", role: .caption, secondary: true)
        ui.InputHint(["Esc"])
        ui.Text("Close", role: .caption, secondary: true)
    }
}

/// The keys of the switcher: hold the modifier, Tab walks, letting go goes there.
view key_hints_switcher() {
    stack(gap: .tight) {
        ui.InputHint(["Alt", "Tab"])
        ui.Text("Walk", role: .caption, secondary: true)
        ui.InputHint(["Enter"])
        ui.Text("Go there", role: .caption, secondary: true)
        ui.InputHint(["Esc"])
        ui.Text("Stay", role: .caption, secondary: true)
    }
}
