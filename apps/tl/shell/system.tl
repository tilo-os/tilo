/// The device's controls the Quick Settings panel shows: sound, brightness, the network,
/// Wi-Fi, Bluetooth, the power, the theme and do-not-disturb. The session writes what the
/// device says; a change is the setting's own action, which the session publishes, so the
/// person, a shortcut and an agent change them the same way.
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
