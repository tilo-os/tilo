/// The person's speakers: sets that play across their devices, each set up here, placed in the
/// room and measured against where the person sits.
///
/// Everything shown is `device.speakers`, the session's reading: the devices with the outputs and
/// microphones they offer, the sets they form, and any measurement under way. Every press is one
/// of that resource's published actions, so a set formed here is the set the shell, a script and
/// an agent see and change, and nothing here keeps a copy of one.
view speakers_page() {
    // The setup flow as the person walks it: the step, the outputs they picked, the role or place
    // each one takes, and the set a measurement belongs to. This is the window's own
    // presentation, so it is view state and belongs to the person looking, never a resource.
    state step: Text = "overview"
    state placement: Text = "stereo"
    state chosen: List<Text> = []
    state roles: List<Text> = []
    state xs: List<Int> = []
    state ys: List<Int> = []
    state name: Text = ""
    state at: Int = 0
    state played: Bool = false
    state group: Text = ""
    state listening: Text = ""
    state mic_on: List<Text> = []
    state mic_off: List<Text> = []
    state selected: Int = 0
    state confirming: Text = ""
    let sp = device.speakers
    if not sp.media_enabled {
        section("Speakers across your devices") {
            text(
                "Turn on Media in Sync between devices to play through speakers on several of your devices.",
                role: .caption,
                secondary: true,
            )
        }
    } else if step == "overview" {
        speakers_overview(
            sp,
            step,
            group,
            listening,
            mic_on,
            mic_off,
            confirming,
            placement,
            chosen,
            roles,
            xs,
            ys,
            name,
            at,
            played,
        )
    } else if step == "choose" {
        speakers_choose(sp, placement, chosen, roles, xs, ys, step, at, played)
    } else if step == "identify" {
        speakers_identify(placement, chosen, roles, at, played, step)
    } else if step == "layout" {
        speakers_layout(sp, chosen, xs, ys, selected, at, step)
    } else if step == "name" {
        speakers_name(sp, chosen, roles, name, xs, ys, placement, step)
    } else if step == "calibrate" {
        speakers_calibrate(sp, group, listening, mic_on, mic_off, step)
    }
}

/// Every set the person has, the way to make one, and the question a removal waits on.
template view speakers_overview(
    sp: device.Speakers,
    step: State<Text>,
    group: State<Text>,
    listening: State<Text>,
    mic_on: State<List<Text>>,
    mic_off: State<List<Text>>,
    confirming: State<Text>,
    placement: State<Text>,
    chosen: State<List<Text>>,
    roles: State<List<Text>>,
    xs: State<List<Int>>,
    ys: State<List<Int>>,
    name: State<Text>,
    at: State<Int>,
    played: State<Bool>,
) {
    section("Speakers across your devices") {
        if sp.groups.is_empty() {
            text(
                "Use speakers on several of your devices together: a stereo pair, surround, or speakers around the room.",
                role: .caption,
                secondary: true,
            )
        }
        for g in sp.groups {
            ui.ListRow(
                g.name,
                secondary: if g.led_here {
                    member_summary(g)
                } else {
                    "{member_summary(g)}, set up on {g.leader_label}"
                },
                icon: .speaker,
                badge: "{ready_text(g)}",
                tone: if ready_here(g) == g.members.len() { .success } else { .warning },
                value: if let playing = g.playback { playback_text(playing) } else { "" },
            ) {
                trailing: {
                    row(gap: .related, align: .center) {
                        ui.Button(
                            "Use {g.name} as this device's output",
                            icon: .speaker,
                            size: .small,
                            toggled: g.output_here,
                        ) {
                            on press: if g.output_here {
                                device.stop_using_as_output(device.speakers)
                            } else {
                                device.use_as_output(device.speakers, group: g.id)
                            }
                        }
                        if g.led_here {
                            ui.Button("Calibrate", size: .small) {
                                on press: set(group, g.id)
                                    .then(set(listening, ""))
                                    .then(set(mic_on, []))
                                    .then(set(mic_off, []))
                                    .then(
                                        set(step, "calibrate"),
                                    )
                            }
                            ui.MenuButton(
                                "More for {g.name}",
                                icon: .more_horizontal,
                                icon_only: true,
                            ) {
                                ui.MenuItem("Delete", icon: .trash) {
                                    on choose: set(confirming, g.id)
                                }
                            }
                        }
                    }
                }
            }
        }
        row(fill) {
            ui.Button("Set up speakers", icon: .plus, role: .primary) {
                on press: set(step, "choose")
                    .then(set(placement, "stereo"))
                    .then(set(chosen, []))
                    .then(set(roles, []))
                    .then(set(xs, []))
                    .then(set(ys, []))
                    .then(set(name, ""))
                    .then(set(at, 0))
                    .then(set(played, false))
            }
        }
    }
    if let doomed = sp.groups.find(g -> g.id == confirming) {
        ui.ModalDialog(
            title: "Remove {doomed.name}?",
            description: "The speakers stay paired with this device; the set they form goes.",
            open: true,
            primary: "Remove",
            destructive: true,
        ) {
            on act_primary: device
                .delete_group(device.speakers, group: doomed.id)
                .then(set(confirming, ""))
            on dismiss: set(confirming, "")
        }
    }
}

