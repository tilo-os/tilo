/// The device's controls the Quick Settings panel shows: sound, brightness, the network,
/// Wi-Fi, Bluetooth, the power, the theme and do-not-disturb. The session writes what the
/// device says; a change is an action of the shell's, so the person, a shortcut and an agent
/// change them the same way.
type Control (origin: session.control) {
    key: Text merge register
    label: Text merge register
    icon: Text merge register
    on: Bool
    value: Int
    detail: Text merge register
    available: Bool
    has_level: Bool
}

/// One menu of the focused app, as the bar shows it after the system's.
type AppMenu {
    title: Text (title)
    entries: List<Text>
}

/// Turns a control on or off, or sets its level: what a tile's press does, and what an agent
/// or a shortcut does for the person.
action toggle_control(s: Shell, control: Control) (icon: .settings_2, label: "Change") {
    outside("session.set", control.key)
}

/// Sets a control's level, 0 to 100: a slider's commit.
action set_level(s: Shell, control: Control, level: Int) (icon: .settings_2, label: "Set the level") {
    outside("session.set", control.key)
}
