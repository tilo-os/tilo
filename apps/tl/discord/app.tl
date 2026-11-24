app so.tilo.discord.preview "Discord" (
    icon: .discord,
    category: .network,
    tagline: "Discord servers in the workspace's conversations",
    keywords: ["discord", "chat", "server", "channels"],
)

use tilo.workspace as workspace

/// A connected Discord account: the server it bridges, what the bridge knows of both sides, and
/// who here linked their Discord account. A server's categories are Chat's categories and its
/// channels Chat's channels, so the sidebar lists them as any other conversation; this holds
/// what joins the two sides.
// Discord as one of Chat's connected accounts: a Discord account's server, its channels and
// its direct messages are conversations here, and what people say in a bridged one is said
// there too. The bridge is this app's origin (`discord.perform`): it carries each queued call to
// Discord as the person it maps, and writes what Discord reports back through these actions.
type Discord (origin: discord.account) {
    /// What the account is called on Discord.
    name: Text
    /// The server bridged, by its Discord id; empty until one is connected.
    guild_id: Text = ""
    /// The server's name, as Discord shows it.
    server: Text = ""
    /// The one channel bridged, by its Discord id; empty for the whole server.
    only_channel: Text = ""
    /// Channels made here after the server was adopted are made in Discord too.
    mirror_channels: Bool = false
    /// The workspace's roles are kept as roles the bridge owns in Discord.
    map_roles: Bool = false
    /// Whether the server is still being brought in, or bridged both ways.
    ownership: Ownership = .importing
    /// The secret item holding the bot token on this device; the token never enters the resource.
    token_item: Text = ""
    /// The bot user's Discord id: its own messages and reactions are echoes.
    bot_user: Text = ""
    /// Discord's channels and categories as the bot last observed them.
    channels: Collection<DChannel>
    /// Discord's roles as the bot last observed them.
    roles: Collection<DRole>
    /// The server's custom emoji, as the bot last observed them.
    emojis: Collection<DEmoji>
    /// What joins a conversation here and a channel, category, thread, message, webhook, role or
    /// emoji there.
    map: Collection<Mapping>
    /// The Discord accounts people here linked.
    links: Collection<Link>
    /// Discord accounts offered for linking, each to whoever holds its one-time token.
    link_offers: Collection<LinkOffer>
    /// People on their way through Discord's consent page, by person.
    connects: Collection<Connect>
    /// Discord's consent page for signing in through this device, without its state: empty when
    /// the device offers none.
    sign_in_page: Text = ""
    /// Where a sign-in the person just started sends them, while it waits for what comes back.
    sign_in_url: Text = ""
    /// A Discord avatar's hash and the address its image is kept at.
    avatars: Map<Text>
    /// Discord accounts whose messages are not mirrored.
    blocked_personas: Set<Text>
    /// Mapped Discord channels and roles seen once and gone from the latest layout.
    vanished: Set<Text>
    /// Conversations here that stay here: unmatched when the server was adopted, or their
    /// Discord side was deleted.
    tilo_only: Set<Text>
    /// Discord channels and categories that stay there: their side here was deleted.
    discord_only: Set<Text>
    /// Why the bridge cannot carry a voice channel's call now, as Discord refused it; empty while
    /// it can.
    voice_problem: Text = ""
    /// Why the workspace's emoji do not reach the server now; empty while they do.
    emoji_problem: Text = ""
    /// Whether the setup dialog is open.
    setup_open: Bool = false
    /// The servers the bot is in, as finding them answered.
    setup_servers: Collection<ServerChoice>
    /// Why finding them did not work, in words the person reads.
    setup_problem: Text = ""
}

/// Whether the server is still being brought in, or bridged both ways.
enum Ownership {
    importing
    adopted
}

/// What a mapping joins.
enum MappedKind {
    channel
    category
    thread
    message
    webhook
    role
    emoji
}

/// Which side a mapped object was made on.
enum OriginSide {
    tilo
    discord
}

/// A Discord channel, thread or category as the bridge last observed it.
type DChannel {
    id: Text
    kind: ChannelKind
    name: Text
    parent: Option<Text> = .none
    position: Int = 0
    topic: Text = ""
    overwrites: List<Overwrite>
}

/// What kind of place a Discord channel is.
enum ChannelKind {
    text
    voice
    category
    thread
}

/// Who a channel's permission overwrite is for, and what it allows and denies.
type Overwrite {
    id: Text
    /// 0 for a role, 1 for a member, as Discord numbers them.
    kind: Int = 0
    allow: Text = ""
    deny: Text = ""
}

/// A Discord role as the bridge last observed it.
type DRole {
    id: Text
    name: Text
    permissions: Text = ""
    color: Int = 0
    position: Int = 0
    managed: Bool = false
}

/// A Discord custom emoji.
type DEmoji {
    id: Text
    name: Text
    animated: Bool = false
}

/// What joins a conversation here to its Discord side.
type Mapping {
    kind: MappedKind
    /// The conversation's, message's or role's published id here.
    tilo: Text
    /// The Discord id it is carried to.
    discord: Text
    origin: OriginSide = .tilo
    /// For a message Discord wrote: its author's account there.
    persona: Text = ""
    /// For a reply posted in a thread here: its thread's root, as Discord threads are one level.
    root: Text = ""
    /// For an emoji the bridge uploaded: the address of its picture.
    picture: Text = ""
    /// For a message carried there: what it last said there, so a change here is carried once.
    said: Text = ""
    /// For a message carried there: the emoji the bot put on it there, each once.
    emoji: Set<Text>
}

/// A Discord account someone here linked.
type Link {
    discord_user: Text
    /// The person here it is linked to.
    person: Text
    /// When it was linked, as ISO text.
    linked_at: Text = ""
    user_name: Text = ""
    /// The name the account shows when that is another.
    display_name: Text = ""
    /// The account's Discord avatar hash.
    avatar: Text = ""
    /// The slash command's reply that says what came of the link, when it was made from there.
    interaction: Text = ""
    /// The avatar as a small picture, which the person's profile shows until they upload one.
    photo: Text = ""
    /// The avatar hash `photo` is of.
    photo_avatar: Text = ""
}

/// A Discord account offered for linking, to whoever holds the one-time token it was offered
/// with: only the token's check is kept.
type LinkOffer {
    check: Text
    discord_user: Text
    user_name: Text = ""
    display_name: Text = ""
    avatar: Text = ""
    /// The person who started it here: only they may take it.
    person: Text = ""
    /// The slash command it answers.
    interaction: Text = ""
    /// When it runs out, as ISO text.
    expires: Text = ""
}

/// Someone on their way through Discord's consent page: the round trip's state, hashed, binds
/// what comes back to them.
type Connect {
    person: Text
    state_sha256: Text
    /// When it runs out, as ISO text.
    expires: Text = ""
}

/// A server the bot is in.
type ServerChoice {
    id: Text
    name: Text
}