/// Where the speakers stand and which outputs join the set. Changing either clears the roles
/// assigned so far: they were answers about outputs picked before.
template view speakers_choose(
    sp: device.Speakers,
    placement: State<Text>,
    chosen: State<List<Text>>,
    roles: State<List<Text>>,
    xs: State<List<Int>>,
    ys: State<List<Int>>,
    step: State<Text>,
    at: State<Int>,
    played: State<Bool>,
) {
    section("Choose speakers") {
        ui.ToggleGroup(caption: "Placement", fill: true) {
            for each in ["stereo", "surround", "room"] {
                ui.Segment(placement_title(each), chosen: placement == each) {
                    on press: set(placement, each).then(set(roles, []))
                }
            }
        }
        text(placement_description(placement), role: .caption, secondary: true)
        if sp.devices.all(d -> not d.online or not d.can_join or d.outputs.is_empty()) {
            text(
                "No output of your devices can join a set yet: turn on Media in Sync for the devices whose speakers you want, and check they are online.",
                role: .caption,
                secondary: true,
            )
        }
        for d in sp.devices {
            if d.online and d.can_join {
                for o in d.outputs {
                    let key = speaker_key(d.id, o.selector)
                    ui.ListRow(
                        "{d.label} ({o.label})",
                        secondary: if d.local {
                            "This device"
                        } else {
                            "Asks for approval on that device when you save"
                        },
                        icon: .speaker,
                        selected: chosen.contains(key),
                    ) {
                        trailing: {
                            ui.Button(
                                "Use {d.label} ({o.label})",
                                icon: .check,
                                icon_only: true,
                                toggled: chosen.contains(key),
                                role: .plain,
                            ) {
                                on press: set(chosen, toggled(chosen, key)).then(set(roles, []))
                            }
                        }
                    }
                }
            }
        }
        let problem = placement_problem(placement, chosen.len())
        if problem != "" {
            text(problem, role: .caption)
        }
        row(fill, distribute: .end) {
            ui.Button("Back") {
                on press: set(step, "overview")
            }
            ui.Button("Continue", role: .primary, enabled: problem == "", description: problem) {
                on press: set(roles, outputs_roles(placement, chosen))
                    .then(set(xs, outputs_xs(placement, chosen, xs)))
                    .then(set(ys, outputs_ys(placement, chosen, ys)))
                    .then(set(step, "identify"))
                    .then(set(at, 0))
                    .then(set(played, false))
            }
        }
    }
}

/// Play a sound on one output and answer which speaker it was, so the set knows where each one
/// stands. Around the room this step only plays each speaker in turn; the places come next.
template view speakers_identify(
    placement: State<Text>,
    chosen: State<List<Text>>,
    roles: State<List<Text>>,
    at: State<Int>,
    played: State<Bool>,
    step: State<Text>,
) {
    section("Which one just played?") {
        text(
            "Speaker {at + 1} of {chosen.len()}. Play a sound and listen for which speaker it comes from.",
            role: .caption,
            secondary: true,
        )
        if let key = chosen.at(at) {
            ui.Button("Play sound", icon: .volume_2) {
                on press: device
                    .identify(
                        device.speakers,
                        device: speaker_device(key),
                        output: speaker_output(key),
                    )
                    .then(set(played, true))
            }
        }
        if placement == "room" {
            text(
                "You will place it on the room layout next; each speaker starts on an arc in front of you.",
                role: .caption,
                secondary: true,
            )
        } else {
            text(if played { "Which one just played?" } else { "Play the sound first." })
            row(gap: .related, wrap: true) {
                for each in free_roles(placement, roles, at) {
                    ui.Button(
                        role_answer(each),
                        enabled: played,
                        description: "Play a sound first.",
                    ) {
                        on press: set(roles, answered_roles(each, at, roles, placement))
                            .then(
                                if let next = answered_roles(each, at, roles, placement)
                                        .index_of("") {
                                    set(at, next)
                                } else { set(step, "name") },
                            )
                    }
                }
            }
        }
        row(fill, distribute: .end) {
            ui.Button("Back") {
                on press: if at == 0 {
                    set(step, "choose").then(set(played, false))
                } else { set(at, at - 1).then(set(played, false)) }
            }
            if placement == "room" {
                ui.Button("Next speaker", role: .primary) {
                    on press: if at + 1 < chosen.len() {
                        set(at, at + 1)
                    } else { set(step, "layout") }
                }
            }
        }
    }
}

