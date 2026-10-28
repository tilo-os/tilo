# Slack bridge

The Slack bridge mirrors a room's Chat to a Slack workspace. A message posted
in either place shows up in the other, under its author's name and avatar.
It is the second user of the bot framework ([bots.md](bots.md)), and it is
put together exactly like the first: a connector holds the credentials and the
socket, a bot program decides what maps to what
([discord-bridge.md](discord-bridge.md), "The bridge bot"), and Chat learns
nothing about Slack. Reading that document first is the cheapest way into this
one; the sections below say what Slack makes different, not what Slack shares.

## 1. Pieces

```text
 Slack ◄── Events API (HTTP) / Web API / Socket Mode ──► outside worker (egress: slack.com …)
                                                             │ stdin/stdout
                                                  connector (App, no room identity)
                                                    actions ▲   │ trigger sources
 tilo-relay room --bots ── BotHost ──────────────────────────┘
                                   └─ bridge bot (bot:<installation id>) → chat.workspace ⇄ slack.workspace
```

- a **Slack connector** (`so.tilo.slack`), a native integration app on the
  room's host. It holds the bot token and the app's signing secret, speaks
  Slack's Web API, receives Slack's events and publishes them as trigger
  sources. It never acts in the room.
- the **Slack Bridge** bot (`so.tilo.bots.slack-bridge`), a Shortcut program
  published in the bot directory. It is the only room actor in the bridge.
- **Chat** itself, which needs no change: appearance, author checks, the
  Moderator role and channel locks are what any bot-carried bridge uses.

Any room may install the bridge against its own Slack workspace. It is not
tied to a hosted or public room.

## 2. The connector

The connector publishes one resource kind, `slack.workspace`, with one record
per bridged Slack workspace. The record is the bridge's memory: the workspace's
layout as the bot last saw it, the ids of both sides, its outbox, and the
links between Slack accounts and Tilo people.

### Record

```rust
// apps/slack/src/record.rs
#[record(kind = "slack.workspace")]
pub struct Workspace {
    #[readonly] pub team_id: String,             // Slack workspace id, "T0123456789"
    pub name: String,
    pub ownership: Ownership,                    // Importing | Adopted
    #[readonly] pub only_channel: Option<String>,// bridge one channel only
    pub token_item: Option<String>,              // secret item id; the token stays on the host
    pub signing_item: Option<String>,            // the app's signing secret, likewise
    #[readonly] pub threading: Threading,        // how Chat replies reach Slack (§4)
    pub bot_user: String,                        // the app's bot user id
    pub channels: Keyed<SChannel>,               // Slack's layout as last observed
    pub users: Keyed<SUser>,                     // the people Slack reports
    pub map: Keyed<Mapping>,                     // Tilo id ⇄ Slack id (§5)
    pub links: Keyed<Link>,
    pub pending_links: Keyed<PendingLink>,
    pub link_failures: Keyed<LinkFailure>,
    pub avatars: BTreeMap<String, String>,       // avatar hash → content address
    pub blocked_personas: Set<String>,           // Slack user ids
    pub tilo_only: Set<String>,
    pub slack_only: Set<String>,
    pub problem: Option<String>,                 // what the host shows when Slack refuses
    pub outbox: Outbox<Queued, SlackResult>,
}

pub enum Threading { Branches, ChatThreads }

pub struct SChannel { pub id: String, pub kind: SChannelKind, pub name: String,
                      pub is_private: bool, pub parent: Option<String>,
                      pub position: i64, pub topic: String, pub archived: bool }
pub enum SChannelKind { Channel, Private, Im }
pub struct SUser { pub id: String, pub name: String, pub real_name: String,
                   pub is_bot: bool, pub is_app_user: bool, pub deleted: bool,
                   pub avatar_hash: Option<String> }
```

### Verbs

Every verb is called by the bridge bot, and each checks that its caller's kind
is `ActorKind::Bot` in the app, so every replica refuses a member who forges
one ([bots.md](bots.md) §6). Undoable verbs write the record; outside verbs
queue an op the bot's runner drains, and take only ids the record already
knows, so a call can never aim the connector at another workspace.