/// The servers finding them answered.
type ServerList {
    servers: List<ServerChoice>
}

/// A message through a channel's webhook, under the person's Discord name.
// ---- what goes to Discord ---------------------------------------------------------------
type SendOp {
    op: Text = "send"
    channel: Text
    webhook: Text
    /// The thread it goes in, when the conversation is one; none for a channel or a direct
    /// conversation.
    thread: Option<Text> = .none
    username: Text
    avatar_url: Option<Text> = .none
    /// The avatar this message's author wears on Discord, by the hash of its bytes there.
    avatar_blob: Option<Text> = .none
    content: Text
    attachments: List<OutAttachment>
    created_ms: Int = 0
    /// The message here it carries, by its published id.
    tilo: Text
}

/// A file a message carries, by the bare blake3 hex of its bytes, as Discord's worker reads it.
type OutAttachment {
    blob: Text
    name: Text
    size: Int = 0
}

/// A change to a message the bridge sent.
type EditOp {
    op: Text = "edit"
    channel: Text
    webhook: Text
    message: Text
    thread: Option<Text> = .none
    content: Text
}

/// A message deleted there.
type DeleteOp {
    op: Text = "delete"
    channel: Text
    webhook: Option<Text> = .none
    message: Text
    thread: Option<Text> = .none
}

/// The bot's reaction added there, or taken back.
type ReactOp {
    op: Text = "react"
    channel: Text
    message: Text
    emoji: Text
}

/// A thread started on a message.
type StartThreadOp {
    op: Text = "start-thread"
    channel: Text
    message: Text
    name: Text
    tilo: Text
}

/// A channel's webhook made there.
type CreateWebhookOp {
    op: Text = "create-webhook"
    channel: Text
    tilo: Text
}

/// A channel made there, or brought back in line.
type ChannelSpec {
    op: Text = "apply-channel"
    /// The Discord channel it is, when it is known; none for a channel made afresh.
    id: Option<Text> = .none
    tilo: Text
    kind: ChannelKind = .text
    name: Text
    topic: Text = ""
    parent: Option<Text> = .none
    position: Int = 0
}

/// A channel deleted there.
type DeleteChannelOp {
    op: Text = "delete-channel"
    channel: Text
}

/// Where a category goes.
type CategoryPlace {
    category: Text
    position: Int
}

/// Where a channel goes: under `parent` (none for the top level), at `position`.
type ChannelPlace {
    channel: Text
    parent: Option<Text> = .none
    position: Int
}

/// Channels and categories put in their places at once.
type ArrangeOp {
    op: Text = "arrange-channels"
    categories: List<CategoryPlace>
    channels: List<ChannelPlace>
}

/// A role made there, or brought back in line.
type RoleSpec {
    op: Text = "apply-role"
    id: Option<Text> = .none
    tilo: Text
    name: Text
    permissions: Text = ""
    color: Int = 0
}

/// A role deleted there.
type DeleteRoleOp {
    op: Text = "delete-role"
    role: Text
}

/// A linked member's roles set there.
type MemberRolesOp {
    op: Text = "set-member-roles"
    user: Text
    add: List<Text>
    remove: List<Text>
}

/// A linked member kicked, banned or unbanned there.
type MemberOp {
    op: Text = "kick"
    user: Text
    reason: Text = ""
}

/// A channel's permission overwrite set there.
type OverwriteOp {
    op: Text = "set-overwrite"
    channel: Text
    target: Text
    /// Whose overwrite it is: a role's or a member's, as Discord numbers them.
    target_kind: OverwriteTarget = .role
    allow: Text = ""
    deny: Text = ""
}

/// Whose permission overwrite an op sets.
enum OverwriteTarget {
    role
    member
}

/// A file copied from Discord's CDN into the workspace's content store.
type FetchAssetOp {
    op: Text = "fetch-asset"
    url: Text = ""
    max_bytes: Int = 0
    /// Whether the answer carries the picture itself, as a `data:` address, for a record to keep
    /// (an avatar, a link's photo), rather than its bytes reaching the host to store.
    inline: Bool = false
}

/// A slash command's reply edited to what came of it.
type AnswerInteractionOp {
    op: Text = "answer-interaction"
    interaction: Text
    content: Text
}

/// A workspace emoji uploaded to the server, or renamed there.
type EmojiSpec {
    op: Text = "apply-emoji"
    id: Option<Text> = .none
    name: Text
    picture: Text = ""
    tilo: Text
}

/// An emoji the bridge made deleted there.
type DeleteEmojiOp {
    op: Text = "delete-emoji"
    emoji: Text
    tilo: Text
}

/// What the bridge carries out: which account's server, and the call, as JSON.
type Perform {
    account: Text
    op: Text
}

/// The servers this app bridges: one account per connected Discord server.
type Accounts {
    accounts: Collection<Discord>
}

root accounts: Accounts

/// Whether `who` has the workspace's moderating role.
fn moderates(who: Actor) -> Bool {
    let role = who.role() ?? ""
    role == "moderator" or role == "owner"
}

/// Whether `who` is the bridge's own program, carrying the workspace's changes to Discord: the
/// app's process the daemon attested, which the invoker names in `app`.
fn the_bridge(who: Actor) -> Bool {
    (who.app() ?? "") == "so.tilo.discord.preview"
}

/// Whether `who` may change the bridge's own setup and server on Discord: the workspace's
/// moderators, or the bridge itself as it carries the workspace's changes there.
fn may_configure(who: Actor) -> Bool {
    the_bridge(who) or moderates(who)
}

// ---- the page ---------------------------------------------------------------------------

/// The bridge's window: the connected server, or the empty state that connects one.
default view accounts_view for Accounts
view accounts_view(accounts: Accounts) {
    if let d = accounts.accounts.first() {
        account(d)
    } else {
        ui.EmptyState(
            "Bring a Discord server into this workspace",
            icon: .message_circle,
            message: "Its channels become conversations here, and what people say on either side shows on both.",
        )
        std.add_entry("Connect Discord", begin_setup(accounts))
    }
}

/// Starts the account: the record the setup writes into.
action begin_setup(accounts: Accounts) -> Discord (icon: .plug, label: "Connect Discord") {
    require moderates(invoker) "Only the workspace's moderators connect a Discord server"
    accounts.accounts.insert(Discord(name: "Discord", setup_open: true))
}