/// Where each speaker stands: dragged to its place on a plan of the room, snapping to the centre
/// line and to the lines the other speakers stand on.
template view speakers_layout(
    sp: device.Speakers,
    chosen: State<List<Text>>,
    xs: State<List<Int>>,
    ys: State<List<Int>>,
    selected: State<Int>,
    at: State<Int>,
    step: State<Text>,
) {
    section("Room layout") {
        text(
            "Drag each speaker to where it stands. You sit at the marker.",
            role: .caption,
            secondary: true,
        )
        ui.Drawing(
            room_shapes(sp, chosen, xs, ys, selected),
            meaning: .picture,
            label: room_label(sp, chosen, xs, ys),
            height: .some(320.0),
        ) {
            on press(at: Int): set(selected, at)
            on drag_at(at: List<Float>): set(xs, placed(xs, selected, at, true))
                .then(set(ys, placed(ys, selected, at, false)))
        }
        if let picked = chosen.at(selected) {
            ui.ListRow(
                "Position",
                secondary: position_text(xs.at(selected) ?? 0, ys.at(selected) ?? 0),
                icon: .map_pin,
            ) {
                trailing: {
                    ui.Button(
                        "Play a sound on {speaker_name(sp, chosen, selected)}",
                        icon: .volume_2,
                    ) {
                        on press: device.identify(
                            device.speakers,
                            device: speaker_device(picked),
                            output: speaker_output(picked),
                        )
                    }
                }
            }
        }
        let problem = crowded(chosen, xs, ys)
        if problem != "" {
            text(problem, role: .caption)
        }
        row(fill, distribute: .end) {
            ui.Button("Back") {
                on press: set(at, (chosen.len() - 1).max(0)).then(set(step, "identify"))
            }
            ui.Button("Continue", role: .primary, enabled: problem == "", description: problem) {
                on press: set(step, "name")
            }
        }
    }
}

/// What the set is called, and the role or place each speaker took.
template view speakers_name(
    sp: device.Speakers,
    chosen: State<List<Text>>,
    roles: State<List<Text>>,
    name: State<Text>,
    xs: State<List<Int>>,
    ys: State<List<Int>>,
    placement: State<Text>,
    step: State<Text>,
) {
    section("Name your speakers") {
        ui.TextInput(bind: name, placeholder: "Living room")
        text("Shown wherever you can play to these speakers.", role: .caption, secondary: true)
        for each in chosen {
            ui.ListRow(
                speaker_name(sp, chosen, chosen.index_of(each) ?? 0),
                secondary: role_text(roles.at(chosen.index_of(each) ?? 0) ?? ""),
                icon: .speaker,
            )
        }
        text(
            "The saved name identifies this speaker or room when joining it.",
            role: .caption,
            secondary: true,
        )
        let problem = name_problem(name, roles)
        if problem != "" {
            text(problem, role: .caption)
        }
        row(fill, distribute: .end) {
            ui.Button("Back") {
                on press: if placement == "room" {
                    set(step, "layout")
                } else { set(step, "identify") }
            }
            ui.Button("Save", role: .primary, enabled: problem == "", description: problem) {
                on press: device
                    .create_group(
                        device.speakers,
                        name: name.trim(),
                        members: chosen.map(
                            (key, i) -> device.SpeakerMemberChoice(
                                device: speaker_device(key),
                                output: speaker_output(key),
                                role: role_choice(
                                    roles.at(i) ?? "",
                                    xs.at(i) ?? 0,
                                    ys.at(i) ?? 0,
                                ),
                            ),
                        ),
                    )
                    .then(set(step, "overview"))
            }
        }
    }
}

