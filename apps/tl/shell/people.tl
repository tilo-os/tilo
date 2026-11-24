use tilo.workspace as workspace

/// Who is here, and their cards. A person's presence on the bar is their face in the group that
/// opens the roster, where each person's card is; an agent's presence joins the same roster, and
/// its own menu stops its run.
///
/// Every entry reads `device.people` (`session:people`), the session's reading of the workspace's
/// members and their presence: who is here, where each is, what they say they are doing, the call
/// they are in, whether this person follows them and whether they follow this person. Nothing
/// here reads a copy of presence. Every choice is an action the session publishes, so a person, an
/// agent and a script follow, wave or write the same way.
///
/// Who else is here, as one api: the faces alone, up to three and then a face counting the
/// rest, so the group says who is here at a glance and takes no words from the bar. The one
/// button opens the roster, and its name names the people for assistive technology. With
/// nobody here it is the people button's own glyph.
view people_button() {
    let people = layout.people
    if people.is_empty() {
        ui.Button("People", icon: .users, icon_only: true, role: .bar) {
            on press: session.toggle_people()
        }
    } else {
        // Who is here, in the fewest words that name them all: one name, two joined, or the
        // first and how many others.
        let one = people.first() ?? LayoutPerson(key: "", name: "People")
        let others = people.len() - 1
        let second = people.last() ?? one
        let label = if others == 0 {
            "{one.name} is here"
        } else if others == 1 {
            "{one.name} and {second.name} are here"
        } else {
            "{one.name} and {others} others are here"
        }
        ui.Button(
            label,
            faces: workspace.members.people.filter(m -> people.any(p -> p.name == m.name())),
            role: .bar,
        ) {
            on press: session.toggle_people()
        }
    }
}

/// The actor a person the session lists is, where the workspace lists their name: a face on a
/// button, which takes the member rather than a name.
fn face_of(p: device.Person) -> Option<Actor> {
    return workspace.members.people.filter(m -> m.name() == p.name).first()
}

/// An agent here, as its presence: a chip that opens its menu. The menu says what it is doing and
/// where to find it, and stopping one ends the connection the person admitted.
view agent_chip(a: device.Agent) {
    let label = if a.unseen > 0 { "{a.name}, {a.unseen} new" } else { a.name }
    ui.MenuButton(label, icon: .bot, role: .bar) {
        if a.doing != "" {
            ui.MenuItem(a.doing, disabled: true)
        }
        ui.MenuItem("Show who is here", icon: .users) {
            on choose: session.toggle_people()
        }
        ui.MenuItem("Stop", icon: .square, destructive: true) {
            on choose: device.stop_agent(device.agents, agent: a.key)
        }
    }
}

/// Who follows this person: marked on their face in the roster, as the bar marks it on the faces
/// there, so a person sees who is watching their view without opening anything.
fn is_follower(p: device.Person) -> Bool {
    p.follower
}

/// Who is here, over the windows: the roster of everyone in the workspace in front, or a person's
/// card where the person opened one. It reads the session's reading every time it draws, so what
/// it says moves with the workspace.
view people() {
    let board = device.people
    row(fill, distribute: .end, padding: .related) {
        ui.FloatingPanel(
            width: if not space.compact { 400.0 },
            fill: space.compact,
        ) {
            header: {
                row(gap: .related, align: .center, wrap: true) {
                    ui.Text(
                        if board.showing != "" { "Who is here" } else { "People" },
                        role: .heading,
                    )
                    spacer
                    ui.Button("Close", icon: .x, icon_only: true, size: .small, role: .plain) {
                        on press: device.close(device.people)
                    }
                }
            }
            body: {
                if board.showing != "" {
                    person_card(board)
                } else {
                    roster(board)
                }
            }
        }
    }
}

