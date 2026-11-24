/// The shell's control hints over everything it shows: how to open anything from search, move a
/// window by its bar, resize one by its edge and move the view, each taught once beside where it is
/// done and gone once the person does it (`ui.ControlHints`). Every style draws the same hints over
/// its own windows.
///
/// What the person has learned is their own record, the one every device of theirs reads
/// (`settings.control_hints`): the hints read it and write each change back through its actions,
/// so a hint learned on one device never shows on another, and Settings and the profile card's
/// switch turn the same record on and off.
///
/// No hint begins while something else has the person's attention (a surface over the windows,
/// the style's settings), while they type, and none while they follow someone, whose view is not
/// their own.
view control_hints() {
    let here = device.overlays.here
    let learned = settings.control_hints.learned
    ui.ControlHints(
        shows: learned.enabled,
        known: learned.known,
        shown: learned.shown,
        search_keys: ["Super", "Space"],
        touch: space.compact,
        searching: here == "launcher",
        busy: here != "" and here != "none" and here != "launcher"
            or shell.settings_open
            or shell.reporting,
        engaged: device.people.following != "",
        typing: device.input.typing,
        fill,
    ) {
        // Search opens from Super and Space, the chord the person's shortcuts start with.
        // A phone's output is pointed at with a finger.
        on learn(hint: Text): settings.learn_control_hint(settings.control_hints, hint: hint)
        on show(hint: Text): settings.note_control_hint_shown(settings.control_hints, hint: hint)
        on withdraw(hint: Text): settings.withdraw_control_hint_shown(
            settings.control_hints,
            hint: hint,
        )
    }
}