/// Measure the set and keep the correction, or take back the one kept. A measurement is a
/// proposal until the person keeps it.
template view speakers_calibrate(
    sp: device.Speakers,
    group: State<Text>,
    listening: State<Text>,
    mic_on: State<List<Text>>,
    mic_off: State<List<Text>>,
    step: State<Text>,
) {
    if let known = sp.groups.find(g -> g.id == group) {
        let run = sp.calibrations.find(c -> c.group == known.id)
        section("Calibrate {known.name}") {
            for m in known.members {
                ui.ListRow(
                    m.device_label,
                    secondary: "{role_text(member_role(m))}, {trim_text(m.trim)}",
                    icon: .speaker,
                    value: member_state_text(m),
                    tone: if m.state == .ready { .success } else { .warning },
                )
            }
            let settling = if let r = run {
                r.phase == .measuring or r.phase == .waiting-for-microphones or r.phase == .proposed
            } else { false }
            if not settling {
                text("Where you sit", role: .caption, secondary: true)
                text(
                    "Each speaker plays a sweep in turn; this microphone hears when it arrives where you sit.",
                    role: .caption,
                    secondary: true,
                )
                ui.MenuButton(listening_label(known, listening), icon: .mic) {
                    for m in known.microphones {
                        if usable(m) {
                            ui.MenuItem(
                                "{m.device_label} ({m.label})",
                                checked: listening_of(known, listening) == m.device,
                            ) {
                                on choose: set(listening, m.device)
                            }
                        }
                    }
                }
                for m in extras(known, listening) {
                    ui.ListRow(
                        if m.local { "{m.device_label} (this device)" } else { m.device_label },
                        secondary: microphone_role_text(m),
                        icon: .mic,
                        selected: also_listens(m, known, mic_on, mic_off),
                    ) {
                        trailing: {
                            ui.Button(
                                "Also listen with {m.device_label}",
                                icon: .check,
                                icon_only: true,
                                toggled: also_listens(m, known, mic_on, mic_off),
                                enabled: usable(m),
                                description: "This device cannot listen for this set.",
                                role: .plain,
                            ) {
                                on press: if also_listens(m, known, mic_on, mic_off) {
                                    set(mic_on, mic_on.filter(d -> d != m.device))
                                        .then(
                                            set(mic_off, [mic_off, [m.device]].flat_map(s -> s)),
                                        )
                                } else {
                                    set(mic_off, mic_off.filter(d -> d != m.device))
                                        .then(
                                            set(mic_on, [mic_on, [m.device]].flat_map(s -> s)),
                                        )
                                }
                            }
                        }
                    }
                }
            }
            if let r = run {
                if r.listening_label != "" {
                    text("Aligned for {r.listening_label}", role: .caption, secondary: true)
                }
                for m in r.members {
                    ui.ListRow(m.label, value: calibration_member_text(m), icon: .speaker)
                }
                for m in r.microphones {
                    ui.ListRow(
                        if m.listening { "{m.label} (where you sit)" } else { m.label },
                        value: calibration_microphone_text(m),
                        icon: .mic,
                    )
                }
                if r.phase == .waiting-for-microphones {
                    text(
                        "Approve on each device that asks, or cancel and switch that microphone off.",
                        role: .caption,
                        secondary: true,
                    )
                }
                if r.message != "" {
                    text(r.message, role: .caption)
                }
            }
            let problem = calibration_problem(known, run)
            if problem != "" {
                text(problem, role: .caption)
            }
            row(gap: .related, wrap: true, fill) {
                ui.Button("Done") {
                    on press: set(step, "overview").then(set(group, ""))
                }
                spacer
                if let r = run {
                    if r.phase == .proposed {
                        ui.Button("Discard") {
                            on press: device.dismiss_calibration(device.speakers, group: known.id)
                        }
                        ui.Button("Apply", role: .primary) {
                            on press: device.apply_calibration(device.speakers, group: known.id)
                        }
                    } else if r.phase == .applied {
                        ui.Button("Revert") {
                            on press: device.revert_calibration(device.speakers, group: known.id)
                        }
                    } else if r.phase == .measuring or r.phase == .waiting-for-microphones {
                        ui.Button("Cancel") {
                            on press: device.cancel_calibration(device.speakers, group: known.id)
                        }
                    } else {
                        ui.Button(
                            "Try again",
                            role: .primary,
                            enabled: problem == "",
                            description: problem,
                        ) {
                            on press: device.start_calibration(
                                device.speakers,
                                group: known.id,
                                excluded: excluded_microphones(known, listening, mic_on, mic_off),
                                listening: seat_microphone(known, listening),
                            )
                        }
                    }
                } else {
                    ui.Button(
                        "Calibrate",
                        role: .primary,
                        enabled: problem == "",
                        description: problem,
                    ) {
                        on press: device.start_calibration(
                            device.speakers,
                            group: known.id,
                            excluded: excluded_microphones(known, listening, mic_on, mic_off),
                            listening: seat_microphone(known, listening),
                        )
                    }
                }
            }
        }
    } else {
        section("Speakers saved") {
            text("Saving your speakers…", role: .caption, secondary: true)
            row(fill, distribute: .end) {
                ui.Button("Done") {
                    on press: set(step, "overview").then(set(group, ""))
                }
            }
        }
    }
}

/// How many outputs the placement takes: the least, then the most.
// ---------------------------------------------------------------------------------------------
// The flow's own rules: which outputs may join, which roles are left, where a speaker stands, and
// what a set needs before it can be made or measured. Each is a function of what the person
// picked, so the page and its controls never disagree.
fn placement_range(placement: Text) -> List<Int> {
    if placement == "stereo" { [2, 2] } else if placement == "surround" { [3, 6] } else { [1, 8] }
}

/// Why the picked outputs cannot go on yet; empty when they can.
fn placement_problem(placement: Text, count: Int) -> Text {
    let range = placement_range(placement)
    let least = range.at(0) ?? 0
    let most = range.at(1) ?? 0
    if count < least {
        "{placement_title(placement)} takes at least {least} speakers."
    } else if count > most {
        "{placement_title(placement)} takes at most {most} speakers."
    } else { "" }
}

