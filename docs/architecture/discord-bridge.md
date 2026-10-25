# Discord bridge and persistent public rooms

A persistent public room is a Tilo workspace that is always on. It is an
ordinary workspace of the Tilo app, and anyone opens it from a browser in a
second, with an identity of their own and no account. Its Chat can be
mirrored both ways to a Discord server. A message posted in either place
shows up in the other, under its author's name and avatar.

The bridge is not a special feature of Chat. It is the first user of the bot
framework ([bots.md](bots.md)):

- a **Discord connector** (`so.tilo.discord`), a native integration app on the
  host that holds the bot token, speaks Discord's API, exposes typed actions and
  publishes Discord's events as trigger sources. It never acts in the room;
- the **Discord Bridge** bot (`so.tilo.bots.discord-bridge`), a Shortcut program
  published in the bot directory that maps Chat to the connector and back. It
  is the only room actor in the bridge;
- **Chat** itself, which gains appearance rendering, author checks, a Moderator
  role and channel locks that every workspace has, bridged or not
  ([chat-shared-space.md](chat-shared-space.md), "Bridges").

Anyone can install the same bot in their own room against their own Discord
server; a persistent public room is one installation of it.

## 1. Pieces

```text
 Discord ◄──── gateway / REST / webhooks ────► outside worker (egress: discord.com …)
                                                    │ stdin/stdout
                                         connector (App, no room identity)
                                           actions ▲   │ trigger sources
 tilo-room-service ── tilo-relay room --bots ── BotHost ◄┘
   /join /presence /avatars                         └─ bridge bot (bot:<bridge id>) → chat.workspace ⇄ discord.guild
 browsers ── WebTransport / WebSocket ── relay ── Chat (so.tilo.chat)
```

Everything Discord-specific lives in the connector: its outside worker, and
the `discord.guild` record kind in which the bridge keeps its state. Everything that decides what maps to what lives in the bridge program,
which members can read in the room. Chat knows nothing about Discord.

## 2. The connector

`apps/discord` (new) publishes one resource kind, `discord.guild`, with one
record per bridged server, and runs its outside worker on the room's host. It is
installed on the host like any app and has no identity or grant in the room
(bots.md §6). The bridge bot creates the record, writes it and invokes its
actions, so every change in it is the bot's or a person's.

### Record

```rust
// apps/discord/src/record.rs (new)
#[record(kind = "discord.guild")]
pub struct Guild {
    #[readonly] pub guild_id: String,           // Discord snowflake
    pub name: String,
    pub ownership: Ownership,                   // Importing | Adopted
    #[readonly] pub mirror_channels: bool,      // channels made in Tilo after adoption go to Discord
    #[readonly] pub map_roles: bool,            // Tilo roles go to Discord as the bridge's own roles
    pub token_item: Option<String>,             // secret item id; the token stays in the host's store
    pub channels: Keyed<DChannel>,              // Discord's layout as the bot last observed it
    pub roles: Keyed<DRole>,
    pub map: Keyed<Mapping>,                    // Tilo id ⇄ Discord id (§6)
    pub links: Keyed<Link>,                     // Discord account ⇄ Tilo person (§5)
    pub pending_links: Keyed<PendingLink>,
    pub blocked_personas: Set<String>,          // Discord user ids
    pub tilo_only: Set<String>,                 // Tilo ids kept out of Discord
    pub discord_only: Set<String>,              // Discord ids kept out of Tilo
    pub outbox: Outbox<DiscordOp, DiscordResult>,
}

pub enum Ownership { Importing, Adopted }

pub struct DChannel { pub id: String, pub kind: DChannelKind, pub name: String,
                      pub parent: Option<String>, pub position: i64, pub topic: String,
                      pub overwrites: Json<Vec<Overwrite>> }
pub enum DChannelKind { Text, Voice, Category, Thread }
pub struct DRole { pub id: String, pub name: String, pub permissions: String,
                   pub color: u32, pub position: i64, pub managed: bool }
```

### Verbs

| Verb | Who may call | Kind | What it does |
|---|---|---|---|
| `observe-layout(channels, roles, emojis)` | bots | Undoable | Records Discord's layout (and the server's custom emoji) as the latest layout event reported it, for the reconcile (§4) and for mirroring reactions (§3). |
| `send`, `edit`, `delete`, `react`, `unreact`, `start-thread`, `create-webhook` | bots | Outside | Queues the matching webhook or REST call. |
| `apply-channel`, `delete-channel`, `apply-role`, `delete-role`, `set-member-roles`, `kick`, `ban`, `unban`, `set-overwrite` | bots | Outside | Queues layout and membership calls (§4, §5). |
| `map`, `unmap` | bots | Undoable | Writes the id map (§6). |
| `begin-link` | a person (device effect, then this verb) | Undoable | Stores a pending link code's hash (§5). |
| `complete-link`, `unlink` | bots | Undoable | Confirms or removes a link. |
| `block-persona`, `unblock-persona` | moderation | Undoable | Stops mirroring a Discord user into Tilo (§7). |
| `set-bot-token(token: Secret)` | the host's administrator, on the host | effect | Stores the token in the host's secret store (bots.md §6). |

The bridge bot settles the outbox entries it queued, after the connector has
performed them (bots.md §6). Each verb checks its caller in the app, so every
replica refuses a forged call (bots.md §6). Outside verbs take only ids the record already knows, such as
mapped channels and the guild's own roles, so a bot cannot aim the connector at
another server.

### Trigger sources