| Verb | Kind | What it does |
|---|---|---|
| `connect(team_id, name, only_channel, threading)` | Undoable | Creates the record for a bridged workspace. |
| `observe-layout(channels, users)` | Undoable | Records Slack's layout and people as the latest layout event reported them. |
| `send`, `edit`, `delete`, `react`, `unreact`, `upload` | Outside | Queues the matching Web API call. |
| `apply-channel`, `delete-channel`, `create-channel` | Outside | Queues `conversations.create`, `conversations.rename`, `conversations.archive`. |
| `map`, `unmap` | Undoable | Writes and clears the id map. |
| `begin-link`, `complete-link`, `unlink`, `rename-link` | Undoable | Account links (§6). |
| `block-persona`, `unblock-persona` | Undoable, moderation | Stops mirroring one Slack account into Tilo. |
| `remember-avatar(hash, blob)` | Undoable | Copies an avatar into the content store once per hash. |
| `finish-import(tilo_only)` | Undoable | Ends an import pass (§7). |
| `set-credentials(token: Secret, signing: Secret)` | effect, the host's administrator | Stores the credentials in the host's secret store; the record keeps only the item ids. |

`send` names the channel, the author's name and avatar, the text, the time it
was written and the Tilo message it mirrors. A message Slack refuses because it
is too long is split by the worker, which is why `send`'s result carries one id
per chunk (§4).

### Trigger sources

| Source | Payload | Event id |
|---|---|---|
| `message-created` | channel, message id and thread timestamp, author (id, name, avatar hash, bot flag, app id), text, edited or not, files (name, size, type, private URL), the author's `subtype` when it is one Slack adds by itself | channel and message timestamp |
| `message-changed` | channel, message id, text or edit, `subtype` | channel, message, edit timestamp and what changed |
| `message-deleted` | channel, message id, the account that removed it when Slack reports one | channel, message and `deleted` |
| `reaction-added`, `reaction-removed` | channel, message, the account, its name and avatar, the emoji (Unicode, or a workspace emoji as `:name:`) | channel, message, account, emoji and add or remove |
| `layout-changed` | the workspace's channels and users, read after any of `channel_*`, `team_join`, `user_change` | the event's own id |
| `member-changed` | account, the workspace it is deactivated or reinstated in | the event's own id |
| `command` | `/link` or `/unlink`, the account, the code's SHA-256 (hashed by the worker as it arrives) | Slack's payload id |

The connector has no gateway to keep open: Slack pushes events to a request URL
or, in Socket Mode, over a WebSocket the worker holds. Either way the worker
verifies the app's signing secret (an HMAC over the request's timestamp and
body, with five-minute replay protection) before it reads an event, and
answers Slack's `url_verification` challenge. Events it has delivered before are
dropped by event id, and Slack's own retry — Slack resends an event nobody
acknowledged within three seconds — is therefore harmless. The worker
acknowledges an event *after* it is in the host's event log, so a crash
redelivers it rather than losing it. Slack keeps events for a shorter window
than Discord does (the Events API retries for about an hour), so the worker
backfills a mapped channel's recent history from `conversations.history` after a
reconnect, the same way the Discord worker does.

### Outside worker

The worker is the `slack-worker` binary in the connector's package. Its egress
allowlist is `slack.com` (the Web API, `https://slack.com/api/…`), the workspace's
own `*.slack.com` host for files it downloads, and, in Socket Mode, the WebSocket
endpoint Slack's `apps.connections.open` names. Nothing else.

Slack's rate limits are per method and mostly per-minute
(`conversations.history` and `conversations.replies` are stricter than the
rest), and every response carries `Retry-After` when it is refused. The worker
honours it, keeping the outbox entry the bot queued open, so a burst of Tilo
messages arrives late instead of being dropped — the same rule the Discord
worker follows.

## 3. The bridge bot

The bot program has `RecordChanged` triggers on `chat.workspace`,
`chat.messages` and `tilo.room` (membership and roles), on the record's own
`block-persona`, and a `Connector` trigger for each source above. It asks for
`Personas` appearance, and its derived grant is the connector's verbs, Chat's
`post`, `edit-message`, `delete-message`, `remove-message`,
`create-channel`, `delete-channel`, `set-channel-category`,
`create-category`, `delete-category` and the field edit of Chat's messages, plus
Read on both records, on the connector's app node and on the room.

### Tilo to Slack