/// Takes the picked outputs on: the roles start empty, and around the room each speaker starts on
/// an arc in front of the person until they place it.
fn outputs_roles(placement: Text, chosen: List<Text>) -> List<Text> {
    if placement == "room" { chosen.map((key, i) -> "room") } else { chosen.map(key -> "") }
}

fn outputs_xs(placement: Text, chosen: List<Text>, xs: List<Int>) -> List<Int> {
    if placement == "room" {
        chosen.map((key, i) -> arc_x(i, chosen.len()))
    } else { xs }
}

fn outputs_ys(placement: Text, chosen: List<Text>, ys: List<Int>) -> List<Int> {
    if placement == "room" {
        chosen.map((key, i) -> arc_y(i, chosen.len()))
    } else { ys }
}

/// A speaker's place on the starting arc, across: an even spread over three quarters of a turn.
fn arc_x(index: Int, count: Int) -> Int {
    if count <= 1 {
        0
    } else {
        ((index.to_float() / (count - 1).to_float() - 0.5) * 4712.0).round()
    }
}

/// A speaker's place on the starting arc, ahead: the arc's depth at that angle.
fn arc_y(index: Int, count: Int) -> Int {
    if count <= 1 {
        2000
    } else {
        let turn = index.to_float() / (count - 1).to_float() - 0.5
        (2000.0 * (1.0 - 2.0 * turn.abs())).round()
    }
}

/// The roles of a placement that no other speaker has taken, leaving out the one being answered.
fn free_roles(placement: Text, roles: List<Text>, at: Int) -> List<Text> {
    let all = if placement == "stereo" {
        ["left", "right"]
    } else if placement == "surround" {
        ["left", "right", "center", "lfe", "surround-left", "surround-right"]
    } else { [] }
    all.filter(role -> not taken_elsewhere(roles, role, at))
}

/// Whether another speaker already took `role`.
fn taken_elsewhere(roles: List<Text>, role: Text, at: Int) -> Bool {
    roles.map((taken, i) -> i != at and taken == role).any(taken -> taken)
}

/// Gives the speaker `at` a role and moves on: to the next speaker with no role, or to naming
/// the set. A stereo pair implies the other side, so answering one speaker answers both.
/// The roles after `role` answers the speaker `at`: a stereo pair answers both sides at once.
fn answered_roles(role: Text, at: Int, roles: List<Text>, placement: Text) -> List<Text> {
    if placement == "stereo" {
        let other = if role == "left" { "right" } else { "left" }
        roles.map((taken, i) -> if i == at { role } else { other })
    } else {
        roles.map((taken, i) -> if i == at { role } else { taken })
    }
}

/// The marker and the name of the speaker `at` on the room's plan.
fn placed_shapes(
    sp: device.Speakers,
    chosen: List<Text>,
    xs: List<Int>,
    ys: List<Int>,
    selected: Int,
    at: Int,
) -> List<ui.Shape> {
    let cx = 0.5 + (xs.at(at) ?? 0).to_float() / 10000.0
    let cy = 0.5 - (ys.at(at) ?? 0).to_float() / 10000.0
    [
        ui.Shape.ellipse(
            cx: cx,
            cy: cy,
            rx: 0.012,
            ry: 0.012,
            fill: if at == selected { .accent } else { .ink },
        ),
        ui.Shape.text(
            x: cx + 0.02,
            y: cy,
            text: speaker_name(sp, chosen, at),
            role: .caption,
            ink: if at == selected { .ink } else { .muted },
        ),
    ]
}

/// Where a speaker stands, in millimetres from the person: the plan's point, snapped to the
/// centre line and to any line another speaker stands on, and kept inside the room.
fn placed(axis: List<Int>, selected: Int, at: List<Float>, across: Bool) -> List<Int> {
    let fraction = at.at(if across { 0 } else { 1 }) ?? 0.5
    let value = (if across { fraction - 0.5 } else { 0.5 - fraction } * 10000.0).round()
    // A speaker snaps to the centre line and to the lines the other speakers stand on, never to
    // its own: its own value is set aside so it cannot catch itself.
    let others = axis.map((v, j) -> if j == selected { 100000 } else { v })
    let snapped = if value >= -150 and value <= 150 {
        0
    } else if let line = others.find(v -> v >= value - 150 and v <= value + 150) {
        line
    } else { value }
    axis.map((v, i) -> if i == selected { snapped.clamp(-5000, 5000) } else { v })
}

/// Why the layout cannot go on: two speakers on exactly one spot; empty when it can.
fn crowded(chosen: List<Text>, xs: List<Int>, ys: List<Int>) -> Text {
    let doubled = xs
        .map(
            (x, i) -> xs.map((o, j) -> j > i and o == x and ys.at(j) == ys.at(i)).any(same -> same),
        )
        .any(dupe -> dupe)
    if doubled {
        "Two speakers cannot stand in exactly the same spot."
    } else { "" }
}

