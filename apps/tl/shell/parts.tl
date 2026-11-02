/// The keys a surface shows under it, as `ui.KeyHints`: the drawn keycaps of each chord and
/// what pressing them does, side by side, in the person's own keyboard legends. The shell never
/// writes a chord as a string, so a German keyboard's legends and a Mac's Command key read
/// right.
/// The keys of the launcher: typing searches, the arrows walk, Enter opens, Esc closes.
view key_hints_launcher() {
    ui.KeyHints {
        ui.KeyHint(["Up", "Down"], "Choose")
        ui.KeyHint(["Enter"], "Open")
        ui.KeyHint(["Esc"], "Close")
    }
}

/// The keys of the switcher: hold the modifier, Tab walks, letting go goes there. The walk that
/// keeps to one app is the modifier and the grave key, so that is the one named while it does.
view key_hints_switcher(this_app: Text) {
    ui.KeyHints {
        ui.KeyHint(if this_app != "" { ["Alt", "`"] } else { ["Alt", "Tab"] }, "Walk")
        ui.KeyHint(["Enter"], "Go there")
        ui.KeyHint(["Esc"], "Stay")
    }
}