| Source | Payload | Event id |
|---|---|---|
| `message-created` | channel, message id, author (id, display name, avatar hash, bot flag, webhook id), content, reply reference, thread, attachments (name, size, type, CDN URL) | message id |
| `message-updated` | channel, message id, content, edit time | message id + edit time |
| `message-deleted` | channel, message id, executor from the audit log when Discord reports one | message id + `deleted` |
| `reaction-added`, `reaction-removed` | channel, message id, user id, the member's name and avatar (on an add), emoji (Unicode, or a custom emoji as `:name:` with its id and whether it is animated) | message, user, emoji, add or remove, and the gateway sequence |
| `layout-changed` | the guild's channels, roles and custom emoji, read after any channel, role or emoji event | gateway sequence |
| `member-changed` | user id, roles, banned or kicked | gateway sequence |
| `command` | `/link` or `/unlink`, user id, the code's SHA-256 (the worker hashes it on arrival) | interaction id |

### Outside worker

The worker is the `discord-worker` binary in the connector's package. It keeps
one gateway session with the intents `GUILDS`, `GUILD_MEMBERS`,
`GUILD_MODERATION`, `GUILD_EXPRESSIONS` (custom emoji), `GUILD_VOICE_STATES`, `GUILD_MESSAGES`,
`GUILD_MESSAGE_REACTIONS` and `MESSAGE_CONTENT`, and receives slash-command
interactions over the same gateway. Its egress allowlist is `discord.com`,
`gateway.discord.gg` (and the regional `*.discord.gg` gateways Discord names),
`cdn.discordapp.com`, and, while it carries a call, the voice server
`wss://*.discord.media` Discord assigns plus UDP to the public address and
port that server's Ready names. Nothing else.
It receives the bot token and the webhook tokens from the host as `Secret`
messages and never writes them anywhere. It honours Discord's rate-limit
headers per route, which is why a burst of Tilo messages arrives late rather
than being dropped. On reconnect it resumes the gateway session. If Discord
refuses the resume, it backfills each mapped channel's messages after the last
seen one (at most 100 per channel), and the event log's deduplication by event
id drops what was already delivered.

Webhook tokens are created by the `create-webhook` op. The worker returns the
token to the host as a secret, never in the op's result. The host stores it as
`SecretStore::item("webhook", <channel id>)`, so the record holds only the
webhook's id.

### Voice

The worker reports who is in the guild's voice channels (only the bridged
channel when it bridges one) as `Calls`: each person's account, name (nick,
global name or username), avatar and whether they are muted (self, server or
suppressed), the bot left out, again whenever that changes. A `Call` from the
host puts the bot in one voice channel (a bot can be in one per guild, so the
newest wins) or takes it out; the report's `link` says `Joining`, `Joined`,
`Idle` or `Refused` with the reason.

Before joining the worker computes the bot's permissions in the channel from
the guild's roles and the channel's overwrites and needs View Channel, Connect
and Speak; what is missing is named ("the bot lacks Connect and Speak in
#voice") in `Refused` and on the connector's status. So are a voice server's
fatal close codes, a moderator moving or disconnecting the bot, and a join
Discord ignores for 20 s (a full channel). A refused call is not retried until
the host asks again.

The voice connection is Discord's voice gateway v8 (heartbeats acknowledge the
last sequence, a dropped socket resumes) and RTP over UDP after IP discovery,
in `aead_aes256_gcm_rtpsize` when offered, else
`aead_xchacha20_poly1305_rtpsize` (sealed by `tilo_seal::discord_voice`, as
every raw AEAD is). Media is end-to-end encrypted with DAVE,
which Discord requires in voice channels since March 2026: the worker is an
MLS member through `davey` (vendored, see `vendor/davey/TILO-PATCH.md`),
handles the external sender, proposals, commits, welcomes, transitions, epoch
resets, invalid-commit recovery and the downgrade to transport only.

Discord users' Opus frames go to the host as `Audio` with their account as
the speaker. The host's frames are mixed on a 20 ms tick with a two-to-three
frame jitter buffer per speaker: one speaker's Opus passes through undecoded,
two or more are decoded, summed with saturation and encoded once (stereo,
64 kbit/s). The worker sends Speaking before audio and five silence frames
and Speaking off after it. Received and sent audio never meet, so nothing a
Discord user says goes back to Discord. The `status` of a `Call` (who is on
the Tilo side) is shown as the voice channel's status while the bot may set it
(Set Voice Channel Status, granted with Administrator), else as the bot's
nickname (Change Nickname); leaving clears it.

## 3. The bridge bot

The bridge is a Shortcut program with `RecordChanged` triggers on
`chat.workspace` (by people, agents and other bots, never itself) and
`Connector` triggers on each of the connector's sources, and on `tilo.room`
(`set-role`, `remove-member`, `ban` and `unban`, for role and moderation
mirroring). It asks for `Personas`
appearance. Its derived grant comes to: Chat `post`, `edit-message`,
`delete-message`, `create-category`, `create-channel`,
`move-channel`, `delete-channel`, `remove-message`, `lock-channel` and
`unlock-channel`; the field edit of Chat's messages
(`chat.messages/resource:*/**/action:field-edit`), which reaches only their
reactions: every other field of a message is read-only, so the record and
the room take no other field edit of it; the connector's bot verbs; and Read on both records, on the
connector's app node and on the room. The program's inputs bind `workspace` and `guild` at install.

### Tilo to Discord

