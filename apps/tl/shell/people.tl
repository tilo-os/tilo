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
/// Who else is here: each of the first three people as their own face, a press on which opens
/// their card, with a dot on the face of anyone who follows this person, and then the button that
/// opens the roster, which counts the rest. Its name names everyone here for assistive
/// technology. With nobody here it is the people button's own glyph.
view people_button() {
    let people = layout.people
    if people.is_empty() {
        ui.Button("People", icon: .users, icon_only: true, role: .bar) {
            on press: device.show(device.people)
        }
    } else {
        let shown = device.people.rows.filter(p -> not p.agent).take(3)
        row(gap: .none, align: .center) {
            for p in shown {
                face_button(p)
            }
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
            let rest = people.len() - shown.len()
            if rest > 0 {
                ui.Button(label, icon: .users, count: rest, icon_only: true, role: .bar) {
                    on press: device.show(device.people)
                }
            } else {
                ui.Button(label, icon: .users, icon_only: true, role: .bar) {
                    on press: device.show(device.people)
                }
            }
        }
    }
}

/// One person here as their face on the bar: a press opens their card, and a dot says they follow
/// this person.
view face_button(p: device.Person) {
    let name = if p.name != "" { p.name } else { std.say("Someone") }
    let label = if is_follower(p) { std.say("{name}, follows you") } else { name }
    if let face = face_of(p) {
        ui.Button(label, faces: [face], icon_only: true, dot: is_follower(p), role: .bar) {
            on press: device.show_person(device.people, person: p.key)
        }
    } else {
        ui.Button(label, icon: .user, icon_only: true, dot: is_follower(p), role: .bar) {
            on press: device.show_person(device.people, person: p.key)
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
            on choose: device.show(device.people)
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
    if space.compact {
        // A phone has room for one thing at a time: the roster is a sheet at the shell's foot,
        // between equal gutters, and a press above it puts it away. The layer places it there: a
        // stack's own children go top to bottom.
        layers(fill) {
            stack(at: .bottom, padding: .related) {
                people_panel(board, sheet: true)
            }
        }
    } else {
        row(fill, distribute: .end, padding: .related) {
            people_panel(board, sheet: false)
        }
    }
}

/// The roster's panel: its header, the roster or a card, and nothing else. It holds the screen
/// while it shows, so the keyboard stays in it, and a press off it closes it.
view people_panel(board: device.People, sheet: Bool) {
    ui.FloatingPanel(
        width: if not sheet { 400.0 },
        fill: sheet,
        modal: true,
        label: "People",
        description: "Who is here, where they are and what they are doing",
    ) {
        header: {
            row(gap: .related, align: .center, wrap: true) {
                stack(gap: .none) {
                    ui.Text(people_title(board), role: .heading)
                    // Who is here besides this person, counted as the list is: "2 here" over
                    // one row reads as someone missing.
                    ui.Text(here_line(board.rows.len()), role: .caption, secondary: true)
                }
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
        on dismiss: device.close(device.people)
    }
}

/// The roster's title: the workspace whose people these are, the one fact that says which
/// workspace's roster a person is looking at. A card in the roster's place is about one person, so
/// it says that instead.
fn people_title(board: device.People) -> Text {
    if board.showing != "" {
        return std.say("Who is here")
    }
    if front_workspace_name() != "" {
        return front_workspace_name()
    }
    return std.say("People")
}

/// Who is here besides this person, counted as the list counts them: never this person, who is the
/// "you" of the words.
fn here_line(others: Int) -> Text {
    if others == 0 {
        return std.say("Just you")
    }
    if others == 1 {
        return std.say("You and 1 other")
    }
    return std.say("You and {others} others")
}

/// Everyone here, people first and software after: each row opening that person's card. Someone
/// who follows this person says so on their row. Where both people and software are here, each
/// group stands under its own heading, so software is never read as someone who came in.
view roster(board: device.People) {
    // The person at this device stands first in their own roster: their row opens their own
    // card, where what is theirs to change is.
    ui.ListRow(
        "You",
        secondary: availability_word(board.me),
        mark: if board.following != "" { .eye } else { .user },
    ) {
        avatar: {
            ui.Avatar(
                name: board.me.name,
                colour: board.me.colour,
                status: presence_status(board.me),
                call: board.me.call != "",
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
        let everyone = board.rows.filter(p -> not p.agent)
        let software = board.rows.filter(p -> p.agent)
        // A heading earns its line only where it tells one group from another: with one group the
        // roster's title already says who is here.
        let labelled = not everyone.is_empty() and not software.is_empty()
        // As tall as the people here, up to the room a roster takes: past it the list scrolls
        // and builds only the rows it shows.
        ui.VirtualList(name: "People here now", most: space.rem * 34.0) {
            for p in everyone {
                person_listed(p, board, if labelled { "People" } else { "" })
            }
            for p in software {
                person_listed(p, board, if labelled { "Agents" } else { "" })
            }
        }
    }
    // The way into the workspace of whoever is not here yet: the session's own invite sheet,
    // where the link is made, offered only where the workspace lets this person invite.
    if board.may_invite {
        row(gap: .related, align: .center) {
            ui.Button("Invite someone", icon: .user_plus, size: .small) {
                on press: device.show_invite(device.invite)
            }
        }
    }
}

/// One person as a row of the list who is here: what their row shows, and the actions its menu
/// keeps reachable from the keyboard.
view person_listed(p: device.Person, board: device.People, group: Text) {
    let label = if p.name != "" { p.name } else { std.say("Someone") }
    let followed = p.following or board.following == p.key
    ui.ListItem(value: p.key, group: if group != "" { group }) {
        person_row(p, board)
        menu: {
            if not p.agent {
                ui.MenuItem("Wave at {label}", icon: .hand) {
                    on choose: device.wave(device.people, person: p.key)
                }
            }
            if followed {
                ui.MenuItem("Stop following {label}", icon: .x) {
                    on choose: device.unfollow(device.people)
                }
            } else {
                ui.MenuItem("Follow {label}", icon: .eye) {
                    on choose: device.follow(device.people, person: p.key)
                }
            }
        }
    }
}

/// What a reader hears of a person beyond their name: whether they are software, where they are,
/// what they say they are doing, how available they are, and whether they follow this person.
fn spoken_details(p: device.Person) -> Text {
    // Who they are as software, and when they joined, stand on the row itself: its tag and the
    // line under its name. The details say what those lines do not.
    let said: List<Text> = []
    // The place says where they are, never the call the line under it already says.
    if p.activity != "" or p.in_app != "" {
        let place = where_line(p)
        if place != "" {
            said = said + [std.say("Now: {place}")]
        }
    }
    let doing = doing_line(p)
    if p.call != "" {
        let call = p.call
        said = said + [std.say("In a call: {call}")]
    } else if doing != "" {
        said = said + [doing]
    } else {
        said = said + [availability_word(p)]
    }
    if is_follower(p) {
        said = said + [std.say("Follows you")]
    }
    return said.join("; ")
}

/// One person's row: their face in the colour the workspace gives them, their name, and the one
/// line that says where they are or what they are doing. An agent's row says the window it works
/// in, with the way there; software is tagged as what it is, and nobody waves at it.
view person_row(p: device.Person, board: device.People) {
    let name = if p.name != "" { p.name } else { std.say("Someone") }
    let doing = doing_line(p)
    // Someone nobody named is told apart by when they joined: their name and face are every
    // unnamed person's, and the line where they are says nothing about who they are.
    let joined = if p.joined != "" { std.say("Joined {p.joined}") } else { "" }
    // The person at this device follows them: the reading says so where their own word is, and
    // the row's presence flag where the board has carried it.
    let followed = p.following or board.following == p.key
    let following = following_line(p, board)
    let place = if following != "" { following } else { where_line(p) }
    // The line under the name says where they are, or for someone nobody named when they joined;
    // what they are doing is the row's own line under that, so the two are never the same words.
    let secondary = if joined != "" { joined } else { place }
    ui.ListRow(
        name,
        secondary: if secondary != "" { secondary },
        note: if doing != "" { doing },
        description: spoken_details(p),
        badge: if p.agent { software_tag(p) },
        mark: if p.automatic {
            // Software says what kind it is before its name, as a reader and a painter both find it:
            // an agent someone here runs, or one of the workspace's bots.
            // What software wrote for them is marked on the row's edge, never shown as if they had
            // said it; then where this person's own relation to them shows.
            .sparkles
        } else if followed {
            .eye
        } else if p.agent { .bot } else if is_follower(p) { .users } else { .user },
    ) {
        avatar: {
            // Someone in a call wears the arc sweeping round their face, as they do on the bar.
            if p.agent {
                ui.Avatar(name: p.name, colour: p.colour, square: true, glyph: .bot)
            } else {
                ui.Avatar(
                    name: p.name,
                    colour: p.colour,
                    status: presence_status(p),
                    call: p.call != "",
                )
            }
        }
        trailing: {
            row(gap: .tight, align: .center) {
                // Follow and Wave stand on the row itself, for any row, software included: an
                // agent that has published no view yet is followed as it arrives.
                if not p.agent {
                    wave_button(p, board, name)
                }
                follow_button(followed, p, name)
            }
        }
        on activate: device.show_person(device.people, person: p.key)
    }
    if p.agent and goable(p) {
        // Where software works: the window it is in, with the way there. Pressing it brings that
        // window forward, so what it is on stands in front of this person.
        row(gap: .related, align: .center) {
            ui.Button("Go there", icon: .arrow_right, size: .small, role: .plain) {
                on press: device.go_to(device.people, person: p.key)
            }
        }
    }
}

/// What kind of software a row is: one of the workspace's bots, or an agent someone here runs.
fn software_tag(p: device.Person) -> Text {
    if p.bot {
        return std.say("Bot")
    }
    return std.say("Agent")
}

/// Whether where software works is known here: a window of this workspace, or the app it is in.
fn goable(p: device.Person) -> Bool {
    p.activity != "" or p.in_app != ""
}

/// The wave on a row: sent, so the waver knows it went, or the way to send one.
view wave_button(p: device.Person, board: device.People, name: Text) {
    if board.waved_to == p.key {
        ui.Button(
            "Waved",
            icon: .check,
            size: .small,
            role: .plain,
            accessible_name: std.say("You waved at {name}"),
        ) {
            on press: device.wave(device.people, person: p.key)
        }
    } else {
        ui.Button(
            "Wave",
            icon: .hand,
            size: .small,
            role: .plain,
            accessible_name: std.say("Wave at {name}"),
        ) {
            on press: device.wave(device.people, person: p.key)
        }
    }
}

/// The follow on a row: the way to mirror their view, or the way to stop.
view follow_button(followed: Bool, p: device.Person, name: Text) {
    if followed {
        ui.Button(
            "Stop",
            icon: .x,
            size: .small,
            role: .plain,
            accessible_name: std.say("Stop following {name}"),
        ) {
            on press: device.unfollow(device.people)
        }
    } else {
        ui.Button(
            "Follow",
            icon: .eye,
            size: .small,
            role: .plain,
            accessible_name: std.say("Follow {name}"),
        ) {
            on press: device.follow(device.people, person: p.key)
        }
    }
}

/// Whom a follower's row says they follow, after where they are: another person here, by their
/// name, or "you" for the person at this device.
fn following_line(p: device.Person, board: device.People) -> Text {
    if p.follows == "" {
        return ""
    }
    let place = where_line(p)
    if p.follows == "self" {
        if place != "" {
            return std.say("{place}, following you")
        }
        return std.say("Following you")
    }
    let who = if let found = board.rows.find(r -> r.key == p.follows) { found.name } else { "" }
    if who == "" {
        return ""
    }
    if place != "" {
        return std.say("{place}, following {who}")
    }
    return std.say("Following {who}")
}

/// What someone is doing, in the fewest words that say it: the call they are in, else what they
/// said, else how available they are.
fn doing_line(p: device.Person) -> Text {
    if p.call != "" {
        let call = p.call
        return std.say("In {call}")
    }
    if p.doing != "" {
        return p.doing
    }
    if p.availability == "focusing" {
        return std.say("Focusing")
    }
    if p.availability == "away" {
        return std.say("Away")
    }
    if p.availability == "idle" {
        return std.say("Idle")
    }
    return ""
}

/// Where someone is, in the fewest words that place them: the window they are in, or the app when
/// this shell shows no window of it.
fn where_line(p: device.Person) -> Text {
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
///
/// What can be done with them is what the session says can: following their view, waving, going
/// where they work, and a conversation, which Chat keeps on the key both people know, so the
/// button stands only where Chat is here and that key is known. Their card lists the devices they
/// are here on, each with how available they are there, and the accounts they keep elsewhere.
view person_card(board: device.People) {
    let someone = person_shown(board)
    if someone.key == "" or someone.me {
        own_card(board, someone)
    } else {
        stack(gap: .related) {
            card_head(someone)
            row(gap: .related, align: .center, wrap: true) {
                // Whose view this is, as the bar says it: this person's row may say they are
                // followed by someone here before the roster's own reading of it arrives.
                if someone.following or board.following == someone.key {
                    ui.Button("Stop following", icon: .x, size: .small) {
                        on press: device.unfollow(device.people)
                    }
                } else {
                    ui.Button("Follow", icon: .eye, size: .small) {
                        on press: device.follow(device.people, person: someone.key)
                    }
                }
                if someone.can_message {
                    ui.Button("Message", icon: .message_circle, size: .small, role: .primary) {
                        on press: device.message(device.people, person: someone.key)
                    }
                }
                ui.Button("Wave", icon: .hand, size: .small) {
                    on press: device.wave(device.people, person: someone.key)
                }
                if someone.activity != "" or someone.in_app != "" {
                    ui.Button("Go there", icon: .arrow_right, size: .small) {
                        on press: device.go_to(device.people, person: someone.key)
                    }
                }
                if someone.agent {
                    // What an agent is at is its own work, which its card in Console shows.
                    ui.Button("See its work", icon: .panel_right, size: .small, role: .primary) {
                        on press: device.agent(
                            device.people,
                            person: someone.key,
                            client: someone.agent_client,
                            ask: "card",
                        )
                    }
                }
            }
            // What an agent is at, as its live line: it reads as now, and a press on it goes to
            // where it works, so the person sees what it does.
            if someone.agent and someone.doing != "" {
                ui.ListRow(someone.doing, secondary: "Now") {
                    on activate: device.go_to(device.people, person: someone.key)
                }
            }
            if someone.call != "" {
                ui.Text(std.say("In a call: {someone.call}"), role: .caption)
            }
            // Where they are here: each of their devices, in the order it answers, with how
            // available they are there. A device that named itself nothing reads as the one it
            // is, and every row's key is copied whole where a person wants it.
            if not someone.devices.is_empty() {
                ui.Text("Devices", role: .caption, secondary: true)
                for d in someone.devices {
                    let named = if d.label != "" {
                        d.label
                    } else if d.this { std.say("This computer") } else { std.say("Another device") }
                    let copy = if d.label != "" {
                        std.say("Copy the key of {d.label}")
                    } else { std.say("Copy this device's key") }
                    ui.ListRow(
                        named,
                        meta: availability_named(d.availability),
                        icon: .monitor_smartphone,
                    ) {
                        trailing: {
                            row(gap: .tight, align: .center) {
                                if d.this {
                                    ui.Text("this device", role: .caption, secondary: true)
                                }
                                ui.Button(
                                    copy,
                                    icon: .copy,
                                    icon_only: true,
                                    size: .small,
                                    role: .plain,
                                ) {
                                    on press: device.copy(device.clipboard, text: d.id)
                                }
                            }
                        }
                    }
                }
            }
            // The key they are here by, short, and copied whole: what a record about them is
            // addressed by, never shown in full where a card can be read by anyone looking.
            if someone.user_key != "" {
                row(fill, distribute: .between, align: .center) {
                    stack(gap: .tight) {
                        ui.Text("Person ID", role: .caption, secondary: true)
                        ui.Text(short_key(someone.user_key), role: .code)
                    }
                    ui.Button("Copy ID", icon: .copy, icon_only: true, size: .small, role: .plain) {
                        on press: device.copy(device.clipboard, text: someone.user_key)
                    }
                }
            }
            // The accounts they keep at the services this workspace is connected to.
            if not someone.linked.is_empty() {
                ui.Text("Linked accounts", role: .caption, secondary: true)
                for l in someone.linked {
                    ui.ListRow(std.say("Linked to @{l.handle} on {l.service}"), icon: l.icon)
                }
            }
            if someone.agent {
                agent_commands(someone)
            }
            back_to_everyone()
        }
    }
}

/// What can be done with an agent from its face: Stop, Undo run, Take over and Revoke, each
/// carried out by its card in Console, which comes forward to show what came of it. Labelled,
/// never bare icons: Stop and Revoke must not be guessed at.
view agent_commands(a: device.Person) {
    row(gap: .tight, align: .center, wrap: true) {
        ui.Button("Stop", icon: .square, size: .small, role: .plain) {
            on press: device.agent(
                device.people,
                person: a.key,
                client: a.agent_client,
                ask: "stop",
            )
        }
        ui.Button("Undo run", icon: .undo_2, size: .small, role: .plain) {
            on press: device.agent(
                device.people,
                person: a.key,
                client: a.agent_client,
                ask: "undo-run",
            )
        }
        ui.Button("Take over", icon: .hand, size: .small, role: .plain) {
            on press: device.agent(
                device.people,
                person: a.key,
                client: a.agent_client,
                ask: "take-over",
            )
        }
        ui.Button("Revoke access", icon: .ban, size: .small, role: .destructive) {
            on press: device.agent(
                device.people,
                person: a.key,
                client: a.agent_client,
                ask: "revoke",
            )
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
/// say they are doing. Their face is the member's, so a picture they set shows here as it does on
/// the bar; an agent wears its own mark.
view card_head(p: device.Person) {
    row(gap: .related, align: .start) {
        if let face = face_of(p) {
            ui.Avatar(
                who: face,
                colour: p.colour,
                status: if p.agent { "" } else { presence_status(p) },
                square: p.agent,
            )
        } else if p.agent {
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
            // What they say they are doing, plainly, and a line software wrote marked as read off
            // what they do; an agent's own line is drawn under its card's actions, where a press
            // on it goes to what it is doing. Where it works stands under their name all the same.
            if not p.agent and p.doing != "" {
                if p.automatic {
                    ui.ListRow(
                        p.doing,
                        secondary: std.say("Written automatically from what they do"),
                        mark: .sparkles,
                    )
                } else {
                    ui.ListRow(p.doing, secondary: std.say("In their own words"))
                }
            }
            if p.agent and p.in_app != "" {
                let in_app = p.in_app
                ui.Text(std.say("In {in_app}"), role: .caption, secondary: true)
            } else if where_line(p) != "" {
                ui.Text(where_line(p), role: .caption, secondary: true)
            }
        }
    }
}

/// The person's own card: their face and name, the settings at its top, and the three things that
/// are theirs to say — the name everyone here sees, what they are doing, and how available they
/// are. Each is one action of the session's, so an agent does the same, and the change reaches
/// every device.
///
/// There is no Save button, so nothing typed is lost: a field keeps what it holds on Enter, when
/// it is left, when the card closes, and once typing pauses. A field starts on the name they gave,
/// never on the words that stand in for one they have not, so an unnamed person is asked for a
/// name rather than handed something to type over.
view own_card(board: device.People, someone: device.Person) {
    let me = board.me
    // The field holds the name they gave, never the words that stand in for one they have not.
    state name: Text = board.me.given
    // What this device read off their steps is not theirs to edit: the field holds what they said.
    state doing: Text = if board.me.automatic { "" } else { board.me.doing }
    stack(gap: .related) {
        ask_card()
        row(gap: .related, align: .center) {
            // The picture is the button, where the host offers to change it: what a person aims
            // at is what changes.
            if board.photo_change != "" {
                ui.Button("Change picture", faces: [me_face(board, invoker)], role: .plain) {
                    on press: device.host_action(device.people, id: board.photo_change)
                }
            } else {
                ui.Avatar(name: me.name, colour: me.colour, status: presence_status(me))
            }
            stack(gap: .tight, fill: 1.0) {
                ui.TextInput(
                    bind: name,
                    placeholder: if me.given == "" {
                        std.say("Add your name")
                    } else { std.say("Your name") },
                ) {
                    on changed(text): device.draft(device.people, field: "name", text: text)
                    on submit: device.set_name(device.people, name: name)
                    on blur: device.set_name(device.people, name: name)
                }
                ui.Text("How everyone here sees you", role: .caption, secondary: true)
            }
        }
        if board.photo_change != "" or board.photo_remove != "" {
            row(gap: .tight, align: .center, wrap: true) {
                if board.photo_change != "" {
                    ui.Button("Change picture", icon: .camera, size: .small, role: .plain) {
                        on press: device.host_action(device.people, id: board.photo_change)
                    }
                }
                if board.photo_remove != "" {
                    ui.Button("Remove picture", icon: .x, size: .small, role: .plain) {
                        on press: device.host_action(device.people, id: board.photo_remove)
                    }
                }
            }
        }
        // The card's way into Settings sits at its top, under the person's name and picture:
        // where someone scanning the card for it looks first, before what they are doing.
        for a in board.actions.filter(a -> a.group == "settings") {
            ui.Button(a.title, icon: a.icon, role: .plain) {
                on press: device.host_action(device.people, id: a.id)
            }
        }
        // Status: how available they say they are, then what they are doing in their own words.
        ui.Text("Status", role: .caption, secondary: true)
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
        ui.TextInput(bind: doing, placeholder: "What you're doing") {
            on changed(text): device.draft(device.people, field: "doing", text: text)
            on submit: device.set_doing(device.people, text: doing)
            on blur: device.set_doing(device.people, text: doing)
        }
        // The colour their face is drawn in: the twelve people colours, the one they wear marked,
        // and a choice that reaches every device at once.
        if not board.colours.is_empty() {
            ui.Text("Your colour", role: .caption, secondary: true)
            ui.ColorSwatchPicker(
                swatches: board.colours.map(c -> c.colour),
                columns: 6,
                selected: me.colour_slot,
                label: "Your colour",
            ) {
                on change(i): device.colour(device.people, slot: i)
            }
        }
        // What the person as others see them can do: keeping who they are, and their picture.
        if not board.actions.filter(a -> a.group == "identity").is_empty() {
            ui.Text("Your identity", role: .caption, secondary: true)
        }
        for a in board.actions.filter(a -> a.group == "identity").take(2) {
            ui.Button(a.title, icon: a.icon, role: .plain) {
                on press: device.host_action(device.people, id: a.id)
            }
        }
        // What is the person's own to reach from their card: another of their devices, and their
        // own settings — their accounts, their devices and what they share.
        for a in board.actions.filter(a -> a.group == "devices") {
            ui.Button(a.title, icon: a.icon, role: .plain) {
                on press: device.host_action(device.people, id: a.id)
            }
        }
        // The one setting the desktop's control hints have, where the host offers them.
        if let hints = board.control_hints {
            ui.Toggle(shown: hints, label: "Show control hints") {
                on check(turned): device.control_hints(device.people, on: turned)
            }
        }
        if someone.key != "" and not someone.me {
            back_to_everyone()
        }
    }
}

/// A key shown short: its first eight and last four characters, as a card shows the ID it copies
/// whole.
fn short_key(key: Text) -> Text {
    if key.len() <= 14 {
        return key
    }
    return "{key.slice(0, 8)}…{key.slice(key.len() - 4, key.len())}"
}

/// The person at this device as a face, for the picture that changes theirs: the member the
/// workspace lists them as, so the button wears their picture and colour; the person invoking the
/// view where the workspace lists none.
fn me_face(board: device.People, me: Actor) -> Actor {
    if let face = face_of(board.me) {
        return face
    }
    return me
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
///
/// A bar too narrow for the words folds the chip into their face, with Stop beside it: their face
/// and the way to stop stay on a phone, where the bar shows no other face of them. `folded` is
/// that: the bar's room, never the chip's own.
view follow_chip(folded: Bool) {
    let board = device.people
    if board.following != "" {
        if let who = board.rows.find(p -> p.key == board.following) {
            let name = who.name
            let label = std.say("Following {name}")
            row(gap: .tight, align: .center) {
                following_face(who, label, words: not folded)
                ui.Button("Stop", icon: .square, icon_only: folded, size: .small, role: .plain) {
                    on press: device.unfollow(device.people)
                }
            }
        }
    } else if board.stopped != "" {
        // Their own input took their view back: the bar says so where it said whose view this was,
        // and the moment passes. Someone who left takes the following with them, and says nothing.
        if let who = board.rows.find(p -> p.key == board.stopped) {
            if let face = face_of(who) {
                ui.Chip(std.say("Stopped following"), faces: [face], live: .polite)
            } else {
                ui.Chip(std.say("Stopped following"), icon: .eye_off, live: .polite)
            }
        } else {
            ui.Chip(std.say("Stopped following"), icon: .eye_off, live: .polite)
        }
    }
}

/// The face of the person followed, a press on which opens their card: with `label`, whose view
/// this is, beside it where the bar has room, and alone where it has none.
view following_face(who: device.Person, label: Text, words: Bool) {
    if let face = face_of(who) {
        ui.Button(label, faces: [face], icon_only: not words, role: .bar, live: .polite) {
            on press: device.show_person(device.people, person: who.key)
        }
    } else {
        ui.Button(label, icon: .eye, icon_only: not words, role: .bar, live: .polite) {
            on press: device.show_person(device.people, person: who.key)
        }
    }
}

/// What stands where the bar's glance value does: the time, unless this person follows someone or
/// their own input has just ended it — a view that is not their own is said where the clock
/// stands, so the clock keeps out of its way ([`follow_chip`]), folded on a `compact` bar.
view clock_or_follow(compact: Bool) {
    let board = device.people
    if board.following != "" or board.stopped != "" {
        follow_chip(compact)
    } else {
        clock()
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