/// What a set needs before it can be made: a name, and a role or place for each speaker.
fn name_problem(name: Text, roles: List<Text>) -> Text {
    if name.trim() == "" {
        "Give the speakers a name."
    } else if name.trim().split("").len() > 64 {
        "Keep the name to 64 characters."
    } else if roles.any(role -> role == "") { "Say which speaker each one is first." } else { "" }
}

/// Why a set cannot be measured yet; empty when it can.
fn calibration_problem(known: device.SpeakerGroup, run: Option<device.SpeakerCalibration>) -> Text {
    if not known.led_here {
        return "Calibrate {known.name} on {known.leader_label}, where it was set up."
    }
    if let m = known.members.find(m -> m.state != .ready) {
        return "{m.device_label}: {member_state_text(m)}"
    }
    if known.microphones.filter(m -> usable(m)).is_empty() {
        return "No microphone of your devices can listen for this set."
    }
    if let r = run {
        if r.phase == .measuring or r.phase == .waiting-for-microphones {
            return "A measurement is already going."
        }
    }
    ""
}

/// Whether a microphone can take part at all.
// ---------------------------------------------------------------------------------------------
// The microphones a measurement listens with.
fn usable(m: device.SpeakerMicrophone) -> Bool {
    m.state == .ready or m.state == .not-asked or m.state == .asked
}

/// The microphone at the person's seat: the one picked while it can listen, else this device's,
/// else the first ready one.
fn listening_of(g: device.SpeakerGroup, listening: Text) -> Text {
    let chosen = g.microphones.find(m -> m.device == listening and usable(m))
    if let chosen = chosen {
        return chosen.device
    }
    let local = g.microphones.find(m -> m.local and usable(m))
    if let local = local {
        return local.device
    }
    let ready = g.microphones.find(m -> m.state == .ready)
    if let ready = ready {
        return ready.device
    }
    ""
}

/// What the seat's microphone is called on the page.
fn listening_label(g: device.SpeakerGroup, listening: Text) -> Text {
    let where = listening_of(g, listening)
    if where == "" {
        return "No microphone can listen"
    }
    let found = g.microphones.find(m -> m.device == where)
    if let m = found { "{m.device_label} ({m.label})" } else { where }
}

/// The microphones besides the one at the seat.
fn extras(g: device.SpeakerGroup, listening: Text) -> List<device.SpeakerMicrophone> {
    let where = listening_of(g, listening)
    g.microphones.filter(m -> m.device != where)
}

/// Whether a microphone also listens, besides the one at the seat: the set's own devices that
/// allowed it do unless switched off, and another device's only when switched on.
fn also_listens(
    m: device.SpeakerMicrophone,
    g: device.SpeakerGroup,
    mic_on: List<Text>,
    mic_off: List<Text>,
) -> Bool {
    if not usable(m) {
        return false
    }
    if mic_off.contains(m.device) {
        return false
    }
    if mic_on.contains(m.device) {
        return true
    }
    return m.state == .ready and plays_here(m, g)
}

/// Whether this microphone sits on a device that plays in the set.
fn plays_here(m: device.SpeakerMicrophone, g: device.SpeakerGroup) -> Bool {
    g.members.any(member -> member.device == m.device)
}

/// Switches a microphone on or off for the measurement.
/// The devices whose microphone sits a measurement out: every other usable one not switched on.
fn excluded_microphones(
    g: device.SpeakerGroup,
    listening: Text,
    mic_on: List<Text>,
    mic_off: List<Text>,
) -> List<Text> {
    return g.microphones
        .filter(m -> usable(m) and m.device != listening_of(g, listening))
        .filter(m -> not also_listens(m, g, mic_on, mic_off))
        .map(m -> m.device)
}

/// The microphone a measurement aligns its delays for: the one at the seat, when there is one.
fn seat_microphone(g: device.SpeakerGroup, listening: Text) -> Option<Text> {
    let where = listening_of(g, listening)
    if where == "" { .none } else { .some(where) }
}

/// What a microphone adds to a measurement.
fn microphone_role_text(m: device.SpeakerMicrophone) -> Text {
    if m.state == .ready and m.local {
        "This device"
    } else if m.state == .declined {
        "Declined on that device"
    } else if m.state == .offline {
        "Offline"
    } else if m.state == .unavailable {
        "Media sharing is off there"
    } else if m.state == .ready {
        "Allowed on that device"
    } else if m.state == .asked {
        "Waiting for approval on that device"
    } else {
        "Asks for approval on that device when you start"
    }
}

/// What a set's members say under its name: each device and the place it takes.
// ---------------------------------------------------------------------------------------------
// What each piece of the page says.
fn member_summary(g: device.SpeakerGroup) -> Text {
    g.members.map(m -> "{m.device_label} ({role_text(member_role(m))})").join(", ")
}

