/// The hardware keys' readout, as the session publishes it (`device.osd`): the level a volume,
/// brightness or backlight key just set, or the state an airplane, night-light or transport key
/// just switched, named on the display whose key changed it and gone a moment after the last
/// press.
///
/// It stands at the foot of the screen, centred, over whatever the shell shows there, and takes
/// neither the pointer nor the keyboard: a person reads it and goes on. Nothing while no key has
/// just answered.
view osd_readout() {
    let o = device.osd
    if o.visible {
        row(at: .bottom, fill, align: .center, distribute: .center, padding: .related) {
            if let level = o.level {
                let glyph = o.glyph ?? ""
                ui.Card(title: osd_name(o)) {
                    row(gap: .related, align: .center) {
                        ui.Icon(
                            if glyph == "headphones" {
                                .headphones
                            } else if glyph == "display" {
                                .monitor
                            } else if glyph == "keyboard" {
                                .keyboard
                            } else if o.kind == "brightness" { .sun } else { .volume_2 },
                            size: .regular,
                        )
                        ui.BarGauge(value: level, readout: osd_value(o, level), compact: true, fill)
                    }
                }
            } else {
                ui.Card(title: osd_words(o))
            }
        }
    }
}

/// What the readout is about: the device a level names, else the kind's own word.
fn osd_name(o: device.Osd) -> Text {
    if let named = o.name {
        return named
    }
    if o.kind == "volume" {
        "Volume"
    } else if o.kind == "brightness" {
        "Brightness"
    } else if o.kind == "keyboard-backlight" { "Keyboard" } else { osd_words(o) }
}

/// The level as the person reads it: a percentage, "Muted" when the sound is, and "Off" when a
/// backlight is.
fn osd_value(o: device.Osd, level: Float) -> Text {
    if not o.on {
        if o.kind == "volume" { "Muted" } else { "Off" }
    } else if level <= 0.0 and o.kind == "keyboard-backlight" {
        "Off"
    } else {
        let percent = (level * 100.0 + 0.5).to_int()
        "{percent}%"
    }
}

/// A readout said in words: the state the key switched, or what it did.
fn osd_words(o: device.Osd) -> Text {
    if o.kind == "volume" {
        "Volume"
    } else if o.kind == "brightness" {
        "Brightness"
    } else if o.kind == "keyboard-backlight" {
        "Keyboard backlight"
    } else if o.kind == "microphone" {
        if o.on { "Microphone on" } else { "Microphone muted" }
    } else if o.kind == "airplane-mode" {
        if o.on { "Airplane mode on" } else { "Airplane mode off" }
    } else if o.kind == "touchpad" {
        if o.on { "Touchpad on" } else { "Touchpad off" }
    } else if o.kind == "play" {
        if let named = o.name { "Playing {named}" } else { "Play" }
    } else if o.kind == "pause" {
        if let named = o.name { "Paused {named}" } else { "Pause" }
    } else if o.kind == "next-track" {
        "Next track"
    } else if o.kind == "previous-track" {
        "Previous track"
    } else if o.kind == "stop" {
        "Stop"
    } else if o.kind == "no-backlight" {
        "No adjustable display"
    } else if o.kind == "night-light" {
        if o.on { "Night light on" } else { "Night light off" }
    } else if o.kind == "do-not-disturb" {
        if o.on { "Do not disturb on" } else { "Do not disturb off" }
    } else if o.kind == "magnifier" {
        if o.on { "Magnifier on" } else { "Magnifier off" }
    } else if o.kind == "input-source" {
        if let named = o.name { named } else { "Input source" }
    } else if o.kind == "captured-keys" {
        "Keys captured"
    } else if o.kind == "pointer-released" { "Pointer released" } else { "Changed" }
}