/// One Discord account's page: what the bridge knows, the people here who linked an account,
/// the server's channels in Discord's order, and the setup sheet while it is open.
view account(d: Discord) {
    if d.guild_id == "" {
        ui.EmptyState(
            "Bring a Discord server into this workspace",
            icon: .message_circle,
            message: "Its channels become conversations here, and what people say on either side shows on both.",
        )
        std.add_entry("Connect Discord", open_setup(d))
    } else {
        ui.NavShell(content_kind: .page, sidebar_title: "Discord") {
            sidebar: {
                ui.NavList(selected: "") {
                    ui.NavSection(d.server) {
                        for ch in ordered_channels(d) {
                            if ch.kind == .category {
                                if ch.name != "" {
                                    ui.NavItem("/discord/{ch.id}", ch.name)
                                }
                            } else if ch.kind == .voice {
                                ui.NavItem("/discord/{ch.id}", ch.name, icon: .volume_2)
                            } else {
                                ui.NavItem("/discord/{ch.id}", ch.name, icon: .hash)
                            }
                        }
                    }
                    if not d.links.is_empty() {
                        ui.NavSection("People", collapsible: true) {
                            for l in d.links {
                                ui.NavItem(
                                    "/discord/link/{l.discord_user}",
                                    shown_name(l),
                                    person: person_of(l, invoker),
                                )
                            }
                        }
                    }
                }
            }
            content: {
                stack(gap: .group) {
                    ui.Section(d.server) {
                        ui.ListRow(
                            if d.ownership == .importing { "Being brought in" } else { "Bridged" },
                            secondary: "What people say on either side shows on both.",
                            icon: if d.ownership == .importing { .clock } else { .circle_check },
                        )
                        ui.ListRow(
                            how_many(
                                d.channels.filter(c -> c.kind != .category).len(),
                                "channel",
                                "channels",
                            ),
                            secondary: how_many(d.links.len(), "linked account", "linked accounts"),
                            icon: .hash,
                        )
                    }
                    if d.voice_problem != "" {
                        ui.Section("Calls") {
                            ui.Text(d.voice_problem, role: .body)
                        }
                    }
                    if d.emoji_problem != "" {
                        ui.Section("Emoji") {
                            ui.Text(d.emoji_problem, role: .body)
                        }
                    }
                    ui.Section("People") {
                        if let mine = my_link(d, invoker) {
                            ui.ListRow(
                                "You: {shown_name(mine)}",
                                secondary: "What you say on Discord shows as you.",
                                icon: .circle_check,
                            )
                            ui.ListRow("Unlink your Discord account") {
                                trailing: {
                                    button(unlink_mine(d))
                                }
                            }
                        } else if d.sign_in_page != "" {
                            ui.ListRow(
                                "Link your Discord account",
                                secondary: "What you say there then shows as you.",
                            ) {
                                trailing: {
                                    button(connect_account(d))
                                }
                            }
                        } else {
                            ui.ListRow(
                                "Link your Discord account from the server with /link",
                                secondary: "What you say there then shows as you.",
                            )
                        }
                        for l in d.links.filter(l -> l.person != invoker.id()) {
                            ui.ListRow(shown_name(l), secondary: "Linked", icon: .user)
                        }
                    }
                    ui.Section("The server") {
                        for ch in ordered_channels(d) {
                            if ch.kind == .category {
                                ui.Text(ch.name, role: .label)
                            } else if ch.kind == .voice {
                                ui.ListRow(ch.name, icon: .volume_2)
                            } else {
                                ui.ListRow(ch.name, secondary: ch.topic, icon: .hash)
                            }
                        }
                    }
                    std.after_body {
                        std.history(d)
                    }
                }
            }
        }
    }
    if d.setup_open {
        setup_dialog(d)
    }
}

/// The setup dialog: the bot's token, the servers it is in, and the one to bring in.
view setup_dialog(d: Discord) {
    state token: Text = ""
    ui.ModalDialog(title: "Connect a Discord server", open: true, secondary: "Cancel") {
        body: {
            if not d.setup_servers.is_empty() {
                ui.Text("Choose the server to bring in.", role: .body)
                for s in d.setup_servers {
                    ui.ListRow(s.name, secondary: s.id) {
                        trailing: {
                            button(bridge_server(d, s, token))
                        }
                    }
                }
            } else {
                ui.Text(
                    "The bot's token is kept on this device, never in the workspace's records.",
                    role: .caption,
                    secondary: true,
                )
                ui.TextInput(
                    bind: token,
                    placeholder: "The bot's token",
                    secret: true,
                    autofocus: true,
                )
                if d.setup_problem != "" {
                    ui.Text(d.setup_problem, role: .body)
                }
                ui.ListRow("Find the bot's servers") {
                    trailing: {
                        button(find_servers(d, token))
                    }
                }
            }
        }
        on act_secondary: close_setup(d)
        on dismiss: close_setup(d)
    }
}

/// What the account calls a linked account: the name it shows, else its Discord user name.
// ---- helpers ----------------------------------------------------------------------------

/// Whether an op names no Discord object: one that is made afresh.
fn fresh(value: Option<Text>) -> Bool {
    match value {
        .none => true,
        .some(_) => false,
    }
}

/// An option's text, when it has one: what a guard reads an op's absent id or parent as.
fn or_empty(value: Option<Text>) -> Text {
    match value {
        .some(text) => text,
        .none => "",
    }
}

fn shown_name(l: Link) -> Text {
    if l.display_name != "" { l.display_name } else { l.user_name }
}

/// How many of something, in words.
fn how_many(n: Int, one: Text, many: Text) -> Text {
    if n == 1 { "1 {one}" } else { "{n} {many}" }
}

/// The person here a link is with, `me` when the workspace no longer holds them.
fn person_of(l: Link, me: Actor) -> Actor {
    if let p = workspace.members.people.find(p -> p.id() == l.person) { p } else { me }
}

/// The link of the person `me`, when they linked an account.
fn my_link(d: Discord, me: Actor) -> Option<Link> {
    d.links.find(l -> l.person == me.id())
}

/// The mapping of `tilo` as `kind`.
fn mapping(d: Discord, kind: MappedKind, tilo: Text) -> Option<Mapping> {
    d.map.find(m -> m.kind == kind and m.tilo == tilo)
}

/// The mapping of the Discord `discord` as `kind`.
fn by_discord(d: Discord, kind: MappedKind, discord: Text) -> Option<Mapping> {
    d.map.find(m -> m.kind == kind and m.discord == discord)
}

/// The Discord channel `id`, as the bridge last observed it.
fn observed(d: Discord, id: Text) -> Option<DChannel> {
    d.channels.find(c -> c.id == id)
}

/// Whether the bridge knows `id`: a channel it observed, or one it made since.
fn knows_channel(d: Discord, id: Text) -> Bool {
    observed(d, id) != .none
        or d.map.any(
            m -> (m.kind == .channel or m.kind == .category or m.kind == .thread)
                and m.discord == id,
        )
}

/// The account's channels in Discord's order: each category, then its channels, then the
/// channels outside any.
fn ordered_channels(d: Discord) -> List<DChannel> {
    let categories = d.channels.filter(c -> c.kind == .category).sort_by(c -> c.position)
    let rest = d.channels.filter(c -> c.kind != .category).sort_by(c -> c.position)
    let rows: List<DChannel> = []
    for cat in categories {
        rows = rows + [cat]
        for ch in rest.filter(ch -> ch.parent == .some(cat.id)) {
            rows = rows + [ch]
        }
    }
    let loose = rest.filter(
        ch -> not d.channels.any(c -> c.kind == .category and ch.parent == .some(c.id)),
    )
    rows + loose
}

