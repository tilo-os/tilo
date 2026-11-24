app so.tilo.discord "Discord" (icon: .discord)

/// The server the app of old bridged, as it kept it: its settings, the joinings of both sides,
/// the linked accounts, the blocked ones and the channels it observed.
type Accounts {
    guild_id: Text = ""
    name: Text = ""
    only_channel: Text = ""
    mirror_channels: Bool = false
    map_roles: Bool = false
    ownership: Text = ""
    bot_user: Text = ""
    sign_in_page: Text = ""
    map: Collection<OldMapping>
    links: Collection<OldLink>
    channels: Collection<OldChannel>
    blocked_personas: Set<Text>
}

root accounts: Accounts

default view before_account for Accounts
view before_account(accounts: Accounts) {
    ui.Text(accounts.name, role: .body)
}

type OldMapping {
    kind: Text
    tilo: Text
    discord: Text
    origin: Text = "tilo"
    persona: Text = ""
    root: Text = ""
    picture: Text = ""
}

type OldLink {
    discord_user: Text
    person: Text
    linked_at: Int = 0
    user_name: Text = ""
    display_name: Text = ""
    avatar: Text = ""
    interaction: Text = ""
    photo: Text = ""
    photo_avatar: Text = ""
}

type OldChannel {
    id: Text
    kind: Text
    name: Text
    parent: Text = ""
    position: Int = 0
    topic: Text = ""
}

scenario "the Lounge's server" {
    accounts.guild_id = "123"
    accounts.name = "The Lounge"
    accounts.only_channel = "456"
    accounts.mirror_channels = true
    accounts.map_roles = true
    accounts.ownership = "adopted"
    accounts.bot_user = "900"
    accounts.blocked_personas.add("3002")
    accounts.channels.insert(OldChannel(id: "456", kind: "text", name: "general"))
    accounts.channels.insert(OldChannel(id: "789", kind: "voice", name: "Lounge"))
    accounts.map.insert(OldMapping(kind: "channel", tilo: "tl:so.tilo.chat:c1", discord: "456"))
    accounts.map.insert(OldMapping(kind: "channel", tilo: "tl:so.tilo.chat:c2", discord: "789"))
    accounts.map.insert(OldMapping(kind: "webhook", tilo: "tl:so.tilo.chat:c1", discord: "555"))
    accounts.map.insert(OldMapping(kind: "message", tilo: "tl:so.tilo.chat:m1", discord: "777", origin: "discord", persona: "3001"))
    accounts.links.insert(OldLink(discord_user: "3001", person: "ada", linked_at: 1700000000000, user_name: "ada", display_name: "Ada", avatar: "abc"))
}