/// How many of a set's speakers are ready.
fn ready_here(g: device.SpeakerGroup) -> Int {
    g.members.filter(m -> m.state == .ready).len()
}

/// How many of a set's speakers are ready, as its badge says it.
fn ready_text(g: device.SpeakerGroup) -> Text {
    "{ready_here(g)} of {g.members.len()} ready"
}

/// The placement, by its name in the flow.
fn placement_title(placement: Text) -> Text {
    if placement == "stereo" {
        "Stereo pair"
    } else if placement == "surround" { "Surround" } else { "Around the room" }
}

/// What a placement means.
fn placement_description(placement: Text) -> Text {
    if placement == "stereo" {
        "Two speakers, left and right of where you sit."
    } else if placement == "surround" {
        "Three to six speakers around where you sit, each on its own channel."
    } else {
        "Speakers anywhere in the room, each placed where it stands."
    }
}

/// What a role is called.
fn role_text(role: Text) -> Text {
    if role == "left" {
        "Left"
    } else if role == "right" {
        "Right"
    } else if role == "center" {
        "Centre"
    } else if role == "lfe" {
        "Subwoofer"
    } else if role == "surround-left" {
        "Left surround"
    } else if role == "surround-right" {
        "Right surround"
    } else if role == "room" { "In the room" } else { "—" }
}

/// The words a person answers with, hearing a speaker.
fn role_answer(role: Text) -> Text {
    if role == "left" {
        "On my left"
    } else if role == "right" {
        "On my right"
    } else if role == "center" {
        "In front of me"
    } else if role == "lfe" {
        "It is the subwoofer"
    } else if role == "surround-left" {
        "Behind me, to the left"
    } else if role == "surround-right" { "Behind me, to the right" } else { role }
}

/// The role a speaker of a saved set takes, as the flow names it.
fn member_role(m: device.SpeakerMember) -> Text {
    match m.role {
        .left => "left",
        .right => "right",
        .center => "center",
        .lfe => "lfe",
        .surround-left => "surround-left",
        .surround-right => "surround-right",
        .room(x, y, z) => "room",
    }
}

/// The role value an action takes, from the flow's name for it and the place it stands.
fn role_choice(role: Text, x: Int, y: Int) -> device.SpeakerRole {
    if role == "left" {
        .left
    } else if role == "right" {
        .right
    } else if role == "center" {
        .center
    } else if role == "lfe" {
        .lfe
    } else if role == "surround-left" {
        .surround-left
    } else if role == "surround-right" { .surround-right } else { .room(x_mm: x, y_mm: y, z_mm: 0) }
}

/// What a measurement leaves on a speaker: its delay and gain.
fn trim_text(trim: device.SpeakerTrim) -> Text {
    "{one_decimal(trim.delay_ms)} ms delay, {signed(trim.gain_db)} dB gain"
}

/// A speaker's state in a saved set.
fn member_state_text(m: device.SpeakerMember) -> Text {
    match m.state {
        .ready => "Ready",
        .awaiting-consent => "Waiting for approval on that device",
        .offline => "Offline",
        .output-missing => "Output unplugged",
        .unavailable => "Media sharing is off there",
        .not-certified => "Not one of your devices",
    }
}

/// What plays on a set now.
fn playback_text(playback: device.SpeakerPlayback) -> Text {
    match playback {
        .playing => "Playing",
        .connecting => "Connecting…",
        .syncing-clocks => "Syncing clocks…",
        .clock-failed(reason) => "Can't play in time",
    }
}

/// What one speaker of a measurement came to.
fn calibration_member_text(m: device.CalibrationMember) -> Text {
    if let after = m.after {
        return "Before {trim_text(m.before)}, after {trim_text(after)}"
    }
    match m.state {
        .waiting => "Waiting",
        .measuring => "Listening…",
        .measured(delay, level, latency, acoustic) => measured_text(delay, latency, acoustic),
        .failed(reason) => reason,
    }
}

/// Arrival at the person's seat, and what it is made of, line by line.
fn measured_text(delay: Float, latency: Option<Float>, acoustic: Option<Float>) -> Text {
    [
        "Arrives after {one_decimal(delay)} ms at your seat",
        if let l = latency { "its output takes {one_decimal(l)} ms" } else { "" },
        if let a = acoustic { "about {one_decimal(a * 0.343)} m away" } else { "" },
    ]
        .filter(line -> line != "")
        .join("\n")
}

/// What one microphone of a measurement heard.
fn calibration_microphone_text(m: device.CalibrationMicrophone) -> Text {
    match m.state {
        .awaiting-consent => "Waiting for approval on that device",
        .declined => "Declined on that device",
        .listening => "Listening…",
        .used(heard, rejected) => if rejected > 0 {
            "Heard {heard} speakers, {rejected} readings set aside as inconsistent"
        } else { "Heard {heard} speakers" },
        .not-heard => "Heard nothing: too far away",
        .failed(reason) => "Could not listen: {reason}",
    }
}