/// A server or channel id: digits.
fn is_snowflake(id: Text) -> Bool {
    id != "" and digits_at(id, 0)
}

/// Whether every character of `t` from `at` is a digit.
fn digits_at(t: Text, at: Int) -> Bool {
    if at >= t.len() {
        true
    } else if "0123456789".contains(t.slice(at, at + 1)) { digits_at(t, at + 1) } else { false }
}

/// Whether `t` is a hex digit at `at` and every one after it too.
fn hex_at(t: Text, at: Int) -> Bool {
    if at >= t.len() {
        true
    } else if "0123456789abcdef".contains(t.slice(at, at + 1).lower()) {
        hex_at(t, at + 1)
    } else { false }
}

/// Whether `t` names an entry of the content store: 64 hex digits.
fn is_content_address(t: Text) -> Bool {
    t.len() == 64 and hex_at(t, 0)
}

/// Whether `t` is a SHA-256 digest in hex.
fn is_digest(t: Text) -> Bool {
    t.len() == 64 and hex_at(t, 0)
}

/// Whether the moment `expires` still stands at `at`; one that cannot be read does not.
fn stands(expires: Text, at: Time) -> Bool {
    if let when = Time.parse(expires, near: at) { at <= when } else { false }
}

// ---- connecting the account -------------------------------------------------------------
action open_setup(d: Discord) (icon: .plug, label: "Connect Discord") {
    d.setup_open = true
    d.setup_problem = ""
}

/// Closes it.
action close_setup(d: Discord) {
    d.setup_open = false
}

/// Forgets what the setup last said went wrong, as choosing a channel does.
action clear_problem(d: Discord) {
    d.setup_problem = ""
}

/// Binds this account to one server, with the token its bot uses. The server's layout stays
/// Discord's until the import ran.
action bridge_server(d: Discord, s: ServerChoice, token: Text) (icon: .plug, label: "Bring it in") {
    require moderates(invoker) "Only the workspace's moderators connect a Discord server"
    require token.trim() != "" "The bot's token is needed"
    require d.guild_id == "" "This account is already connected to a server"
    require is_snowflake(s.id) "A server or channel id is a number"
    match engine.call("set-token", token.trim()) {
        .err(why) => {
            d.setup_problem = why
        }
        .ok(_) => {
            connect(d, s.id, s.name)
            d.setup_open = false
            d.setup_servers.remove_where(s -> true)
            d.setup_problem = ""
        }
    }
}

/// Finds the servers the bot is in, so one can be chosen.
action find_servers(d: Discord, token: Text) (icon: .search, label: "Find") {
    require moderates(invoker) "Only the workspace's moderators find the bot's servers"
    require token.trim() != "" "The bot's token is needed"
    match engine.call("find-servers", token.trim()) {
        .err(why) => {
            d.setup_problem = why
        }
        .ok(answer) => match json.decode<ServerList>(answer) {
            .err(why) => {
                d.setup_problem = why
            }
            .ok(list) => {
                if list.servers.is_empty() {
                    d.setup_problem = "The bot is in no server. Add it to one in Discord first."
                } else {
                    d.setup_problem = ""
                    d.setup_servers.remove_where(s -> true)
                    for s in list.servers {
                        d.setup_servers.insert(s)
                    }
                }
            }
        },
    }
}

/// Binds this account to one server. The server's layout stays Discord's until the import ran.
action connect(
    d: Discord,
    guild_id: Text,
    name: Text,
    only_channel: Text = "",
    mirror_channels: Bool = false,
    map_roles: Bool = false,
) {
    require may_configure(invoker) "Only the workspace's moderators connect a Discord server"
    require d.guild_id == "" "This account is already connected to a server"
    require is_snowflake(guild_id) "A server or channel id is a number"
    require only_channel == "" or is_snowflake(only_channel) "A server or channel id is a number"
    d.guild_id = guild_id
    d.only_channel = only_channel
    d.mirror_channels = mirror_channels
    d.map_roles = map_roles
    d.server = name
    d.name = name
    d.ownership = .importing
}

/// Records Discord's layout as the latest event reported it: channels and roles the layout no
/// longer lists go, the ones it lists are brought up to date, and new ones come in.
action observe_layout(
    d: Discord,
    bot_user: Text,
    channels: List<DChannel>,
    roles: List<DRole>,
    emojis: List<DEmoji>,
) {
    require may_configure(invoker) "Only the bridge keeps the server's layout"
    d.bot_user = bot_user
    // What vanished: a mapped channel or role the layout no longer lists.
    let now = channels.map(c -> c.id)
    let gone = d.channels
        .filter(c -> knows_channel(d, c.id) and not now.contains(c.id))
        .map(c -> c.id)
    let role_now = roles.map(r -> r.id)
    let roles_gone = d.roles
        .filter(r -> by_discord(d, .role, r.id) != .none and not role_now.contains(r.id))
        .map(r -> r.id)
    for id in [gone, roles_gone].flat_map(x -> x) {
        d.vanished.add(id)
    }
    for id in [now, role_now].flat_map(x -> x) {
        if d.vanished.contains(id) { d.vanished.remove(id) }
    }
    for id in gone {
        if let held = d.channels.find(c -> c.id == id) { d.channels.remove(held) }
    }
    for id in roles_gone {
        if let held = d.roles.find(r -> r.id == id) { d.roles.remove(held) }
    }
    for c in channels {
        if let held = d.channels.find(h -> h.id == c.id) {
            held.kind = c.kind
            held.name = c.name
            held.parent = c.parent
            held.position = c.position
            held.topic = c.topic
            held.overwrites = c.overwrites
        } else { d.channels.insert(c) }
    }
    for r in roles {
        if let held = d.roles.find(h -> h.id == r.id) {
            held.name = r.name
            held.permissions = r.permissions
            held.color = r.color
            held.position = r.position
            held.managed = r.managed
        } else { d.roles.insert(r) }
    }
    let names = emojis.map(e -> e.name)
    for held in d.emojis.filter(e -> not names.contains(e.name)) {
        d.emojis.remove(held)
    }
    for e in emojis {
        if let held = d.emojis.find(h -> h.name == e.name) {
            held.id = e.id
            held.animated = e.animated
        } else { d.emojis.insert(e) }
    }
}

/// Joins a conversation, message or the like here to its Discord side.
action map(
    d: Discord,
    kind: MappedKind,
    tilo: Text,
    discord: Text,
    origin: OriginSide = .tilo,
    persona: Text = "",
    root: Text = "",
) {
    require may_configure(invoker) "Only the bridge joins the two sides"
    require tilo != "" and discord != "" "A mapping joins two ids"
    if kind == .channel or kind == .category {
        if d.tilo_only.contains(tilo) { d.tilo_only.remove(tilo) }
        if d.discord_only.contains(discord) { d.discord_only.remove(discord) }
    }
    if let m = d.map.find(m -> m.kind == kind and m.tilo == tilo) {
        m.discord = discord
        m.origin = origin
        m.persona = persona
        m.root = root
    } else {
        d.map.insert(
            Mapping(
                kind: kind,
                tilo: tilo,
                discord: discord,
                origin: origin,
                persona: persona,
                root: root,
            ),
        )
    }
}