/// Everyone here, people first and software after: each row opening that person's card. Someone
/// who follows this person says so on their row.
view roster(board: device.People) {
    // The person at this device stands first in their own roster: their row opens their own
    // card, where what is theirs to change is.
    ui.ListRow(
        "You",
        secondary: availability_word(board.me),
        mark: if board.me.following { .eye } else { .user },
    ) {
        avatar: {
            ui.Avatar(
                name: board.me.name,
                colour: board.me.colour,
                status: presence_status(board.me),
            )
        }
        on activate: device.show_person(device.people, person: "self")
    }
    if board.rows.is_empty() {
        ui.EmptyState(
            "Nobody else is here",
            message: "Invite someone to this workspace, or open the same one on another device",
            icon: .users,
            inline: true,
        )
    } else {
        scroll {
            for p in board.rows {
                person_row(p)
            }
        }
    }
    // The way into the workspace of whoever is not here yet: the session's own invite sheet,
    // where the link is made.
    row(gap: .related, align: .center) {
        ui.Button("Invite someone", icon: .user_plus, size: .small) {
            on press: device.show_invite(device.invite)
        }
    }
}

/// One person's row: their face in the colour the workspace gives them, their name, and the one
/// line that says where they are or what they are doing. An agent's row says the window it works
/// in.
view person_row(p: device.Person) {
    let place = where_line(p)
    let label = if p.name != "" { p.name } else { std.say("Someone") }
    ui.ListRow(
        label,
        secondary: if place != "" { place },
        meta: if is_follower(p) { "Follows you" },
        mark: if p.following {
            .eye
        } else if p.agent { .bot } else if is_follower(p) { .users } else { .user },
    ) {
        avatar: {
            ui.Avatar(name: p.name, colour: p.colour, status: presence_status(p))
        }
        trailing: {
            row(gap: .tight, align: .center) {
                if p.call != "" {
                    ui.Badge("In a call", tone: .info)
                }
                if p.availability == "away" or p.availability == "idle" {
                    ui.Badge(availability_word(p), tone: .neutral)
                }
            }
        }
        on activate: device.show_person(device.people, person: p.key)
    }
    if p.doing != "" {
        ui.Text(p.doing, role: .caption, secondary: true)
    }
}

/// Where someone is, in the fewest words that place them: what they are doing if they said, the
/// window they are in, the app, or the call.
fn where_line(p: device.Person) -> Text {
    if p.doing != "" {
        return ""
    }
    if p.activity != "" {
        let title = p.activity
        if p.agent {
            return std.say("Working in {title}")
        }
        return std.say("Viewing {title}")
    }
    if p.in_app != "" {
        let in_app = p.in_app
        return std.say("In {in_app}")
    }
    if p.call != "" {
        let call = p.call
        return std.say("In {call}")
    }
    return ""
}

/// The presence dot over someone's face: here, away, or heads down.
fn presence_status(p: device.Person) -> Text {
    if p.availability == "away" {
        return "away"
    }
    if p.availability == "idle" {
        return "offline"
    }
    if p.availability == "focusing" {
        return "busy"
    }
    return "online"
}

/// How available someone is, in the person's words.
fn availability_word(p: device.Person) -> Text {
    if p.availability == "focusing" {
        return "Focusing"
    }
    if p.availability == "away" {
        return "Away"
    }
    if p.availability == "idle" {
        return "Idle"
    }
    return "Available"
}

/// One person's card: their face, their name, what they say they are doing, where they are and
/// the call they are in, with the actions their card offers. The person at this device gets their
/// own card instead ([`own_card`]), where their name, what they are doing and how available they
/// are are theirs to change.
view person_card(board: device.People) {
    let someone = person_shown(board)
    if someone.key == "" or someone.me {
        own_card(board, someone)
    } else {
        stack(gap: .related) {
            card_head(someone)
            row(gap: .related, align: .center, wrap: true) {
                if someone.following {
                    ui.Button("Stop following", icon: .x, size: .small) {
                        on press: device.unfollow(device.people)
                    }
                } else {
                    ui.Button("Follow", icon: .eye, size: .small) {
                        on press: device.follow(device.people, person: someone.key)
                    }
                }
                ui.Button("Message", icon: .message_circle, size: .small) {
                    on press: device.message(device.people, person: someone.key)
                }
                ui.Button("Wave", icon: .hand, size: .small) {
                    on press: device.wave(device.people, person: someone.key)
                }
                if someone.activity != "" or someone.in_app != "" {
                    ui.Button("Go there", icon: .arrow_right, size: .small) {
                        on press: device.go_to(device.people, person: someone.key)
                    }
                }
            }
            if someone.call != "" {
                ui.Text(std.say("In a call: {someone.call}"), role: .caption)
            }
            back_to_everyone()
        }
    }
}