| Tilo change | Slack call |
|---|---|
| `post` in a mapped channel | `chat.postMessage` with the author's `username` and their avatar as an image (Slack takes an image upload, not a URL, so the worker uploads an avatar once per hash and caches the URL). Mentions of linked people become `<@U…>`, and Chat's `@Name` of anyone else stays a plain name. |
| reply (`parent`) | `chat.postMessage` with `thread_ts` of the mapped root, or of the mapped message Slack threaded under (`threading: Branches`); a Chat thread of replies becomes one Slack thread (§4). |
| `edit-message` | `chat.update`. |
| `delete-message`, `remove-message` | `chat.delete`. |
| a reaction (the `react` / `unreact` field edit) | `reactions.add` once while at least one Tilo person holds the emoji, `reactions.remove` when the last one goes. A workspace emoji `:name:` crosses as `:name:` (§4). |
| attachment | `files.upload` into the mapped channel with the message's `thread_ts`, up to 1 GiB (Slack's per-file limit for a paid workspace; 100 MB otherwise), and the file shared with a line of text. Anything larger becomes a line with the message's link. |
| channel and category changes | Layout reconcile (§7). |

Messages by agents and other bots cross too, with a "🤖" prefix on the
username, because Slack shows a custom username with no other mark.

### Slack to Tilo

| Slack event | Tilo change |
|---|---|
| `message`, plain | `chat.post` in the mapped channel with `Appearance { name: the account's display name, avatar: the avatar blob, persona: slack/<user id> }`. |
| `message` with `thread_ts` | A reply in the Chat thread of the mapped root, or, with `threading: ChatThreads`, a reply that starts its own thread (§4). |
| `message_changed` | `edit-message` on the mapped message. |
| `message_deleted` | `delete-message` (the bot is the author of a mirrored message). |
| `reaction_added` / `reaction_removed` | The `react` / `unreact` field edit of the mapped message's reactions, under the reacting account's appearance. |
| `file_share` | The message's files, each up to 32 MiB, downloaded and posted as a Chat attachment; a larger one becomes a link to the file's workspace URL. |
| `subtype: bot_message`, `channel_join`, `channel_leave`, `channel_topic`, `pinned_item` from a person | Not mirrored: Slack adds them itself, and they are not what anyone said. |

### Slack's own noise

A Slack workspace and Tilo room do not hold the same things, and three of the
differences matter:

- **Edits are replays.** Slack's edit replaces a message wholesale rather than
  sending a diff, so the bridge applies the whole new text. Slack also edits
  its own message on occasions the person did not make (link unfurls); those
  arrive as a `message_changed` whose message still names the same text and are
  dropped.
- **Threads nest.** A Slack thread can hold threads that hold threads; Chat's
  threads are one level deep. `threading: ChatThreads` (the default) maps every
  message of a Slack thread, at any depth, into the one thread under the root,
  quoting the message it answers where the answer is not to the root. A reply in
  Tilo that answers one of those is posted with the `thread_ts` of the Slack
  root. `threading: Branches` mirrors Slack's nesting into separate Chat
  threads and loses the depth, so a bridge that needs a faithful one-to-one may
  choose it, and the room is told what it chose when it installs the bot.
- **`channel_join` is not a message.** Slack's Join and Leave lines never
  become Chat messages, because Chat shows who is in a room from presence and
  the room's members, not from lines like Slack's.

## 4. Messages, threads and emoji

### Long messages

Slack's per-message limit is 40 000 characters and Chat's is smaller, so a Chat
message always fits. The opposite direction does not: a Slack message is split
at paragraph boundaries when it exceeds Chat's limit, and the chunks are posted
in order as replies in the same thread, each one mapped so later edits and
deletes reach it. The first chunk keeps the original's time; the rest follow it.
A message that is only an emoji or a mention is never split mid-word.

### Reactions

Chat's reactions are the framework's `Reactions` field, changed only through the
`react` and `unreact` field edits (see
[discord-bridge.md](discord-bridge.md) §3, "Reactions"), and the bridge reads
them from the change's history entry. Slack has no reaction netspeak to
translate: a Unicode emoji crosses as it is, with or without its variation
selector, and a workspace emoji starts and ends as its own `:name:`. Slack's
built-in aliases (`:+1:` for 👍) are compared by the emoji Slack itself reports,
not by the alias anyone typed, so one 👍 never shows as two reactions.

