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
        ui.Button(
            "People",
            icon: .users,
            icon_only: true,
            role: .bar,
            size: if shell.bar_size == .small {
                .small
            } else if shell.bar_size == .large { .large } else { .regular },
        ) {
            on press: show_people()
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
            // How many here the faces cannot show, counted on the list they are taken from: a
            // count read off another list would claim people the bar was never given.
            let rest = device.people.rows.filter(p -> not p.agent).len() - shown.len()
            if rest > 0 {
                ui.Button(
                    label,
                    icon: .users,
                    count: rest,
                    icon_only: true,
                    role: .bar,
                    size: if shell.bar_size == .small {
                        .small
                    } else if shell.bar_size == .large { .large } else { .regular },
                ) {
                    on press: show_people()
                }
            } else {
                ui.Button(
                    label,
                    icon: .users,
                    icon_only: true,
                    role: .bar,
                    size: if shell.bar_size == .small {
                        .small
                    } else if shell.bar_size == .large { .large } else { .regular },
                ) {
                    on press: show_people()
                }
            }
        }
    }
}

/// One person here as their face on the bar: a press opens their card, and the mark on their face
/// says they follow this person. The mark is the presence face's own — a disc in their colour with
/// an eye on its edge — never a count's dot, which says something new waits behind a control.
view face_button(p: device.Person) {
    let name = if p.name != "" { p.name } else { std.say("Someone") }
    let label = if is_follower(p) { std.say("{name}, follows you") } else { name }
    if let face = face_of(p) {
        ui.Button(
            label,
            faces: [face],
            follows_you: is_follower(p),
            icon_only: true,
            role: .bar,
            size: if shell.bar_size == .small {
                .small
            } else if shell.bar_size == .large { .large } else { .regular },
        ) {
            on press: device.show_person(device.people, person: p.key)
        }
    } else {
        // The workspace lists nobody by that name (a session with no member rows yet): the
        // person's own name still says who it opens, and their card shows their face.
        ui.Button(
            label,
            icon: .user,
            icon_only: true,
            role: .bar,
            size: if shell.bar_size == .small {
                .small
            } else if shell.bar_size == .large { .large } else { .regular },
        ) {
            on press: device.show_person(device.people, person: p.key)
        }
    }
}

/// The actor a person the session lists is: the member their root key is, so a face wears one
/// colour whoever it stands beside, and someone nobody named has a face all the same. Software
/// whose row names no root is found by its own name, which is its identity.
fn face_of(p: device.Person) -> Option<Actor> {
    if p.agent {
        return face_named(p.name)
    }
    if p.user_key != "" {
        if let member = face_of_person(p.user_key) {
            return member
        }
    }
    return face_named(p.name)
}

/// The member `root` is here, where the workspace lists that person: by the root every device of
/// theirs acts for, never by a name, which two people may share and nobody named may lack.
fn face_of_person(root: Text) -> Option<Actor> {
    if root == "" {
        return .none
    }
    return workspace.members.people.find(m -> m.person() == root)
}

/// The member named `name` here: an agent, a bot, or a person the workspace named no root for.
/// Someone with no name has none to be found by, and is nobody rather than the first unnamed
/// member in the list.
fn face_named(name: Text) -> Option<Actor> {
    if name == "" {
        return .none
    }
    return workspace.members.people.find(m -> m.name() == name)
}