| Tilo change | Discord call |
|---|---|
| `post` in a mapped channel | Webhook execute in the mapped channel. `username` is the author's display name, and `avatar_url` is their public avatar URL (§9). Content is the message's plain text, with mentions of linked people written as `<@id>` and others as plain names. `allowed_mentions` is limited to users, so nobody can ping `@everyone` through the bridge. |
| reply (`parent`) | Posted into the Discord thread mapped to the Tilo thread. If the thread has none yet, `start-thread` on the root's Discord message first. |
| `edit-message` | Webhook message edit. |
| `delete-message`, `remove-message` | Webhook message delete. |
| a reaction (the `react` / `unreact` field edit of a message's reactions) | The bot user adds the emoji once while at least one Tilo person has that reaction, and removes it when the last one goes. Discord shows one bot reaction per emoji. Tilo shows who reacted. A workspace emoji `:name:` is the server's custom emoji of that name (`name:id`); see Reactions below. |
| attachment | Uploaded with the webhook call if it is at most 10 MiB (Discord's limit for bots). A larger one becomes a line: "📎 name (size) – open in Tilo" with the message's link. |
| channel and category changes | Layout reconcile (§4). |

Messages posted by agents and other bots go across too, with the same name
treatment and a "🤖" prefix on the webhook name, because Discord has no other
way to mark a webhook author as automated.

### Discord to Tilo

| Discord event | Tilo change |
|---|---|
| `MESSAGE_CREATE` | `chat.post` in the mapped channel, with `Appearance { name: member display name, avatar: <avatar blob>, persona: discord/<user id> }`. The avatar is fetched once per avatar hash and stored in the content store. |
| reply (`message_reference`) | Posted as a reply to the mapped message's thread root. Tilo threads are one level deep, so a reply to a reply joins the root's thread, and its text starts with a quote of the message it answered. |
| message in a Discord thread | Reply to the thread's mapped root. |
| `MESSAGE_UPDATE` | `edit-message` on the mapped message. |
| `MESSAGE_DELETE` | `delete-message` on the mapped message (the bot is its author). |
| `MESSAGE_REACTION_ADD` / `REMOVE` | The `react` / `unreact` field edit of the mapped message's reactions, with the member's appearance. The reaction records the persona (`discord/<user id>`) and the name, so each Discord member is a reaction of their own. A custom emoji arrives as its name, `:tilo_hello:`, which shows as the workspace's emoji of that name, or as its text where the workspace has none. |
| attachments | Each one up to 32 MiB (Chat's limit) is downloaded from the CDN and posted as a Chat attachment. A larger one becomes a link to its CDN URL. Discord's URLs expire, so the bytes are copied, not linked, whenever they fit. |

### Echo suppression

The bridge must not mirror its own output back. Two rules cover every case:

- On the Tilo side the dispatcher never fires a bot on its own changes
  (bots.md §4). A message the bridge posted from Discord therefore never goes
  back to Discord.
- On the Discord side the bridge skips gateway events whose `webhook_id` is one
  of the guild's mapped webhooks, or whose author is the bot user. Those are its
  own messages and reactions.

The id map is how edits, deletes, reactions and replies find their
counterparts. It is not needed to stop echoes.

### Reactions

Chat keeps a message's reactions in the framework's `Reactions` field, which
changes only through the `react` and `unreact` field edits. The bridge reads
those edits from the change's history entry (`op` `react` or `unreact` on
`messages › <message> › reactions`, the emoji as `new`), and makes them
itself for Discord members, under each member's appearance.

- **Unicode emoji** cross as they are, skin tones included. One emoji may be
  spelled with or without its variation selector (`❤️`, `❤`); the bridge
  compares emoji without it, and a Discord reaction takes the spelling the
  message's Tilo reactions already use, so one emoji never shows as two.
- **Custom emoji.** Tilo writes a workspace's own emoji `:name:`, under the
  rule Discord names custom emoji by (2 to 32 letters, digits and
  underscores). A Discord custom emoji becomes its `:name:` in Tilo. A Tilo
  reaction `:name:` becomes the server's custom emoji of that name, which the
  worker learns from `GUILD_CREATE` and `GUILD_EMOJIS_UPDATE` and the layout
  event carries to the record. When the server has none of that name, the
  reaction stays in Tilo. Uploading the picture would need Manage Expressions
  and spend the server's few emoji slots on whatever anyone reacts with, and a
  Unicode emoji of a similar name would show Discord something nobody picked.
  It crosses once the server adds an emoji of that name and someone reacts
  again.
- **Echoes.** Reactions the bridge made for Discord members never count
  toward the bot's own reaction, and the dispatcher never fires the bridge
  on its own edits, so nothing mirrored in goes back out. The bot's own
  reactions come back from the gateway as its user and are dropped.

## 4. Channel layout

Connecting to a server adopts it. The server was there first, with its own
channels, roles and permissions, and the people who run it keep running it
in Discord. The bridge never renames, moves, recreates or deletes a channel
it did not make, and never changes a permission it does not own.

- **Mapping.** Each Discord category maps to a Tilo category, each text
  channel to a Text channel, and each voice channel to a Voice channel. The
  mapping, not the name, is the identity, and each map entry records which
  side the pair came from.
- **Adoption.** While `ownership` is `Importing`, every reconcile reads
  `Guild::channels` and joins it with the workspace. A Discord category
  adopts the workspace's unmapped category of the same name; a Discord
  channel adopts the unmapped Tilo channel whose slug matches in the same
  category, else one whose slug matches in a category of Tilo's own (one no
  Discord category joins, such as the channels a room opened with), so the
  room's `general` is the server's #general rather than a second one. A
  channel joined that way moves into its Discord channel's category
  (`set-channel-category`), and a category of Tilo's own those moves leave
  empty is deleted, so the room is laid out as the server is. What
  has no match is created in Tilo. When every Discord channel
  is mapped, the Tilo channels that matched nothing and hold no message
  (the `#chat` every room opens with) are deleted, with any category of
  Tilo's own they leave empty, so the room has exactly the server's
  channels; the Tilo categories and channels that matched nothing and
  remain are listed in `tilo_only`, and `ownership` becomes `Adopted`.
  Chat's `read-room` names the channels holding a message (`busy`). The first time
  the worker meets the server it reads the newest 50 messages of each text
  channel and reports them after the layout, as if just posted, so the
  adopted room starts with that history; each is posted at the time it was
  written (Chat's `post` keeps `written` for a message a bot brings over).
- **After adoption.** A category or channel created in Discord later is
  imported the same way. One deleted on either side lets its pair go: the map
  entry is removed and both ids are kept apart (`tilo_only`,
  `discord_only`), so neither side recreates it. A Discord channel renamed or
  moved stays as Discord has it.
- **Mirroring.** Only with `mirror_channels` does a category or channel
  created in Tilo after adoption get made in Discord, placed last among its
  siblings. Only such a bridge-made channel follows Tilo's name, topic and
  category, and only it is deleted in Discord when Tilo deletes its side.
  What Tilo had before adoption stays in Tilo.
- **Webhooks.** Every mapped text channel gets the bridge's webhook, which
  is how Tilo messages are posted (§3). That webhook is the one change
  adoption makes in the server.
- **Voice.** A mapped voice channel pair is one call. The room's host runs
  the call beside the bridge bot (`tilo-relay`'s `room_bots::discord_voice`):
  while either side has anyone in it, the bot joins the other. In Tilo each
  Discord person is a participant of their own, shown by name and avatar
  "via Discord" (as the linked member when the account is linked), with
  their own speaking light and volume; the bridge's own participation only
  listens and marks the call "bridged to Discord" for everyone in it. In
  Discord the bot is the one voice user, carrying the Tilo speakers mixed,
  and the voice channel's status (or the bot's nickname) names who is on the
  Tilo side. Nobody hears themselves back, and a person removed or banned
  from the room, blocked in the bridge or denied in the call is not
  carried. Discord allows a bot in one voice channel per server, so one pair
  is carried at a time.

The bot's guards are in the record as well as in the bot: `apply-channel`
and `delete-channel` are refused without `mirror_channels`, before adoption,
and for any channel the bridge did not make.

For the Tilo Discord server, joined with a room whose Chat already has
"Channels" with #general, #help and #show-and-tell, adoption produces this:

| Discord | Tilo |
|---|---|
| category "Channels" | the room's `Channels` |
| #general and voice channel "voice" in "Channels" | its `general`, and a new Voice channel `voice` |
| #goal, #roadmap, #progress (no category) | new Text channels with no category |
| empty category "chat" | new category `chat` |
| (none) | `help` and `show-and-tell` stay Tilo-only |
| roles | nothing; roles are only touched with `map_roles` (§5) |

The only calls made to Discord are the webhooks and the bot's `/link`
command.

## 5. Accounts, links and roles

### Linking

A Discord user is an appearance-only persona until they link their account.
After linking, their Discord messages still arrive as the bot's posts with their
appearance, but the persona resolves to their Tilo person. The author view then
links to their profile, and with `map_roles` their Tilo role is mirrored to
Discord.

1. In Tilo, the person runs "Link Discord account" (an action on the guild
   record). A device effect generates a code of 60 random bits, shown as 12
   base-32 characters. `begin-link` records the person, the code's SHA-256 and
   an expiry 10 minutes out. The code itself stays on the person's screen.
2. In Discord, they run `/link CODE`. The worker hashes the code before it is
   delivered and answers the interaction ephemerally. The `command` event
   carries the Discord user id and the hash.
3. The bot finds the pending link with that hash, checks the expiry, and calls
   `complete-link`. The verb checks again that the hash matches and that the
   pending link belongs to the person it names, then records
   `Link { discord_user, person, linked_at }` and removes the pending entry.

`/unlink` in Discord and "Unlink" in Tilo both call `unlink`. Five failed
`/link` attempts from one Discord user within an hour are ignored for the rest
of that hour. With 60 bits and a 10-minute window, guessing a code is not
practical. The limit keeps the log quiet.

The person does this from their profile card or from Settings → External
Accounts, without opening the connector's window. Both list "Linked accounts"
for every service the room is connected to (`tilo_app::linked_accounts`).
The connector's code stays on the room's host: `my-link`, `link-account` and
`unlink-mine` are effects any member may ask the host to run for themselves
(`#[effect(member)]`). The host runs them as that member, and the page needs
none of the connector's code. `my-link` answers whether the record is
connected to a server, the reader's linked account by name (username, and
display name when it differs), and the actions that link and unlink it.
`link-account` makes the code on the host, records only its hash with
`begin-link` and answers the code to the asking device alone, which shows it
in a monospace chip with a Copy button under "In Discord, type /link with this
code. It works for 10 minutes." `unlink-mine` unlinks the invoker's own
account.

The worker reads each `/link` author's username and display name off the
interaction. `complete-link` records them in the `Link`, and `rename-link`
updates them when a member event shows newer ones.

### Roles

By default the bridge touches no role and no permission. With `map_roles`,
Tilo's roles are mirrored as Discord roles the bridge makes and owns:
`Tilo Viewer`, `Tilo Commenter`, `Tilo Editor`, `Tilo Moderator` and
`Tilo Owner`. `@everyone` and the server's own roles are never changed, even
when they share a name; the record refuses a role or overwrite change for
any role the bridge did not make.

| Tilo role | Discord role |
|---|---|
| Editor | View Channels, Read History, Send Messages, Add Reactions, Attach Files, Connect, Speak, Use Slash Commands |
| Viewer | as Editor without Send Messages, Add Reactions and Attach Files, which a deny overwrite for the role on every mapped channel also withholds |
| Commenter | as Viewer, plus Add Reactions |
| Moderator | as Editor, plus Manage Messages, Moderate Members, Kick Members, Ban Members |
| Owner | as Moderator |

Membership is mirrored only for linked accounts: the bot calls
`set-member-roles` when a linked person's Tilo role changes, and reverts a
change someone makes by hand to one of the bridge's roles. Unlinked Discord
users keep whatever the server gives them.

## 6. The id map

`Guild::map` holds one `Mapping` per bridged object:

```rust
pub struct Mapping {
    pub tilo: TiloRef,                // workspace / category / channel / message (by id)
    pub discord: String,              // snowflake
    pub kind: MappedKind,             // Category | Channel | Thread | Message | Webhook
    pub origin: Origin,               // Tilo | Discord: where the object was created
}
```

`map` and `unmap` refuse callers that are not bots (bots.md §6), so a member
cannot redirect a channel. The key is `kind:tilo-id`, and a reverse index is
derived on read. The map is a record field because both directions and every
restart need it, and a record is replicated, undo-aware and readable by members
who want to know where a message went. Entries for deleted messages are removed
with them. A month of a busy public room is tens of thousands of small rows, well
within a record's size.

## 7. Moderation

Moderation is Chat's own, decided by Tilo roles ([permissions.md](permissions.md)
§3). The bridge carries its effects across.