## 5. The id map

The map holds one row per bridged object, keyed `kind:tilo-id`, with a reverse
index derived on read:

```rust
pub struct Mapping {
    pub tilo: String,          // workspace / category / channel / message / thread
    pub slack: String,         // C…, U…, or a message's channel and ts
    pub kind: MappedKind,      // Category | Channel | Thread | Message
    pub origin: Origin,        // Tilo | Slack
}
```

Slack has no categories. A channel sits in a *section*, and a section is a
per-member arrangement of the sidebar rather than workspace state, so the bridge
maps nothing for a Chat category: a Chat category is Tilo's own grouping and
Slack's channels arrive ungrouped. A Slack channel with no Tilo counterpart is `slack_only`
and listing it as a Chat channel is an import (§7). A channel's privacy is
Slack's and is not mirrored: a private Slack channel that someone bridges
becomes an ordinary Chat channel in the room, so the bridge refuses to map a
private channel unless a Moderator chose it in the import screen, which says
what becomes visible (§7).

## 6. Accounts and links

A Slack account is linked to a Tilo person the way a Discord one is, so that
mentions cross, moderation reaches, and a person's own Slack messages show as
themselves rather than as a persona ([discord-bridge.md](discord-bridge.md)
§5). The command differs only in what a person types in the app's slash
command:

1. The person chooses **Link Slack account** in Settings → Accounts. The device
   makes a twelve-character code, shows it, and writes its SHA-256 to the
   record (`begin-link`).
2. In Slack they run `/tilo link ABCDEFGHJKLM`. Slack sends the command to the
   worker, which hashes the code and publishes it as a `command` event; the
   pending link's hash and the event's must match, along with the workspace, or
   the link is refused.
3. The bridge writes the link (`complete-link`) and the account is linked.

A wrong code is refused and counted per account in a window, as on Discord:
Slack gives anyone in the workspace a place to guess, so ten failures in ten
minutes close linking for that account until the window passes. A link only
ever names the person whose code redeemed it.

## 7. Layout and import

Slack's channels are a flat list with names, privacy and topics. There are no
categories, so the reconcile is simpler than Discord's: it creates a Chat
channel for a new Slack channel, renames one Slack renamed, archives one Slack
archived, and never deletes. A Tilo channel with no Slack counterpart gets one
on request from the record's own `apply-channel`, and stays where it is when it
does not.

Import is one page in the connector with a single button, not a per-item copy:

- **Import an existing Slack workspace.** The bot creates the Tilo categories
  and channels for Slack's layout, then posts each channel's recent messages —
  as far back as `conversations.history` reaches within the pagination budget
  the connector states — with their authors' appearances, threads and
  reactions, each mapped. The whole import runs in one bot run, so the room's
  history shows it as one action by the bridge's installation, and undoing it
  removes what it created: the messages it posted and the channels it made.
- **What import does not do.** It does not bring Slack's pins, bookmarks,
  canvases, calls or files larger than Chat takes; it does not copy a private
  channel unless the person chose it on that page; and it does not make Slack
  mirror what it imported (the mapped messages are the bot's, so a later Slack
  edit or delete of one is a Slack-side change and is refused, as Slack cannot
  edit a message it did not write through the app).

## 8. Moderation

Moderation is Chat's own and decided by Tilo roles. The bridge carries the
effect across, in the same shape as Discord's
([discord-bridge.md](discord-bridge.md) §7):

| In Tilo | Effect in Slack |
|---|---|
| A Moderator runs `remove-message` | `chat.delete` on the mapped message. |
| A Moderator removes or bans a person with no linked Slack account | Tilo-only. |
| A Moderator removes a linked person | Their Slack account is not kicked: Slack has no server membership to remove, so the bridge does nothing there and the room's own refusal stands. |
| A Moderator bans a linked person | The account keeps its Slack side; the bridge stops mirroring anything from them (blocking the persona). |
| A Moderator runs `block-persona` | Their Slack messages stop being mirrored, and the ones already mirrored are removed. |
| A Moderator locks a channel | `conversations.lock` where Slack has it, and the bridge refuses to mirror a post into a locked channel even if Slack accepts one. |