/// An agent here, as its presence: a chip that opens its menu. The menu says what it is doing and
/// where to find it, and stopping one ends the connection the person admitted.
view agent_chip(a: device.Agent) {
    let label = if a.unseen > 0 { "{a.name}, {a.unseen} new" } else { a.name }
    ui.MenuButton(
        label,
        icon: .bot,
        role: .bar,
        size: if shell.bar_size == .small {
            .small
        } else if shell.bar_size == .large { .large } else { .regular },
    ) {
        if a.doing != "" {
            ui.MenuItem(a.doing, disabled: true)
        }
        ui.MenuItem("Show who is here", icon: .users) {
            on choose: show_people()
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
    // The height of the room the panel stands in, before the panel takes its share: each
    // person's screen is sized from it.
    let tall = space.height * space.rem
    if space.compact {
        // A phone has room for one thing at a time: the roster is a sheet at the shell's foot,
        // between equal gutters, and a press above it puts it away. The surface fills the room
        // under the bar (`people(fill)`), so the sheet's layer runs to the foot and the sheet
        // stands at it, whatever it holds.
        layers(fill) {
            stack(at: .bottom, fill, padding: .related) {
                people_panel(board, tall, sheet: true)
            }
        }
    } else {
        row(fill, distribute: .end, padding: .related) {
            people_panel(board, tall, sheet: false)
        }
    }
}

/// The roster's panel: its header, the roster or a card, and nothing else. It holds the screen
/// while it shows, so the keyboard stays in it, and a press off it closes it.
view people_panel(board: device.People, tall: Float, sheet: Bool) {
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
                // Kept on screen it waits at an edge of the shell rather than in the bar: never
                // from a phone's sheet, where it would stand over the one window there is.
                if not sheet {
                    ui.Button(
                        "Keep on screen",
                        icon: .pin,
                        icon_only: true,
                        size: .small,
                        role: .plain,
                    ) {
                        on press: keep_on_screen(true)
                    }
                }
                ui.Button("Close", icon: .x, icon_only: true, size: .small, role: .plain) {
                    on press: device.close(device.people)
                }
            }
        }
        body: {
            if board.showing != "" {
                person_card(board)
            } else {
                roster(board, tall)
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
view roster(board: device.People, tall: Float) {
    // The person at this device stands first in their own roster: their row opens their own
    // card, where what is theirs to change is.
    ui.ListRow(
        "You",
        secondary: availability_word(board.me),
        mark: if board.following != "" { .eye } else { .user },
    ) {
        leading: {
            // The same face the bar draws for them: a member's face is theirs, in one colour
            // wherever it stands, and the roster's faces form one leading column.
            if let me = face_of(board.me) {
                ui.Avatar(me, colour: board.me.colour)
            } else {
                ui.Avatar(name: board.me.name, colour: board.me.colour)
            }
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
        // Each person's screen stands under their row in the room the panel has for them all:
        // half the display's height, shared among the people here (software has no screen), in
        // the screen's own shape, and none at all where that leaves too little for a picture.
        let room = tall * 0.5 - 48.0
        let each = room / if everyone.len() > 0 { everyone.len() } else { 1 }.to_float()
        let natural = mirror_height(if space.compact { space.width * space.rem } else { 400.0 })
        let mirror = if each < 64.0 { 0.0 } else if each < natural { each } else { natural }
        // As tall as the people here, up to the room a roster takes: past it the list scrolls
        // and builds only the rows it shows.
        ui.VirtualList(name: "People here now", most: space.rem * 34.0) {
            for p in everyone {
                person_listed(p, board, if labelled { "People" } else { "" }, mirror)
            }
            for p in software {
                person_listed(p, board, if labelled { "Agents" } else { "" }, mirror)
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
view person_listed(p: device.Person, board: device.People, group: Text, mirror: Float) {
    let label = if p.name != "" { p.name } else { std.say("Someone") }
    let followed = p.following or board.following == p.key
    ui.ListItem(value: p.key, group: if group != "" { group }) {
        person_row(p, board, mirror)
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
    // Who they are as software stands as the row's tag; when someone nobody named joined stands on
    // the row's line under the name, and is said here too: a row's description is what a reader
    // hears in place of the lines it draws.
    let said: List<Text> = []
    if p.joined != "" {
        said = said + [std.say("Joined {p.joined}")]
    }
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
/// in, with the way there; software is tagged as what it is, and nobody waves at it. Under a
/// person's row stands their screen, `mirror` tall, or the line that says why it does not: they
/// keep where they are private, or nothing stands open there.
view person_row(p: device.Person, board: device.People, mirror: Float) {
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
        leading: {
            // Their face in the colour the workspace gives them: the same face the bar draws,
            // ring and all, in the roster's one leading column of faces.
            if let face = face_of(p) {
                ui.Avatar(face, colour: p.colour)
            } else if p.agent {
                ui.Avatar(name: p.name, colour: p.colour, square: true, glyph: .bot)
            } else {
                ui.Avatar(name: p.name, colour: p.colour)
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
                // Where software works: pressing it brings that window forward, so what it is on
                // stands in front of this person.
                if p.agent and goable(p) {
                    ui.Button(
                        "Go there",
                        icon: .arrow_right,
                        icon_only: true,
                        size: .small,
                        role: .plain,
                        description: std.say("Go where {name} works"),
                    ) {
                        on press: device.go_to(device.people, person: p.key)
                    }
                }
            }
        }
        below: {
            // Their screen, or the line that says why it is not shown, is part of their row: it
            // starts in the name's column and moves, hovers and is read with the row.
            if not p.agent and mirror > 0.0 {
                screen_under(p, mirror, followed)
            }
        }
        on activate: device.show_person(device.people, person: p.key)
    }
}

/// How tall a picture of someone's screen stands under their row in a roster `wide` across: a
/// screen's usual shape, inside the row's margins.
fn mirror_height(wide: Float) -> Float {
    (wide - 32.0) / 1.6
}

/// A person's screen under their row: a picture of where they are, their windows where they stand
/// and the one they work in ringed in their colour and named, or one quiet line saying why there
/// is none. `followed`: the person at this device follows them, and their colour runs the picture's
/// height.
view screen_under(p: device.Person, mirror: Float, followed: Bool) {
    if let screen = p.screen {
        if screen.windows.is_empty() {
            ui.Text("Nothing open on their screen", role: .caption, secondary: true)
        } else {
            let name = if p.name != "" { p.name } else { std.say("Someone") }
            ui.ScreenMirror(
                std.say("{name}'s screen"),
                screen_width: screen.width,
                screen_height: screen.height,
                colour: p.colour,
                height: mirror,
                followed: followed,
            ) {
                for w in screen.windows {
                    ui.ScreenMirrorWindow(
                        w.name,
                        app: w.app,
                        left: w.left,
                        top: w.top,
                        right: w.right,
                        bottom: w.bottom,
                        focused: w.focused,
                    )
                }
            }
        }
    } else {
        ui.Text("Keeps their place private", role: .caption, secondary: true)
    }
}

/// Everyone else's pointer over the window of place `place`, in their colour and under their
/// name, with what they say beside it: drawn over the window whatever the roster shows. While the
/// roster is open, each says where its person is, by the window it is over, as the roster's own
/// words for who is here.
view pointers_over(place: Text, title: Text) {
    let over = device.people.pointers.filter(x -> x.tab == place)
    let listed = device.overlays.here == "people" and device.people.showing == ""
    if not over.is_empty() {
        ui.PeerPointers(place, fill) {
            for x in over {
                let who = x.name
                ui.PeerPointer(
                    if not listed {
                        ""
                    } else if title != "" { std.say("{who} is in {title}") } else { who },
                    person: x.person,
                    name: x.name,
                    colour: x.colour,
                    x: x.x,
                    y: x.y,
                    agent: x.agent,
                    said: x.said,
                    typing: x.typing,
                    reaction: x.reaction,
                    reaction_seq: x.reaction_seq,
                )
            }
        }
    }
}

/// The colour of the person this device follows, as the roster lists them: what the frame round
/// the work is drawn in. Empty while it follows nobody, or nobody here.
fn followed_colour() -> Text {
    let board = device.people
    if board.following == "" {
        return ""
    }
    if let found = board.rows.find(r -> r.key == board.following) {
        return found.colour
    }
    return ""
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
            } else if let face = face_of(board.me) {
                // Their own card draws the face the bar and the roster draw of them, picture and
                // all, not a second spelling of their initials.
                ui.Avatar(face, colour: me.colour, status: presence_status(me))
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
                ui.Button(
                    "Stop",
                    icon: .square,
                    icon_only: folded,
                    size: .small,
                    role: .plain,
                    accessible_name: std.say("Stop following {name}"),
                ) {
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
        ui.Button(
            label,
            faces: [face],
            icon_only: not words,
            role: .bar,
            size: if shell.bar_size == .small {
                .small
            } else if shell.bar_size == .large { .large } else { .regular },
            live: .polite,
        ) {
            on press: device.show_person(device.people, person: who.key)
        }
    } else {
        ui.Button(
            label,
            icon: .eye,
            icon_only: not words,
            role: .bar,
            size: if shell.bar_size == .small {
                .small
            } else if shell.bar_size == .large { .large } else { .regular },
            live: .polite,
        ) {
            on press: device.show_person(device.people, person: who.key)
        }
    }
}

/// What stands where the bar's glance value does: whose view this person follows, the news about
/// people (who arrived, who waved) beside the time, or the time alone. The news stands where
/// nothing a person aims at does, beside the clock rather than over it: the faces and every
/// control keep their places, and the time stays in view while the bar says who came. A compact
/// bar has room for one, and the news takes the time's place there.
view clock_or_news(compact: Bool) {
    let board = device.people
    if board.following != "" or board.stopped != "" {
        follow_chip(compact)
    } else if board.arrived_seen != "" or not board.arrived.is_empty() {
        // The news stands beside the time where the bar has room for both, and takes the time's
        // place on a compact bar, which has room for one.
        if compact {
            arrival_notice()
        } else {
            row(gap: .related, align: .center) {
                clock()
                arrival_notice()
            }
        }
    } else if board.waved != "" {
        if compact {
            wave_notice()
        } else {
            row(gap: .related, align: .center) {
                clock()
                wave_notice()
            }
        }
    } else if board.waved_to != "" {
        if compact {
            waved_notice()
        } else {
            row(gap: .related, align: .center) {
                clock()
                waved_notice()
            }
        }
    } else {
        clock()
    }
}

/// A wave this person sent went: who it went to, in the bar's glance, for as long as their row
/// says it went. The person sees their own step answered where they took it, without opening
/// anyone's card.
view waved_notice() {
    let board = device.people
    if board.waved_to != "" {
        if let who = board.rows.find(p -> p.key == board.waved_to) {
            let name = if who.name != "" { who.name } else { std.say("Someone") }
            let label = std.say("You waved at {name}")
            if let face = face_of(who) {
                ui.Chip(label, faces: [face], tone: .success, live: .polite)
            } else {
                ui.Chip(label, icon: .hand, tone: .success, live: .polite)
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
        ui.MenuButton(
            label,
            icon: .screen_share,
            role: .bar,
            size: if shell.bar_size == .small {
                .small
            } else if shell.bar_size == .large { .large } else { .regular },
        ) {
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

/// Someone waved at this person: who, in the bar's glance, for a moment. The session clears it when
/// its moment is over, so the bar never keeps a word that has gone stale. It is news, so a reader
/// hears it without having to be looking at the bar, and it carries the waver's face.
view wave_notice() {
    let board = device.people
    if board.waved != "" {
        let name = board.waved
        let label = std.say("{name} waved at you")
        if let face = face_named(name) {
            ui.Chip(label, faces: [face], tone: .info, live: .polite)
        } else {
            ui.Chip(label, icon: .hand, tone: .info, live: .polite)
        }
    }
}

/// Someone arrived where this person is: the first time in a public workspace, what its people
/// see of the person instead, opening their own card where they choose otherwise. The session
/// clears it when its moment is over.
view arrival_notice() {
    let board = device.people
    if board.arrived_seen != "" {
        ui.Button(board.arrived_seen, icon: .eye, size: .small, role: .plain, live: .polite) {
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
        // Whoever nobody named reads as "Someone" here, as a roster row names them: their face is
        // still theirs, found by the person they are and not by the name they arrived with.
        let arriving = board.arrived_keys.first() ?? ""
        if let face = face_arriving(board, arriving) {
            ui.Chip(label, faces: [face], tone: .success, live: .polite) {
                on press: device.show(device.people)
            }
        } else {
            ui.Chip(label, icon: .users, tone: .success, live: .polite) {
                on press: device.show(device.people)
            }
        }
    }
}

/// A face for whoever arrived with the presence key `key`: their row here, and the member it is.
fn face_arriving(board: device.People, key: Text) -> Option<Actor> {
    if key == "" {
        return .none
    }
    if let row = board.rows.find(r -> r.key == key) {
        return face_of(row)
    }
    return .none
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

/// Who is here, kept on screen: the roster waiting at the edge of the shell the person keeps it at
/// (`settings.widgets.people_overlay`), so it comes back there after a restart. It waits away while
/// nobody else is here, and comes in when someone arrives; it stands aside while the bar's own
/// roster is up. Dragged by its header and let go, it moves to the edge nearest the pointer, and
/// its button puts it back in the bar. It comes and goes on the shared motion, where it stands.
/// Titled with the workspace it is of, it shows each person with a picture of their screen.
view kept_people() {
    let board = device.people
    let kept = settings.widgets.people_overlay
    let named = front_workspace_name()
    // The band under the bar is the room its edges are read in: it fills the band, and stands
    // at the edge the person keeps it at.
    stack(fill) {
        ui.FloatingPanel(
            width: 400.0,
            label: "People",
            description: std.say("{named}. Who is here, where they are and what they are doing"),
            edge: if kept.edge == .left {
                .left
            } else if kept.edge == .bottom {
                .bottom
            } else if kept.edge == .bottom_left {
                .bottom_left
            } else if kept.edge == .bottom_right { .bottom_right } else { .right },
            shown: kept.pinned and not board.rows.is_empty() and device.overlays.here != "people",
        ) {
            on docked(edge): move_people(edge)
            header: {
                row(gap: .related, align: .center) {
                    ui.TextStack(named, secondary: here_line(board.rows.len()))
                    spacer
                    ui.Button(
                        "Put back in the bar",
                        icon: .x,
                        icon_only: true,
                        size: .small,
                        role: .plain,
                    ) {
                        on press: keep_on_screen(false)
                    }
                }
            }
            body: {
                // The roster's own list: it scrolls past the room it takes and builds only the
                // rows it shows, so a workspace of thousands costs a kept roster what it costs
                // the bar's.
                let everyone = board.rows.filter(p -> not p.agent)
                let software = board.rows.filter(p -> p.agent)
                let labelled = not everyone.is_empty() and not software.is_empty()
                ui.VirtualList(name: "Who is here", most: space.rem * 34.0) {
                    for p in everyone {
                        person_listed(
                            p,
                            board,
                            if labelled { "People" } else { "" },
                            mirror_height(400.0),
                        )
                    }
                    for p in software {
                        person_listed(
                            p,
                            board,
                            if labelled { "Agents" } else { "" },
                            mirror_height(400.0),
                        )
                    }
                }
                if board.may_invite {
                    row(gap: .related, align: .center) {
                        ui.Button("Invite people", icon: .user_plus, size: .small) {
                            on press: device.show_invite(device.invite)
                        }
                    }
                } else {
                    ui.Text(
                        "You can't invite people to this workspace. Its owner decides who joins.",
                        role: .caption,
                        secondary: true,
                    )
                }
            }
        }
    }
}

/// Whether who is here stands kept on screen now: kept, and someone else here to show.
fn people_kept() -> Bool {
    settings.widgets.people_overlay.pinned and not device.people.rows.is_empty()
}

/// What the bar's faces do: open who is here. While the roster stands kept on screen it is already
/// where they would open it, and they have nothing to do.
action show_people() (label: "Show who is here") {
    if not people_kept() { device.show(device.people) }
}

/// Keep who is here on screen at the edge the person last kept it at, or put it back in the bar.
/// Kept, the bar's own roster closes: what the person sees from then on is the roster at its edge.
action keep_on_screen(kept: Bool) (label: "Keep who is here on screen") {
    settings.keep_people(
        settings.widgets,
        kept: kept,
        edge: edge_key(settings.widgets.people_overlay.edge),
    )
    if kept { device.close(device.people) }
}

/// Move who is here, kept on screen, to the edge `edge`, as a drag of it let go there.
action move_people(edge: Text (label: "Edge")) (label: "Move who is here") {
    settings.keep_people(settings.widgets, kept: true, edge: edge)
}

/// The name of the edge the roster is kept at, as the settings' action takes it.
fn edge_key(edge: settings.DockEdge) -> Text {
    if edge == .left {
        "left"
    } else if edge == .bottom {
        "bottom"
    } else if edge == .bottom_left {
        "bottom_left"
    } else if edge == .bottom_right { "bottom_right" } else { "right" }
}