/// The plan of the room: its rings, the seat, and every speaker where it stands, the selected one
/// filled.
// ---------------------------------------------------------------------------------------------
// The room plan.
fn room_shapes(
    sp: device.Speakers,
    chosen: List<Text>,
    xs: List<Int>,
    ys: List<Int>,
    selected: Int,
) -> List<ui.Shape> {
    let rings = [1, 2, 3, 4, 5]
        .map(
            ring -> ui.Shape.ellipse(
                cx: 0.5,
                cy: 0.5,
                rx: ring.to_float() / 10.0,
                ry: ring.to_float() / 10.0,
                stroke: .track,
                weight: .hairline,
            ),
        )
    let centre = ui.Shape.line(
        x1: 0.5,
        y1: 0.0,
        x2: 0.5,
        y2: 1.0,
        stroke: .track,
        weight: .hairline,
    )
    let seat = ui.Shape.ellipse(cx: 0.5, cy: 0.5, rx: 0.008, ry: 0.008, fill: .muted)
    let you = ui.Shape.text(
        x: 0.5,
        y: 0.53,
        text: "You",
        role: .caption,
        ink: .muted,
        anchor: .middle,
    )
    let speakers = chosen.flat_map((key, i) -> placed_shapes(sp, chosen, xs, ys, selected, i))
    [rings, [centre, seat, you], speakers].flat_map(s -> s)
}

/// The plan as assistive technology hears it: every speaker and where it stands.
fn room_label(sp: device.Speakers, chosen: List<Text>, xs: List<Int>, ys: List<Int>) -> Text {
    let places = chosen.map(
        (key, i) -> "{speaker_name(sp, chosen, i)}, {position_text(xs.at(i) ?? 0, ys.at(i) ?? 0)}",
    )
    "Speakers in the room: {places.join("; ")}"
}

/// Where a speaker stands, in metres from the person, across and ahead. Within 5 cm reads as
/// level with them.
fn position_text(x: Int, y: Int) -> Text {
    let across = if x <= -50 {
        "{metres(x)} to your left"
    } else if x >= 50 { "{metres(x)} to your right" } else { "centred" }
    let depth = if y >= 50 {
        "{metres(y)} in front of you"
    } else if y <= -50 { "{metres(y)} behind you" } else { "level with you" }
    "{across}, {depth}"
}

/// A length in metres, to one decimal: `1234` is "1.2 m".
fn metres(mm: Int) -> Text {
    "{one_decimal(mm.abs().to_float() / 1000.0)} m"
}

/// A number to one decimal, as a person reads it.
fn one_decimal(value: Float) -> Text {
    let tenths = (value * 10.0).round()
    let whole = tenths / 10
    let part = (tenths - whole * 10).abs()
    "{whole}.{part}"
}

/// A gain, with the sign a person reads: `-1.5` is "-1.5", `2.0` is "+2.0".
fn signed(value: Float) -> Text {
    if value >= 0.0 { "+{one_decimal(value)}" } else { one_decimal(value) }
}

/// What one speaker of the flow is called: its device, and the output when one device gives two.
fn speaker_name(sp: device.Speakers, chosen: List<Text>, index: Int) -> Text {
    if index < 0 or index >= chosen.len() {
        return ""
    }
    let key = chosen.at(index) ?? ""
    let name = speaker_device(key)
    let shared = chosen.map(k -> speaker_device(k)).filter(device -> device == name).len() > 1
    if shared {
        "{device_label(sp, name)} ({speaker_output_label(sp, key)})"
    } else { device_label(sp, name) }
}

/// What the device with id `name` is called.
fn device_label(sp: device.Speakers, name: Text) -> Text {
    let found = sp.devices.find(d -> d.id == name)
    if let d = found { d.label } else { name }
}

/// The key a chosen speaker is kept under: its device and the output it plays through.
fn speaker_key(device: Text, output: Text) -> Text {
    "{device}|{output}"
}

/// The device of a chosen speaker.
fn speaker_device(key: Text) -> Text {
    key.split("|").at(0) ?? ""
}

/// The output a chosen speaker plays through.
fn speaker_output(key: Text) -> Text {
    key.split("|").at(1) ?? ""
}

/// What a chosen speaker's output is called.
fn speaker_output_label(sp: device.Speakers, key: Text) -> Text {
    let selector = speaker_output(key)
    let owner = sp.devices.find(d -> d.id == speaker_device(key))
    if let d = owner {
        let found = d.outputs.find(o -> o.selector == selector)
        if let o = found {
            return o.label
        }
    }
    selector
}

/// The chosen speakers with one added or taken out.
fn toggled(chosen: List<Text>, key: Text) -> List<Text> {
    if chosen.contains(key) {
        chosen.filter(k -> k != key)
    } else { [chosen, [key]].flat_map(s -> s) }
}