| In Slack | Effect in Tilo |
|---|---|
| A Slack message deleted | The mirrored message is deleted. |
| A Tilo message deleted in Slack by someone the room's linked moderators name | `remove-message` in Tilo, attributed to the bot, with the deleter's link noted in the reason. |
| A Tilo message deleted in Slack by anyone else | Sent again by the bridge: a Slack-only permission must not remove a Tilo person's message. |
| A Slack account deactivated | The persona is blocked, and the bridge says so on the record (`problem`). |

Slack reports who deleted a message only in some plans; when it does not, the
bridge sees a `message_deleted` from Slack and treats it like any other, because
it cannot tell a Moderator's removal apart from a member's.

## 9. Failure handling

- **Order.** The bot's firings run one at a time in change order
  ([bots.md](bots.md) §4), so a message is mirrored before its edit or delete.
- **Duplicate-safe into Slack.** `chat.postMessage` accepts no idempotency key,
  and Slack's `client_msg_id` is only honoured for a message a person posts. A
  host that fails after Slack accepted a message but before the outbox entry
  settled drains the entry again. Before re-sending, the worker reads the
  channel's last 100 messages for one from this bot user with the same text
  posted after the entry was created. Finding one, it returns that message's
  `ts` as the result and the bot settles with it. A duplicate remains possible
  only when the channel moved past 100 messages in the meantime.
- **Exactly once into Tilo.** The event log deduplicates by event id, a run's id
  is derived from that id, and each Chat change is checked against history by
  its derived invocation id, so Slack's redeliveries and the worker's backfill
  never post twice.
- **Slack unavailable.** Outbox entries wait and the bridge's queue holds; Tilo
  keeps working, and the backlog drains in order at Slack's rate limits.
- **Token revoked, or the app uninstalled.** Slack answers `invalid_auth` or
  `token_revoked`. The worker reports it, every op fails with it, and the
  record's `problem` says "Slack no longer accepts the bot token", which the
  connector's page on the host shows.
- **Several hosts.** A room has one governor and therefore one host
  ([bots.md](bots.md) §10), so two workers never answer for the same event URL.

## 10. What this document commits to, and what it does not

This is the design the connector and its bridge are built against. It shares
every mechanism with the Discord bridge — the bot framework, the dispatcher,
appearance, the outbox, the id map and the reconcile — so the work is the Slack
half: the wire types, the worker, the plan, and the record. Where Slack's own
limits make a promise unkeepable (the last-100-message reconcile, the
deletion report on some plans), this document states the worst case rather than
hiding it.

Not part of this bridge: Slack calls. Slack's Calls API represents calls hosted
by an integrating app and does not expose Slack's own call media
([chat-shared-space.md](chat-shared-space.md)). The `open-slack-channel`
effect stays what it is: a browser deep link, not a native client.

## 11. Testing

The suite is the Discord bridge's, run against Slack's shapes:

- **Worker.** Against a recorded Events API and Web API transcript (a fake
  Slack in the test harness): signature check and replay window, the
  `url_verification` challenge, redelivery deduplication, `Retry-After`
  handling, `conversations.create/rename/archive`, `chat.update/delete`,
  reactions, file download and upload, and the reconcile read of the last 100
  messages.
- **Plan.** Table tests for every row of §3: the whole-text edit, the dropped
  unfurl edit, a nested thread's messages into one Chat thread, the long
  message split and its mapping, custom emoji both ways, and the `channel_join`
  that plans nothing.
- **Map and reconcile.** The same difference applied twice changes nothing;
  private channels are refused unless chosen; a Slack-side rename survives; a
  Tilo channel left without a Slack counterpart is not deleted.
- **Import.** One run creates the channels and the recent history with their
  appearances and maps, and undoing it removes exactly what it made.
- **Links.** Codes expire, a wrong code is refused and counted, and a code from
  one person cannot link another.
- **Moderation.** A guest cannot edit or delete another's message; a Moderator's
  removal reaches Slack; a Slack deletion by anyone else is re-sent.
- **End to end.** A real-boot journey: the connector on a room's host against
  the fake Slack, two headless browsers in Chat, a message each way, an edit, a
  reaction, a Moderator removal, a restart of the room's relay in the middle of
  a burst, and nothing lost or duplicated on either side.