/// Forgets a mapping, keeping the two sides apart from now on when asked.
action unmap(d: Discord, kind: MappedKind, tilo: Text, keep_apart: Bool = false) {
    require may_configure(invoker) "Only the bridge joins the two sides"
    if keep_apart and (kind == .channel or kind == .category) {
        if let gone = d.map.find(m -> m.kind == kind and m.tilo == tilo) {
            d.discord_only.add(gone.discord)
        }
        d.tilo_only.add(tilo)
    }
    for m in d.map.filter(m -> m.kind == kind and m.tilo == tilo) {
        d.map.remove(m)
    }
}

// ---- linking accounts -------------------------------------------------------------------
action begin_connect(d: Discord, state_sha256: Text) uses time {
    require d.guild_id != "" "This workspace is not bridged to a Discord server"
    require is_digest(state_sha256) "A digest is 64 hex digits"
    for c in d.connects.filter(c -> c.person == invoker.id()) {
        d.connects.remove(c)
    }
    d.connects.insert(
        Connect(person: invoker.id(), state_sha256: state_sha256, expires: "{now + 10m}"),
    )
}

/// What connecting the person's account offers: where to sign in, and the digest of the state
/// that binds what comes back to them.
type Start {
    url: Text
    sha256: Text = ""
}

/// Starts signing in with Discord, as the person looking: answers where to go.
action connect_account(d: Discord) (icon: .link, label: "Connect") {
    require d.guild_id != "" "This workspace is not bridged to a Discord server"
    require d.sign_in_page != "" "This device does not offer signing in with Discord"
    let asked = json.encode(SignInAsk(page: d.sign_in_page))
    let start = Start(url: "", sha256: "")
    match engine.call("sign-in", asked) {
        .err(why) => {
            start.sha256 = why
        }
        .ok(answer) => {
            match json.decode<Start>(answer) {
                .err(why) => {
                    start.sha256 = why
                }
                .ok(found) => {
                    start = found
                }
            }
        }
    }
    if start.url != "" {
        begin_connect(d, start.sha256)
        d.sign_in_url = start.url
        d.setup_problem = ""
    } else {
        d.setup_problem = start.sha256
    }
}

/// What starting a sign-in asks: where Discord's consent page is, without its state.
type SignInAsk {
    page: Text
}

/// Offers a Discord account for linking, from the server: `/link` answered with a one-time
/// token, whose check this is.
action offer_link(
    d: Discord,
    check: Text,
    discord_user: Text,
    user_name: Text,
    display_name: Text = "",
    avatar: Text = "",
    interaction: Text = "",
) uses time {
    require may_configure(invoker) "Only the bridge offers a link"
    require is_digest(check) "A digest is 64 hex digits"
    keep_offer(
        d,
        LinkOffer(
            check: check,
            discord_user: discord_user,
            user_name: user_name,
            display_name: display_name,
            avatar: avatar,
            interaction: interaction,
        ),
    )
}

/// Offers the account that signed in on Discord's consent page, to the person who started the
/// round trip `state_sha256` names.
action offer_sign_in(
    d: Discord,
    check: Text,
    discord_user: Text,
    user_name: Text,
    state_sha256: Text,
    display_name: Text = "",
    avatar: Text = "",
) uses time {
    require may_configure(invoker) "Only the bridge offers a link"
    require is_digest(check) "A digest is 64 hex digits"
    require d.connects.any(c -> c.state_sha256 == state_sha256 and stands(c.expires, now)) "That sign-in with Discord was not started here, or took too long"
    let started = d.connects.find(c -> c.state_sha256 == state_sha256 and stands(c.expires, now))
    if let start = started {
        for c in d.connects.filter(c -> c.person == start.person) {
            d.connects.remove(c)
        }
        keep_offer(
            d,
            LinkOffer(
                check: check,
                discord_user: discord_user,
                user_name: user_name,
                display_name: display_name,
                avatar: avatar,
                person: start.person,
            ),
        )
    }
}

/// Keeps an offer for ten minutes, dropping those run out.
action keep_offer(d: Discord, offered: LinkOffer) uses time {
    require may_configure(invoker) "Only the bridge keeps an offer"
    for o in d.link_offers.filter(o -> not stands(o.expires, now) or o.check == offered.check) {
        d.link_offers.remove(o)
    }
    d.link_offers.insert(
        LinkOffer(
            check: offered.check,
            discord_user: offered.discord_user,
            user_name: offered.user_name,
            display_name: offered.display_name,
            avatar: offered.avatar,
            person: offered.person,
            interaction: offered.interaction,
            expires: "{now + 10m}",
        ),
    )
}

/// Links the Discord account a one-time token offers to the person looking. The offer is used
/// up, and a link the person had before is replaced.
action complete_link(d: Discord, proof: Text) uses time {
    require check_of(proof) != "" "This link was not started here"
    require offered_to(d, proof) != .none "This link was already used or has run out. Start again from Discord with /link"
    require offer_current(offered_to(d, proof), now) "This link ran out after ten minutes. Start again with /link"
    require offer_mine(offered_to(d, proof), invoker) "This link was started by another account. Sign in as that account, or start again"
    if let offered = offered_to(d, proof) {
        for o in d.link_offers.filter(o -> o.check == offered.check) {
            d.link_offers.remove(o)
        }
        for l in d.links.filter(l -> l.person == invoker.id()) {
            d.links.remove(l)
        }
        d.links.insert(
            Link(
                discord_user: offered.discord_user,
                person: invoker.id(),
                linked_at: "{now}",
                user_name: offered.user_name,
                display_name: offered.display_name,
                avatar: offered.avatar,
                interaction: offered.interaction,
            ),
        )
    }
}

/// The check a link proof stands for, or empty when it opens nothing.
fn check_of(proof: Text) -> Text {
    match engine.call("link-check", proof) {
        .err(_) => "",
        .ok(check) => check,
    }
}

/// The offer a link proof opens, if one is still there.
fn offered_to(d: Discord, proof: Text) -> Option<LinkOffer> {
    let key = check_of(proof)
    d.link_offers.find(o -> o.check == key)
}

/// Whether the offer `found` is still good at `at`.
fn offer_current(found: Option<LinkOffer>, at: Time) -> Bool {
    if let offered = found { stands(offered.expires, at) } else { false }
}

/// Whether the offer `found` is nobody's but `me`'s to take.
fn offer_mine(found: Option<LinkOffer>, me: Actor) -> Bool {
    if let offered = found {
        offered.person == "" or offered.person == me.id()
    } else { false }
}