| In Tilo | Effect in Discord |
|---|---|
| A Moderator runs `remove-message` | The mapped Discord message is deleted. |
| A Moderator removes or bans a guest with no linked account | Tilo-only. Such a guest has no Discord account to act on. |
| A Moderator removes (kicks) a linked person | Their linked Discord account is kicked from the server. |
| A Moderator bans a linked person | Their linked Discord account is banned too. |
| A Moderator unbans a linked person | Their linked Discord account is unbanned. |
| A Moderator runs `block-persona` on a Discord author | Their Discord messages are no longer mirrored, and the bridge removes the ones already mirrored. It does not ban them in Discord, because that is the Discord moderators' decision. |

| In Discord | Effect in Tilo |
|---|---|
| A Discord-origin message deleted | The mirrored message is deleted (§3). |
| A Tilo-origin message deleted by a linked Tilo Moderator (read from the audit log's executor) | `remove-message` in Tilo, attributed to the bot, with the Moderator's link noted in the reason. |
| A Tilo-origin message deleted by anyone else | Sent again by the bridge, because a Discord-only permission must not remove a Tilo person's message. |
| A Discord user banned | The persona is blocked (`block-persona`). |

## 8. Failure handling

- **Order.** The bridge's firings run one at a time in change order
  (bots.md §4), so a message is mirrored before its edit or delete. An edit
  whose message is not mapped yet waits for its post's run.
- **At least once to Discord.** Webhook calls carry no idempotency key, and
  Discord accepts a `nonce` only from bot users. A host that fails after Discord
  accepted a message but before the outbox entry settled will drain it again. Before
  re-sending, the worker therefore lists the channel's last 50 messages for one
  from the same webhook with the same content, posted after the entry was
  created. If it finds one, it returns that id as the op's result, and the bot
settles the entry with it. A duplicate remains possible
  only when the channel moved past 50 messages in the meantime.
- **Exactly once into Tilo.** The connector's event log deduplicates by event
  id, a run's id is derived from the event id (bots.md §4), and each Chat change is checked against history by its derived invocation id
  (bots.md §9), so restarts and gateway replays never post twice in Tilo.
- **Discord unavailable.** Outbox entries wait, and the bridge's queue holds
  (bots.md §9). Tilo keeps working, and the backlog drains in order when Discord
  returns, at Discord's rate limits.
- **Token revoked.** The worker reports the gateway's authentication failure,
  and ops fail with it. The connector's page on the host shows "Discord rejected the bot token",
  and the bots that use it pause until `set-bot-token` is run again.
- **Missing permissions in Discord.** A refused call is a refusal, not a retry.
  It is dead-lettered with Discord's error, and the bot's page shows which
  permission the bot role lacks.

## 9. Persistent public rooms

A persistent public room is a workspace of the Tilo app like any other: the
app can list it as a known public workspace on its host, and opening it
without a membership runs the app's admission against that host, after which
it is an ordinary workspace. Its instant join is the host's admission policy,
not a page or app of its own; the room service serves no product page. The
host runs `tilo-room-service` in a persistent mode. The room service is built
for short rooms: one throwaway room per request, with every visitor
certified as a device of the room's own root. That last part means every
visitor is the same person, which is fine for a scratch room and wrong for a
public one, where people must be told apart, moderated and banned one by
one. Persistent mode changes three things: the room survives restarts, each
visitor is their own person, and joining is rate-limited.

### Room-service changes

- **Persistent room.** `TILO_ROOMS_PERSISTENT=<name>` starts one room at boot
  with a stable id, kept in `TILO_ROOMS_STATE/<name>/room-id`, and the child's
  state in the same directory on a persistent volume. `POST /rooms` is disabled.
  The child runs `tilo-relay room --bots` with the governed room and its Chat
  workspace. Chat history is the room's records, so it persists with the
  volume. The room is created as a governed room, so it has a real member list
  and room policy. A room on a node certificate can own workspaces
  (`cloud_enrollment::host_capabilities()`; `DeviceCapabilities::host()` cannot).
- **Owner root on the host.** The room's owner root is generated once and
  kept in the host's `tilo-secrets` store. It is mounted from the operator's
  encrypted secrets like the bot token (below) and never written to a record.
  Everywhere else in Tilo a room's root lives on its owner's devices, and the
  host never holds it. Public hosted rooms are the documented exception
  (bots.md §2), because nobody is present to approve joins and the room is
  public anyway. The root signs pre-admissions and installs the bridge, and the
  runner renews the bridge's delegation with it on the host automatically,
  each time for at most 30 days. Each renewal is a governed change, visible in
  the room's history. The people who run the room do not use this root. They
  hold ordinary Owner or Moderator grants on their own identities (below), so
  their moderation is attributed to them.
- **Instant guest join.** `POST /join { userPublicKey, displayName }`. The browser
  joins as the person it is (`browserPerson()`: on the first visit a real user
  root, held only wrapped under a non-extractable key in IndexedDB, and a
  device key; see browser-runtime.md, "A browser-held root"). A returning
  visitor keeps their person, and one who kept it with a passkey is the same
  person on any browser. The service checks
  the limits below, generates a one-time token, and sends the room child a new
  operator command,
  `PreAdmit { user_public_key, token_sha256, label, role: Editor }`. The child signs a
  `PreAdmission` with the room root and answers
  `PreAdmitted { invitation }`, an invitation only `user_public_key` can
  redeem. The service returns
  `{ invitation, admissionToken, origin }`. The browser calls
  `joinWorkspace({ origin, invitation, admissionToken, identity, displayName })`
  and is admitted at once as an Editor, with no pairing code. The display name is the
  one typed in, or a random "adjective animal" name when none is given.
  On a later visit, `connectPeer` reuses the stored identity without calling
  `/join`.
- **Spectators before guests.** The site's homepage embed must not mint a member
  for every visitor who scrolls past it. The embed (`?embed=1`) therefore joins
  as an anonymous, read-only spectator: the page creates a device key in memory
  only (nothing is stored, no person is created), and `POST /spectate {
  devicePublicKey }` returns a device certificate of the room's one *spectator*
  person, valid for one hour and with no capabilities. The spectator person's
  root is generated at the room's first start and kept in the host's
  `tilo-secrets` store beside the owner root; it is a Viewer member of the
  room, named "Watching visitors". The relay admits a device that person
  certified as a spectator: it may subscribe, read the room's state and replay
  its history, and nothing else. Its changes, commits, presence, record
  content, compactions, actions and live join are all refused, so a spectator
  neither edits nor shows a cursor. It still sees the members' cursors: the
  relay's presence reaches every subscribed page, and its live grant lets it
  receive on the live plane but not publish there. It is never a channel's
  member and asks no channel for access. Because anyone may join this room as
  a guest and then read every channel, letting a visitor who only watches
  read them exposes nothing new: the room's steward hands the content keys of
  a channel that exists (`POST /channel-keys`) to any device the spectator
  person certified, while that device holds a spectator seat. That is one
  grant to the spectator person, not one per device. A room that is not
  public, and a record a Read Deny keeps from anyone, gets no keys, however
  spectators are configured. The page asks for a record's keys when it opens
  the record or a sealed document of it arrives that its keys cannot open,
  for example after a new epoch. Spectators are not counted against `TILO_ROOMS_MAX_GUESTS` and do
  not appear in the member list one by one. They have their own cap,
  `TILO_ROOMS_MAX_SPECTATORS` (default 1000), which the room service passes to
  the room's relay: the relay refuses a spectator's subscription while that
  many spectators are subscribed, whichever transport they came by. They also
  have their own per-IP window (30 per 10 minutes); above the cap the embed
  shows the site's static fallback. A spectator becomes a guest on the first
  interactive gesture in the embed (a click, tap or key press, under a "click
  or type to join" hint): the page then runs the instant guest join above and
  remounts as that person. Opening the room in the Tilo app (its web entry,
  or a link into it) joins at once, without the spectator step.
- **Operator protocol.** `room_operator` goes to version 2. It adds
  `OperatorCommand::PreAdmit`, `RoomEvent::PreAdmitted`,
  `RoomEvent::PreAdmitRefused { token_sha256, reason }`,
  `OperatorCommand::Spectate { device_public_key }` with
  `RoomEvent::SpectatorCertified { device_public_key, certificate }`,
  `RoomEvent::Online { people, spectators }` (debounced to one per 2 s), and
  `RoomEvent::MemberRemoved { user_public_key, banned }`, which the service
  needs for the hashed-IP cooldown below. `Started` names the governed room.
- **Limits.**
  - Per IP: 5 joins per 10 minutes and 30 per day by default
    (`TILO_ROOMS_GUEST_JOINS_PER_10_MIN`, `TILO_ROOMS_GUEST_JOINS_PER_DAY`;
    raise them where many visitors share an address). The address is taken from
    `X-Forwarded-For` only when the request comes from a proxy listed in
    `TILO_ROOMS_TRUSTED_PROXIES`.
  - Online cap: at most `TILO_ROOMS_MAX_GUESTS` people online at once (default
    300). `/join` returns 503 with `Retry-After` above that. The page then
    shows the room read-only ("the room is full, so you're watching live")
    and asks again after `Retry-After`, joining once a seat frees up.
  - Per member: the relay's new `MemberLimits` token buckets per user root, 20
    changes per 10 s with a burst of 40, and 8 MiB of blobs per hour. A guest
    over the limit gets refusals, not a disconnect.
  - Bans: the service remembers which keyed hash of an IP (SHA-256 with a key
    on the host) each guest joined from, for 7 days. When a guest is removed
    or banned (`MemberRemoved`), that hash is put on a cooldown. `/join` from
    that hash is refused for 24 hours. No raw IP is ever stored.
- **Policy for guests.** The room's settings add a Deny on Outside for the
  Editor role at the room node, so guests cannot trigger outside effects from
  apps in the room. Installing apps is governance, so guests cannot install
  anything either.
- **Pruning.** Guests with no session for an hour (`--prune-after-mins`) are
  removed from the member list, a hundred to a governance change, and the
  room forgets their invitations and join receipts. Their messages stay,
  attributed to their last name; coming back, they join as a new guest. Every
  join costs the room time in proportion to its members, so it keeps the
  people around now rather than every visitor.
- **Declarative administration.** `TILO_ROOMS_OWNERS` and
  `TILO_ROOMS_MODERATORS` list user public keys. At start the host root makes
  them Owner or Moderator members with an ordinary `set-role`, and drops the
  role of keys removed from the lists. `TILO_ROOMS_BOTS` lists bot directory
  entries to install at first start (`so.tilo.bots.discord-bridge`), with their
  inputs, approved by the host-held root. `TILO_ROOMS_SEED_APPS` lists apps
  authored into the room at first start (`so.tilo.chat`), via
  `Rooms::author_application`. `TILO_ROOMS_CHAT_CHANNELS` names the
  channels Chat starts with while the room's workspace has none; each id is
  `c_<name>` with `-` as `_`, so the bridge's mapping finds the same
  channels on every start.
- **Presence.** `GET /presence` answers `{"online": n, "watching": m}` from the
  last `RoomEvent::Online`, cached for 2 s, with `Cache-Control: max-age=5`.
  `online` counts people (guests and members) with an open session; `watching`
  counts spectators. The site's "n people online" shows `online` only.
- **Public avatars.** The bridge's outbound webhooks need a public URL for the
  avatar of each Tilo author whose message goes to Discord. The URL is created
  the first time the bridge sends content by that author: the bot host writes
  that avatar blob (an image of at most 1 MiB) to
  `TILO_ROOMS_STATE/<name>/avatars/<blake3>`. The service serves it at
  `GET /avatars/<blake3>` with an immutable cache header and the sniffed type.
  Only avatars of authors whose content was actually bridged out are
  published. The URL is content-addressed, and there is no listing endpoint,
  so an avatar can be fetched only by someone who already has its hash.
  Discord avatars coming in are stored in the room's content store and are not
  published.
- **Framing.** Responses carry `Content-Security-Policy: frame-ancestors`
  with the origins in `TILO_ROOMS_SITE_ORIGINS`, so the Tilo app and the
  operator's own site can embed the room and nobody else can.
- **Links.** The relay announces the Tilo app's web entry
  (`TILO_ROOMS_WEB_ENTRY`, e.g. `https://app.example/`), so `share-link` and
  the bridge mint links in the ordinary grammar
  (`https://app.example/#/@rooms.example/w/<room>/so.tilo.chat/…`,
  [web-routing.md](web-routing.md)).

### Secrets

The Discord bot token is a Kubernetes secret encrypted with SOPS in the infra
repository and mounted read-only at `/run/secrets/discord-bot-token`. The token
is never in the image, the environment or a record. At start the service passes
the file to the connector's `set-bot-token` effect if the token's hash differs
from what is stored. That puts it in the host's `TILO_SECRETS_DIR` store on the
volume, which is the store the connector reads (bots.md §6).

The room's owner root arrives the same way, as an encrypted secret mounted
at `/run/secrets/room-root`. The operator generates it once and stores it
with their other encrypted secrets. Adding either secret
needs only the public age recipients in the infra repository's `.sops.yaml`
(`sops --encrypt`); no private key is involved. Persistent mode refuses
to start without it, so a restart can never mint a different owner. The service keeps
it in the host's `tilo-secrets` store (`SecretStore::item("room-root", <room
id>)`), never in a record, the environment or a log.

### Image and deployment

- **Image.** A new workflow, `.forgejo/workflows/room-service-image.yml`, builds
  `crates/hosting/tilo-room-service/container/Containerfile` on pushes to main
  that touch the room-service, relay, web shell or Chat, and on manual dispatch.
  It publishes `registry.tilo.so/tilo/room-service:sha-<commit>` (where the
  other service images live) and prints the digest. The
  image adds the Chat and Discord connector packages and `discord-worker` beside
  the relay.
- **Deployment.** One replica with a recreate strategy, because one
  governor owns the volume; a persistent volume for `TILO_ROOMS_STATE`; a
  service for 8790 (HTTP API) and 9780 (the relay's plain HTTP listener); an
  ingress for the host's origin that routes `/peer/` to 9780 and everything
  else to 8790; egress to DNS and TCP 443 on public addresses, since the
  Discord connector's outside worker enforces which hosts it calls. The
  image is pinned by digest, and a new release is a change that bumps it.
- **Routes.** The host is one origin (e.g. `https://rooms.example`), which
  is both the room API and what `origin` in `/join`'s answer names. A host is
  named for itself, not for a workspace on it, since it can serve many. The
  relay's browser routes all start with `/peer/`, so the ingress terminates
  TLS and sends `/peer/` to the relay's plain HTTP listener (`--web-plain
  0.0.0.0:9780`, published origin set with `--public-origin`) and every other
  path (`/join`, `/spectate`, `/presence`, `/mode`, `/errors`, `/avatars/*`)
  to the service; `/metrics` answers in-cluster only. The host serves no
  page: `GET /` redirects to the web entry, where the Tilo app lives. Through
  this route the live plane uses the HTTPS stream fallback, so an ingress
  that carries only HTTP over TCP is enough.
- **WebTransport (additive).** WebTransport needs UDP, which an HTTP ingress
  does not carry. `TILO_ROOMS_LIVE_ORIGIN` names a second origin (e.g.
  `https://live.rooms.example`) with UDP forwarded straight to the relay's
  QUIC listener; browsers try it first and fall back to the HTTPS stream
  through the ingress.
- **Environment.**

  | Variable | Value |
  |---|---|
  | `TILO_ROOMS_PERSISTENT` | the room's operator name, e.g. `plaza` |
  | `TILO_ROOMS_NAME` | the name people see, e.g. `Plaza` (the default) |
  | `TILO_ROOMS_CHAT_CHANNELS` | the channels Chat starts with, e.g. `general,help` (unset: none) |
  | `TILO_ROOMS_PUBLIC_API` | the host's origin, e.g. `https://rooms.example` |
  | `TILO_ROOMS_SITE_ORIGINS` | the web entry's origin and the operator's site, e.g. `https://app.example,https://site.example` |
  | `TILO_ROOMS_RELAY_ORIGIN` | the host's origin again |
  | `TILO_ROOMS_WEB_ENTRY` | the Tilo app's web entry, e.g. `https://app.example/` |
  | `TILO_ROOMS_ROOT_FILE` | `/run/secrets/room-root` (the default) |
  | `TILO_ROOMS_MAX_SPECTATORS` | `1000` |
  | `TILO_ROOMS_TRUSTED_PROXIES` | the ingress's address range |
  | `TILO_ROOMS_MAX_GUESTS` | `300` |
  | `TILO_ROOMS_SEED_APPS` | `so.tilo.chat` |
  | `TILO_ROOMS_BOTS` | `so.tilo.bots.discord-bridge` with `guild=<Discord server id>`, `channel=<id>` to bridge one channel only, `mirror-channels=yes` to make channels created in Tilo in Discord too, and `map-roles=yes` to mirror Tilo roles as the bridge's own Discord roles (both off by default) |
  | `TILO_ROOMS_OWNERS`, `TILO_ROOMS_MODERATORS` | the operators' user public keys |
  | `TILO_SECRETS_DIR` | a directory on the volume, e.g. `/state/<name>/secrets` |

### Embedding

A site that links to the room embeds the Tilo app's entry with `?embed=1`,
which shows the room read-only as a spectator until the visitor's first
click or key press, and can show `GET /presence`'s `online` as "n people
online". Both are on other origins than the site's, so its CSP must name
them: `frame-src` the web entry's origin and `connect-src` the host's. The
service answers the origins in `TILO_ROOMS_SITE_ORIGINS` with CORS.

## 10. Testing

- **Connector.** Unit tests of every verb's caller check: `map` and
  `observe-layout` by a person and an Outside verb by an agent are each refused
  on replay. `perform` refuses a caller that is not a bot holding the grant.
  The event log deduplicates by event id, prunes by window and keeps each
  installation's cursor. The worker against a recorded
  Discord gateway and REST transcript (a fake Discord server in the test
  harness), including resume failure, backfill and 429 handling.
- **Bridge program.** `tilo_automation` tests run the program against a Chat
  record and a guild record with synthetic events, checking each row of the §3
  tables, threads one level deep, echo suppression, reactions counted per
  persona, the attachment limits, and slugged names.
- **Layout and roles.** Reconcile tests: the same difference applied twice is a
  no-op, adopting the Tilo server's recorded layout changes nothing in it but
  webhooks, Discord-side renames and deletions are kept, only opted-in bridges
  make channels or roles and only their own, and role membership is mirrored
  only for linked accounts.
- **Linking.** Codes expire, a wrong code is refused, attempts are rate-limited,
  and a code from one person cannot link another.
- **Moderation.** A guest cannot edit or delete another's message. A Moderator can
  remove it, and the removal reaches Discord. Kicking, banning and unbanning a
  linked person do the same to their Discord account, and leave an unlinked
  guest's Discord side alone. A lock blocks posts on both sides.
  A Discord delete of a Tilo message by a non-Moderator is sent again.
- **Room-service.** Integration tests for persistent restart (same room id, same
  history), `/join` limits and the online cap, the hashed-IP cooldown, `PreAdmit`
  over operator protocol v2, and `/presence`.
- **End to end.** A real-boot journey: room-service in persistent mode with the
  fake Discord server, two headless browsers joining as guests, a message each way,
  an edit, a reaction, a moderator removal, a restart of the room-service in the
  middle of a burst, and a check that nothing is lost or duplicated on either
  side.