/// The person whose card is open, as the reading names them: someone the roster no longer lists
/// (they left) falls back to the person at this device, whose card is always the person's own.
fn person_shown(board: device.People) -> device.Person {
    if board.showing == "self" {
        return board.me
    }
    if let found = board.rows.find(p -> p.key == board.showing) {
        return found
    }
    return board.me
}

/// A card's head: their face in their colour, their name, how available they are and what they
/// say they are doing.
view card_head(p: device.Person) {
    row(gap: .related, align: .start) {
        if p.agent {
            ui.Avatar(name: p.name, colour: p.colour, square: true, glyph: .bot)
        } else {
            ui.Avatar(name: p.name, colour: p.colour, status: presence_status(p))
        }
        stack(gap: .tight) {
            ui.Text(if p.name != "" { p.name } else { std.say("Someone") }, role: .subheading)
            row(gap: .tight, align: .center) {
                ui.Badge(
                    availability_word(p),
                    tone: if p.availability == "away" {
                        .warning
                    } else if p.availability == "focusing" { .info } else { .success },
                )
                if is_follower(p) {
                    ui.Text("Follows you", role: .caption, secondary: true)
                }
            }
            if p.doing != "" {
                ui.Text(p.doing, role: .body)
            }
            if where_line(p) != "" {
                ui.Text(where_line(p), role: .caption, secondary: true)
            }
        }
    }
}

/// The person's own card: their face and name, and the three things that are theirs to say — the
/// name everyone here sees, what they are doing, and how available they are. Each is one action of
/// the session's, so an agent does the same, and the change reaches every device.
view own_card(board: device.People, someone: device.Person) {
    let me = board.me
    state name: Text = ""
    state doing: Text = ""
    stack(gap: .related) {
        ask_card()
        row(gap: .related, align: .center) {
            ui.Avatar(name: me.name, colour: me.colour, status: presence_status(me))
            stack(gap: .tight) {
                ui.Text(if me.name != "" { me.name } else { std.say("You") }, role: .subheading)
                ui.Badge(
                    availability_word(me),
                    tone: if me.availability == "away" {
                        .warning
                    } else if me.availability == "focusing" { .info } else { .success },
                )
            }
        }
        ui.TextInput(bind: name, placeholder: "Your name") {
            on submit: device.set_name(device.people, name: if name == "" { me.name } else { name })
        }
        ui.TextInput(bind: doing, placeholder: "What you are doing") {
            on submit: device.set_doing(device.people, text: doing)
        }
        row(gap: .tight, align: .center, wrap: true) {
            for a in ["available", "focusing", "away", "idle"] {
                ui.Chip(
                    availability_named(a),
                    selected: me.availability == a,
                    icon: if me.availability == a { .check } else { .circle },
                ) {
                    on press: device.set_availability(device.people, availability: a)
                }
            }
        }
        // What is the person's own to reach from their card: another of their devices, and their
        // own settings — their accounts, their devices and what they share.
        row(gap: .tight, align: .center, wrap: true) {
            ui.Button("Add a device", icon: .plus, size: .small) {
                on press: device.pair_device(device.people)
            }
            ui.Button("Your devices", icon: .settings, size: .small) {
                on press: device.open_settings(device.people, link: "settings://devices_access")
            }
        }
        if someone.key != "" and not someone.me {
            back_to_everyone()
        }
    }
}

/// What a named availability is called in the person's words.
fn availability_named(name: Text) -> Text {
    if name == "focusing" {
        return "Focusing"
    }
    if name == "away" {
        return "Away"
    }
    if name == "idle" {
        return "Idle"
    }
    return "Available"
}

/// Back from a card to the roster: the card closes, the roster stays.
view back_to_everyone() {
    ui.Button("Back to everyone", icon: .chevron_left, size: .small) {
        on press: device.show_person(device.people, person: "")
    }
}

