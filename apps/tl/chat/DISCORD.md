# Discord, folded into Chat

The Rust Discord app bridged one Discord server into the workspace's conversations. It is not a
separate app now: Chat holds the connected account, its server's channels and its direct messages
are Chat's conversations, and the Discord protocol client and the planners are engine crates. This
file lists every row of the Rust app and where it lives now.

The bridge is Chat's own origin: a Discord call is queued with `outside("discord.perform", …)` by
an action here, the origin carries it to the Discord worker as the person it maps, and what Discord
reports back is written through the same actions. A person's own edits and deletions reach Discord
as them; a Discord user's words reach Chat as they were written, under the name that service shows.

## Resources and fields

| Row | Home |
| --- | --- |
| The bridged server: its id and name, the one channel, mirroring and role options, ownership | `type Discord` in `apps/tl/chat/discord.tl`: `guild_id`, `server`, `only_channel`, `mirror_channels`, `map_roles`, `ownership` |
| The bot token, never in the record | the device's secret store, read by the origin; the record keeps `token_item` |
| Discord's channels: kind, parent, position, topic, overwrites | `DChannel` records in `Discord.channels`, written by `observe_layout` |
| Discord's roles: permissions, colour, position, managed | `DRole` records in `Discord.roles` |
| Discord's custom emoji | `Discord.emojis`, name → id |
| Every queued call and what became of it | the outbox entry of the `outside("discord.perform", …)` each action queues |
| Tilo ⇄ Discord ids for channels, categories, threads, messages, webhooks, roles and emoji | `Mapping` records in `Discord.map`, joined by `map` and forgotten by `unmap` |
| Linked accounts, their names and avatars, offers and sign-ins | `Link`, `LinkOffer`, `Connect` records in `Discord.links`, `link_offers`, `connects` |
| Avatars fetched once | `Discord.avatars` |
| Blocked Discord accounts | `Discord.blocked_personas` |
| Channels and roles gone from Discord; channels kept on one side | `vanished`, `tilo_only`, `discord_only` |
| Why voice or emoji do not reach Discord now | `voice_problem`, `emoji_problem` |

## Verbs

Each is an action on `Discord` in `apps/tl/chat/discord.tl`; one that asks Discord for something
queues it with `outside("discord.perform", …)`.

| Row | Action |
| --- | --- |
| Connect a server | `connect`, `bridge_server`, from the setup dialog |
| Record Discord's layout and emoji | `observe_layout` |
| Map and unmap Tilo ⇄ Discord ids | `map`, `unmap` |
| Send, edit, delete a message; react and unreact | `send`, `edit`, `delete`, `react`, `unreact` |
| Start a thread, create a webhook | `start_thread`, `create_webhook` |
| Create, delete, arrange a channel | `apply_channel`, `delete_channel`, `arrange_channels` |
| Create, delete a role; set member roles | `apply_role`, `delete_role`, `set_member_roles` |
| Kick, ban, unban a linked member | `kick`, `ban`, `unban` |
| Set a permission overwrite | `set_overwrite` |
| Copy a file from Discord's CDN | `fetch_asset` |
| Start linking an account; offer a link or a sign-in; complete a link; offer signing in | `begin_connect`, `offer_link`, `offer_sign_in`, `complete_link`, `set_sign_in`, `connect_account` |
| Answer a slash command | `answer_interaction` |
| Rename a link; keep its photo; unlink; unlink mine | `rename_link`, `set_link_photo`, `unlink`, `unlink_mine` |
| Block and unblock a Discord author | `block_persona`, `unblock_persona` |
| Remember an avatar; finish an import; settle an emoji; report voice | `remember_avatar`, `finish_import`, `settle_emoji`, `report_voice` |
| Upload and delete a workspace emoji | `apply_emoji`, `delete_emoji` |
| Find the bot's servers; keep its token | `find_servers`, `bridge_server` (through the engine) |
| Read the account's voice pairs | `voice_pairs` |
| Carry Chat's own messages, edits, deletions and reactions to Discord | `carry_message`, `carry_edit`, `carry_delete`, `carry_reaction` |

## Views and platform

| Row | Home |
| --- | --- |
| The page: server name, whether it is importing or bridged, channel and linked-account counts, the two problem banners, linked accounts (mine with link/unlink, everyone else's), the channels in Discord's order | `view account` |
| The empty state before a server is connected, with the one action that starts it | `view discord`, `action begin_setup` |
| The setup sheet: token, finding servers, choosing one | `view setup_dialog`, `find_servers`, `bridge_server` |
| One server per workspace | the account record; `connect` refuses a second |
| The server's categories and channels in the sidebar | Chat's own categories and channels, which the bridge makes and maps |
| The account's direct messages as direct conversations | Chat's `Direct` conversations, read by the people who connected the account |
| German | `locales/de-DE.ftl` |
| A bridge that runs with no window open | `trigger watch_discord` keeps Chat's instance on the device that carries the bridge |

## Tests

| Row | Test |
| --- | --- |
| Every verb's refusal and its queued call | Chat's tl tests, moved from `apps/discord/src/guild/tests.rs` |
| Plans: what a Discord event and a workspace change mean | `crates/engines/discord-bridge` tests |
| The worker's calls | `crates/engines/discord-worker` tests |
| The page, the empty state and the setup flow | Chat's tl tests |
| First-hour walk | `scripts/headless-examples/app-walks/chat.json`, `t test app-walk/chat` |

## Where the work stands

Written: the types, the verbs, the page and the setup above; the engine crates (`discord-wire`,
`discord-worker`, `discord-bridge`) with the planners reading Chat's tree; the language's
`discord.account` kind and its `discord.perform` capability.

Being built, not yet whole: `crates/integrations/tilolang-discord`, the origin. It has the engine
ops (`find-servers`, `set-token`, `sign-in`, `link-check`), the connector process that runs the
worker over the socket, and the reading of the account record and of Chat's tree into the
shapes `discord_bridge::plan` takes. What is missing before the Rust app can be deleted:

- the plan's Chat-sided lists run through Chat's actions (`relay`, `edit`, `delete`,
  `relay_reaction`, the channel and category verbs), with the results of the Discord-sided calls
  mapped when the worker answers them;
- file attachments both ways (the worker's blobs against Chat's content store);
- the walk against `tools/fake-discord`, and only then the deletion of `apps/discord` with its
  catalogue, packaging, `t.toml` and agent-session entries.

The Rust app stays until the walk passes: deleting it while the bridge is not whole would drop
the feature.