/// Offers signing in with Discord: this device's consent page, without its state; empty for
/// none.
action set_sign_in(d: Discord, page: Text) {
    require may_configure(invoker) "Only the workspace's moderators set up signing in with Discord"
    require page == ""
        or (page.starts_with("https://") or page.starts_with("http://")) and page.contains("?") "A consent page is an http(s) address with its query"
    d.sign_in_page = page
}

/// Edits a slash command's reply to what came of it.
action answer_interaction(d: Discord, op: AnswerInteractionOp) {
    require may_configure(invoker) "Only the bridge answers a slash command"
    require op.content != "" and op.content.len() <= 2000 "A reply holds 1 to 2000 characters"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Takes a linked account's newer names and avatar.
action rename_link(
    d: Discord,
    discord_user: Text,
    user_name: Text,
    display_name: Text = "",
    avatar: Text = "",
) {
    require may_configure(invoker) "Only the bridge takes new names for a link"
    for l in d.links.filter(l -> l.discord_user == discord_user) {
        l.user_name = user_name
        l.display_name = display_name
        if avatar != "" {
            l.avatar = avatar
        }
        if l.avatar == "" {
            l.photo = ""
            l.photo_avatar = ""
        }
    }
}

/// Keeps a linked account's avatar as the person's picture, until they upload their own.
action set_link_photo(d: Discord, discord_user: Text, avatar: Text, photo: Text) {
    require may_configure(invoker) "Only the bridge keeps a link's picture"
    require is_photo(photo) "A picture is a base64 data: URL of a PNG, JPEG or WebP"
    for l in d.links.filter(l -> l.discord_user == discord_user and l.avatar == avatar) {
        l.photo = photo
        l.photo_avatar = avatar
    }
}

/// Whether `photo` is a short base64 data: URL of a picture.
fn is_photo(photo: Text) -> Bool {
    if photo.len() > 48 * 1024 {
        false
    } else {
        photo.starts_with("data:image/png;base64,")
            or photo.starts_with("data:image/jpeg;base64,")
            or photo.starts_with("data:image/webp;base64,")
    }
}

/// Unlinks a Discord account: its person's, or the bridge's.
action unlink(d: Discord, discord_user: Text) {
    require may_configure(invoker)
        or d.links.any(l -> l.discord_user == discord_user and l.person == invoker.id()) "Only the workspace's moderators, or the person it is with, unlink a Discord account"
    for l in d.links.filter(l -> l.discord_user == discord_user) {
        d.links.remove(l)
    }
}

/// Unlinks the person's own Discord account.
action unlink_mine(d: Discord) (icon: .x, label: "Unlink") {
    for l in d.links.filter(l -> l.person == invoker.id()) {
        d.links.remove(l)
    }
}

/// Stops mirroring a Discord account: its messages are no longer carried, and the ones already
/// here are removed. It is not banned there.
destructive action block_persona(d: Discord, discord_user: Text) {
    require may_configure(invoker) "Only the workspace's moderators block a Discord account"
    d.blocked_personas.add(discord_user)
}

/// Mirrors a Discord account again.
action unblock_persona(d: Discord, discord_user: Text) {
    require may_configure(invoker) "Only the workspace's moderators block a Discord account"
    if d.blocked_personas.contains(discord_user) { d.blocked_personas.remove(discord_user) }
}

/// Remember where an avatar's image is stored.
action remember_avatar(d: Discord, hash: Text, blob: Text) {
    require may_configure(invoker) "Only the bridge keeps an avatar"
    require hash != "" and hash.len() <= 64 and is_content_address(blob) "An avatar hash and a content address"
    d.avatars.put(hash, blob)
}

/// Finishes adopting the server: everything in it is here now.
action finish_import(d: Discord, tilo_only: List<Text>) {
    require may_configure(invoker) "Only the bridge finishes the import"
    require d.ownership == .importing "The server was already adopted"
    d.ownership = .adopted
    for id in tilo_only {
        d.tilo_only.add(id)
    }
}

/// Sends a message through a channel's webhook, as the bridge plans it.
// ---- the app it was ----

/// The kind a mapping the app of old kept names.
fn mapped_kind(name: Text) -> MappedKind {
    if name == "category" {
        .category
    } else if name == "thread" {
        .thread
    } else if name == "message" {
        .message
    } else if name == "webhook" {
        .webhook
    } else if name == "role" {
        .role
    } else if name == "emoji" {
        .emoji
    } else {
        .channel
    }
}

/// Which side a mapping the app of old kept was made on.
fn origin_side(name: Text) -> OriginSide {
    if name == "discord" { .discord } else { .tilo }
}

/// The kind of a channel the app of old observed.
fn channel_kind(name: Text) -> ChannelKind {
    if name == "voice" {
        .voice
    } else if name == "category" {
        .category
    } else if name == "thread" {
        .thread
    } else {
        .text
    }
}

/// A time the app of old kept as milliseconds since 1970, as the ISO text this app keeps.
fn iso_of(millis: Int) -> Text {
    "{Time("1970-01-01T00:00:00Z") + Duration(millis)}"
}

/// The Discord server the app of old kept, carried once into this app's record: the joinings of
/// both sides (channels, threads, messages, webhooks, roles and emoji), the accounts people
/// linked, the blocked accounts, and the one channel the bridge is confined to. Running it again
/// adds nothing twice and changes nothing.
migration action carry_native_discord(accounts: Accounts) {
    let earlier = earlier(accounts)
    let held = earlier.find(e -> e.of == "")
    if let g = held {
        let guild_id = g.texts.get("guild_id") ?? ""
        if guild_id != "" {
            let server = g.texts.get("name") ?? ""
            let existing = accounts.accounts.find(d -> d.guild_id == guild_id)
            let account = if let d = existing {
                d
            } else {
                accounts.accounts.insert(Discord(name: if server == "" { "Discord" } else { server }))
            }
            if account.guild_id == "" {
                account.guild_id = guild_id
                account.server = server
                account.name = if server == "" { account.name } else { server }
                account.only_channel = g.texts.get("only_channel") ?? ""
                account.mirror_channels = (g.texts.get("mirror_channels") ?? "false") == "true"
                account.map_roles = (g.texts.get("map_roles") ?? "false") == "true"
                account.ownership = if (g.texts.get("ownership") ?? "importing") == "adopted" {
                    .adopted
                } else {
                    .importing
                }
                account.bot_user = g.texts.get("bot_user") ?? ""
                account.sign_in_page = g.texts.get("sign_in_page") ?? ""
            }
            for blocked in g.lists.get("blocked_personas") ?? [] {
                if not account.blocked_personas.contains(blocked) {
                    account.blocked_personas.add(blocked)
                }
            }
            for m in earlier.filter(e -> e.of == "map") {
                let kind = mapped_kind(m.texts.get("kind") ?? "")
                let tilo = m.texts.get("tilo") ?? ""
                let discord = m.texts.get("discord") ?? ""
                if tilo != "" and discord != "" and not account.map.any(x -> x.kind == kind and x.tilo == tilo) {
                    account.map.insert(Mapping(
                        kind: kind,
                        tilo: tilo,
                        discord: discord,
                        origin: origin_side(m.texts.get("origin") ?? ""),
                        persona: m.texts.get("persona") ?? "",
                        root: m.texts.get("root") ?? "",
                        picture: m.texts.get("picture") ?? "",
                    ))
                }
            }
            for l in earlier.filter(e -> e.of == "links") {
                let user = l.texts.get("discord_user") ?? ""
                let at = l.texts.get("linked_at")?.to_int() ?? 0
                if user != "" and not account.links.any(x -> x.discord_user == user) {
                    account.links.insert(Link(
                        discord_user: user,
                        person: l.texts.get("person") ?? "",
                        linked_at: iso_of(at),
                        user_name: l.texts.get("user_name") ?? "",
                        display_name: l.texts.get("display_name") ?? "",
                        avatar: l.texts.get("avatar") ?? "",
                        interaction: l.texts.get("interaction") ?? "",
                        photo: l.texts.get("photo") ?? "",
                        photo_avatar: l.texts.get("photo_avatar") ?? "",
                    ))
                }
            }
            for c in earlier.filter(e -> e.of == "channels") {
                let id = c.texts.get("id") ?? ""
                let parent = c.texts.get("parent") ?? ""
                if id != "" and not account.channels.any(x -> x.id == id) {
                    account.channels.insert(DChannel(
                        id: id,
                        kind: channel_kind(c.texts.get("kind") ?? ""),
                        name: c.texts.get("name") ?? "",
                        parent: if parent == "" { .none } else { .some(parent) },
                        position: c.texts.get("position")?.to_int() ?? 0,
                        topic: c.texts.get("topic") ?? "",
                    ))
                }
            }
        }
    }
}

// ---- messages and reactions --------------------------------------------------------------
action send_there(d: Discord, op: SendOp) {
    require the_bridge(invoker) "The bridge carries messages to Discord"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    require op.webhook == "" or by_discord(d, .webhook, op.webhook) != .none "That is not one of the bridge's webhooks"
    require op.content.len() <= 2000 and (op.content.len() > 0 or not op.attachments.is_empty()) "A Discord message is 1 to 2000 characters"
    require op.attachments.len() <= 10 and not op.attachments.any(a -> a.size > 10 * 1024 * 1024) "Discord takes at most ten files of 10 MiB from a webhook"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Edits a message the bridge sent.
action edit_there(d: Discord, op: EditOp) {
    require the_bridge(invoker) "The bridge carries messages to Discord"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    require by_discord(d, .webhook, op.webhook) != .none "That is not one of the bridge's webhooks"
    require op.content.len() <= 2000 "A Discord message is 1 to 2000 characters"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Deletes a message there.
action delete_there(d: Discord, op: DeleteOp) {
    require the_bridge(invoker) "The bridge carries messages to Discord"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    require op.webhook == .none or by_discord(d, .webhook, or_empty(op.webhook)) != .none "That is not one of the bridge's webhooks"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Adds the bot's reaction.
action react_there(d: Discord, op: ReactOp) {
    require the_bridge(invoker) "The bridge carries reactions to Discord"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Takes the bot's reaction back.
action unreact_there(d: Discord, op: ReactOp) {
    require the_bridge(invoker) "The bridge carries reactions to Discord"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    let back = ReactOp(op: "unreact", channel: op.channel, message: op.message, emoji: op.emoji)
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(back))))
}

/// Starts a thread on a message.
action start_thread_there(d: Discord, op: StartThreadOp) {
    require the_bridge(invoker) "The bridge starts threads on Discord"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Creates a channel's webhook.
action create_webhook(d: Discord, op: CreateWebhookOp) {
    require the_bridge(invoker) "The bridge makes the server's webhooks"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Creates a channel there, or brings it back in line.
// ---- the server's layout ----------------------------------------------------------------
action apply_channel(d: Discord, op: ChannelSpec) {
    require may_configure(invoker) "Only the workspace's moderators change the server's channels"
    require d.mirror_channels "This bridge does not make channels in Discord"
    require d.ownership == .adopted "The server is adopted first"
    require fresh(op.id)
        or d.map.any(
            m -> (m.kind == .channel or m.kind == .category)
                and m.discord == or_empty(op.id)
                and m.origin == .tilo,
        ) "The bridge changes only channels it made, never the server's own"
    require op.parent == .none or knows_channel(d, or_empty(op.parent)) "There is no such channel in the server"
    require op.name != "" and op.name.len() <= 100 "A Discord channel name is 1 to 100 characters"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Deletes a channel there.
action delete_channel_there(d: Discord, op: DeleteChannelOp) {
    require may_configure(invoker) "Only the workspace's moderators change the server's channels"
    require d.mirror_channels "This bridge does not make channels in Discord"
    require d.ownership == .adopted "The server is adopted first"
    require d.map.any(
        m -> (m.kind == .channel or m.kind == .category)
            and m.discord == op.channel
            and m.origin == .tilo,
    ) "The bridge changes only channels it made, never the server's own"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Puts channels and categories in their places there, so the server lists them in this order.
action arrange_channels(d: Discord, op: ArrangeOp) {
    require may_configure(invoker) "Only the workspace's moderators change the server's channels"
    require d.mirror_channels "This bridge does not make channels in Discord"
    require d.ownership == .adopted "The server is adopted first"
    require op.categories.all(place -> knows_channel(d, place.category)) "There is no such channel in the server"
    require op.channels.all(place -> knows_channel(d, place.channel)) "There is no such channel in the server"
    require op.channels.all(place -> place.parent == .none or knows_channel(d, or_empty(place.parent))) "There is no such channel in the server"
    require op.channels.all(place -> not moves_bridged(d, place)) "The bridge moves only the channels it bridges to another category"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Says whether the bridge may change the server's channels: mirroring is on and the server is
/// adopted.
action may_mirror(d: Discord) {
    require d.mirror_channels "This bridge does not make channels in Discord"
    require d.ownership == .adopted "The server is adopted first"
}

/// Says whether the bridge made the Discord channel or category `id` for one here.
action may_change(d: Discord, id: Text) {
    require d.map.any(
        m -> (m.kind == .channel or m.kind == .category) and m.discord == id and m.origin == .tilo,
    ) "The bridge changes only channels it made, never the server's own"
}

/// Whether `place` would move a channel the bridge did not make to another category.
fn moves_bridged(d: Discord, place: ChannelPlace) -> Bool {
    let parent_now = if let seen = observed(d, place.channel) { seen.parent } else { .none }
    if parent_now == place.parent {
        false
    } else {
        by_discord(d, .channel, place.channel) == .none
    }
}

/// Creates a role there, or brings it back in line.
// ---- roles and members ------------------------------------------------------------------
action apply_role(d: Discord, op: RoleSpec) {
    require may_configure(invoker) "Only the workspace's moderators change the server's roles"
    require d.map_roles "This bridge leaves the server's roles alone"
    require fresh(op.id) or by_discord(d, .role, or_empty(op.id)) != .none "The bridge changes only the roles it made"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Deletes a role there.
action delete_role(d: Discord, op: DeleteRoleOp) {
    require may_configure(invoker) "Only the workspace's moderators change the server's roles"
    require by_discord(d, .role, op.role) != .none "The bridge deletes only roles it manages"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Sets a linked member's roles there.
action set_member_roles(d: Discord, op: MemberRolesOp) {
    require may_configure(invoker) "Only the workspace's moderators change the server's roles"
    require d.map_roles "This bridge leaves the server's roles alone"
    require d.links.any(l -> l.discord_user == op.user) "That Discord account is not linked to anyone here"
    require [op.add, op.remove].flat_map(x -> x).all(role -> by_discord(d, .role, role) != .none) "The bridge assigns only roles it manages"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Kicks a linked member there.
action kick(d: Discord, op: MemberOp) {
    require may_configure(invoker) "Only the workspace's moderators manage Discord members"
    require d.links.any(l -> l.discord_user == op.user) "That Discord account is not linked to anyone here"
    let out = MemberOp(op: "kick", user: op.user, reason: op.reason)
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(out))))
}

/// Bans a linked member there.
action ban(d: Discord, op: MemberOp) {
    require may_configure(invoker) "Only the workspace's moderators manage Discord members"
    require d.links.any(l -> l.discord_user == op.user) "That Discord account is not linked to anyone here"
    let out = MemberOp(op: "ban", user: op.user, reason: op.reason)
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(out))))
}

/// Unbans a linked member there.
action unban(d: Discord, op: MemberOp) {
    require may_configure(invoker) "Only the workspace's moderators manage Discord members"
    require d.links.any(l -> l.discord_user == op.user) "That Discord account is not linked to anyone here"
    let out = MemberOp(op: "unban", user: op.user, reason: op.reason)
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(out))))
}

/// Sets a channel's permission overwrite, for a role the bridge made.
action set_overwrite(d: Discord, op: OverwriteOp) {
    require may_configure(invoker) "Only the workspace's moderators change the server's permissions"
    require d.map_roles "This bridge leaves the server's roles alone"
    require knows_channel(d, op.channel) "There is no such channel in the server"
    require op.target_kind == .role and by_discord(d, .role, op.target) != .none "The bridge sets overwrites only for the roles it made"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Copies a file from Discord's CDN into the workspace's content store.
// ---- files and emoji --------------------------------------------------------------------
action fetch_asset(d: Discord, op: FetchAssetOp) {
    require the_bridge(invoker) "The bridge copies Discord's files"
    require op.max_bytes <= 32 * 1024 * 1024 "Chat holds files of at most 32 MiB"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Uploads a workspace emoji to the server, or renames one the bridge uploaded.
action apply_emoji(d: Discord, op: EmojiSpec) {
    require valid_discord_emoji_name(op.name) "A Discord emoji's name is 2 to 32 letters, digits and underscores"
    require fresh(op.id) or by_discord(d, .emoji, or_empty(op.id)) != .none "The bridge changes only emoji it uploaded"
    require is_content_address(op.picture) "An emoji op names its picture's content address"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Deletes an emoji the bridge uploaded.
action delete_emoji(d: Discord, op: DeleteEmojiOp) {
    require by_discord(d, .emoji, op.emoji) != .none "The bridge changes only emoji it uploaded"
    outside("discord.perform", json.encode(Perform(account: d.id(), op: json.encode(op))))
}

/// Whether `name` is one Discord takes for a custom emoji: 2 to 32 letters, digits and
/// underscores.
fn valid_discord_emoji_name(name: Text) -> Bool {
    name.len() >= 2 and name.len() <= 32 and emoji_name_at(name, 0)
}

/// Whether every character of `name` from `at` is one an emoji's name takes.
fn emoji_name_at(name: Text, at: Int) -> Bool {
    if at >= name.len() {
        true
    } else {
        let c = name.slice(at, at + 1).lower()
        if "abcdefghijklmnopqrstuvwxyz0123456789_".contains(c) {
            emoji_name_at(name, at + 1)
        } else { false }
    }
}

/// Records what became of an emoji upload, rename or delete.
action settle_emoji(
    d: Discord,
    tilo: Text,
    emoji_id: Text = "",
    problem: Text = "",
    picture: Text = "",
    was: Text = "",
    deleted: Bool = false,
) {
    require may_configure(invoker) "Only the bridge settles an emoji"
    require problem == "" or problem.len() <= 300 "An emoji problem is 1 to 300 characters"
    if problem != "" {
        d.emoji_problem = problem
    } else {
        d.emoji_problem = ""
        if deleted {
            for m in d.map.filter(m -> m.kind == .emoji and m.tilo == tilo) {
                d.map.remove(m)
            }
        } else if emoji_id != "" and valid_discord_emoji_name(tilo) {
            if was != "" and was != tilo {
                for m in d.map.filter(m -> m.kind == .emoji and m.tilo == was) {
                    d.map.remove(m)
                }
            }
            unmap(d, .emoji, tilo)
            d.map.insert(
                Mapping(
                    kind: .emoji,
                    tilo: tilo,
                    discord: emoji_id,
                    origin: .tilo,
                    picture: if is_content_address(picture) { picture } else { "" },
                ),
            )
        }
    }
}

/// Says why the bridge cannot carry voice now, or that it can again.
// ---- voice ------------------------------------------------------------------------------
action report_voice(d: Discord, problem: Text) {
    require may_configure(invoker) "Only the bridge reports what Discord said of a call"
    require problem == "" or problem.len() <= 300 "A voice problem is 1 to 300 characters"
    d.voice_problem = problem
}

/// A Discord voice channel and the voice channel here it is carried to: one call, heard on both
/// sides.
type VoicePair {
    discord: Text
    tilo: Text
}

/// Every paired voice channel the bridge reaches, by Discord id.
fn voice_pairs(d: Discord) -> List<VoicePair> {
    let pairs: List<VoicePair> = []
    for m in d.map.filter(m -> m.kind == .channel) {
        if let seen = observed(d, m.discord) {
            if seen.kind == .voice and (d.only_channel == "" or d.only_channel == m.discord) {
                pairs = pairs + [VoicePair(discord: m.discord, tilo: m.tilo)]
            }
        }
    }
    pairs.sort_by(p -> p.discord)
}

/// A value nothing reads, for the trigger's frame that does nothing itself: the origin polls
/// Discord's events and the conversations here in its own loop.
// ---- small pieces -----------------------------------------------------------------------
action pump(d: Discord) {
}

/// Keeps the bridge's origin running on a device, whether or not a window of Chat is open.
trigger watch_discord(d: Discord) every 1s: pump(d)