/// Whose view this is while this person follows someone: their face and "Following Ada" where the
/// bar's glance value stands, with the way to stop. The person's own clock gives way to it, as
/// their own view is not the one the bar is saying.
view follow_chip() {
    let board = device.people
    if board.following != "" {
        if let who = board.rows.find(p -> p.key == board.following) {
            let name = who.name
            row(gap: .tight, align: .center) {
                if let face = face_of(who) {
                    ui.Button(std.say("Following {name}"), faces: [face], role: .bar) {
                        on press: device.show_person(device.people, person: who.key)
                    }
                } else {
                    ui.Button(
                        std.say("Following {name}"),
                        icon: .eye,
                        role: .bar,
                    ) {
                        on press: device.show_person(device.people, person: who.key)
                    }
                }
                ui.Button("Stop", icon: .square, size: .small, role: .plain) {
                    on press: device.unfollow(device.people)
                }
            }
        }
    }
}

/// The windows this device shares with its workspaces, beside the faces of who is watching them:
/// how many members ask for one, and each window with the way to stop sharing it.
view sharing_indicator() {
    let sharing = device.shares.sharing
    if not sharing.is_empty() {
        let windows = sharing.len()
        let label = if windows == 1 {
            std.say("Sharing 1 window")
        } else { std.say("Sharing {windows} windows") }
        ui.MenuButton(label, icon: .screen_share, role: .bar) {
            for x in sharing {
                // The window as this shell names it, never its app's id.
                let shown = if let w = device.windows.all.find(w -> w.key == x.tab) {
                    w.title
                } else { "" }
                if shown != "" {
                    ui.MenuItem(
                        std.say("Stop sharing {shown}"),
                        icon: .eye_off,
                        destructive: true,
                    ) {
                        on choose: device.stop(device.shares, tab: x.tab)
                    }
                } else {
                    ui.MenuItem("Stop sharing", icon: .eye_off, destructive: true) {
                        on choose: device.stop(device.shares, tab: x.tab)
                    }
                }
            }
        }
    }
}

/// Someone waved at this person: who, beside the faces, for a moment. The session clears it when
/// its moment is over, so the bar never keeps a word that has gone stale.
view wave_notice() {
    let board = device.people
    if board.waved != "" {
        let name = board.waved
        ui.Chip(std.say("{name} waved at you"), icon: .hand, tone: .info)
    }
}

/// Someone arrived where this person is: the first time in a public workspace, what its people
/// see of the person instead, opening their own card where they choose otherwise.
view arrival_notice() {
    let board = device.people
    if board.arrived_seen != "" {
        ui.Button(board.arrived_seen, icon: .eye, size: .small, role: .plain) {
            on press: device.show_person(device.people, person: "self")
        }
    } else if not board.arrived.is_empty() {
        let first = board.arrived.first() ?? ""
        let short = first.split(" ").first() ?? first
        let others = board.arrived.len() - 1
        let label = if others == 0 {
            std.say("{short} is here")
        } else if others == 1 {
            std.say("{short} and 1 other are here")
        } else {
            std.say("{short} and {others} others are here")
        }
        ui.Chip(label, icon: .users, tone: .success) {
            on press: device.show(device.people)
        }
    }
}

/// The question the session puts to the person about themselves, as a card on their own card,
/// where a question about the person belongs: what it asks, what saying yes does, and the two
/// ways to put it off. The bar's avatar shows a dot while one waits, and opening it comes here.
/// The question, its answer and its detail are the session's own (`device.overlays`), so
/// answering it anywhere answers it here.
view ask_card() {
    let ask = device.overlays
    if ask.ask != "" {
        ui.Card(title: ask.ask) {
            ui.Text(ask.ask_detail, role: .caption, secondary: true)
            row(gap: .tight, align: .center, wrap: true) {
                ui.Button(ask.ask_answer) {
                    on press: device.answer_ask(device.overlays)
                }
                ui.Button("Not now", role: .plain) {
                    on press: device.put_off_ask(device.overlays)
                }
            }
        }
    }
}
