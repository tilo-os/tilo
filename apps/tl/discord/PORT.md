# Discord: the Rust app's inventory, and where each row goes in tilolang

The app bridged one Discord server into the workspace's conversations: its bot's program only
sequenced calls, the app recorded what the bridge observed and queued what it asked Discord to do,
and planners worked out what each event meant. The protocol client and the planners are now
engine code (`crates/engines/discord-worker`, `crates/engines/discord-bridge`, with
`crates/engines/discord-wire` as their vocabulary). The rows below are the app's own surface; they
are what `apps/tl/discord` publishes, and each gets a test in
`crates/lang/tilolang-runtime/tests/e2e_discord.rs` (or a `.tl` test) in the port's push.

Status: the engine move is done and its 21 planner tests pass. The tl page, the verbs and the walk
below are the port still to write; the Rust app is not deleted until they are.

## Resources and fields

| Row | Tl home |
| --- | --- |
| `discord.guild` resource: server id and name, one channel bridge, mirroring and role options, ownership, channels, roles, emojis, mappings, links and offers, connections, sign-in page, avatars, blocked personas, vanished and one-sided sets, outbox, voice and emoji problems | `type Guild` (`tl.so.tilo.discord.Guild`), one root per workspace |
| A channel's kind, parent, position, topic, overwrites; a role's permissions, colour, position, managed | `DChannel`, `DRole` records in `Guild`, matching `discord-wire::event::{ChannelInfo, RoleInfo}` |
| Every queued op and its settled state | `outbox: Collection<Queued>`, `enum Op` mirroring `discord-wire::op::DiscordOp` |

## Verbs (each is an action on `Guild`)

| Row | Tl action | Notes |
| --- | --- | --- |
| Connect a server | `connect` | id shape, ownership starts `importing` |
| Record Discord's layout and custom emoji | `observe_layout` | remembers what vanished; forgets answered overwrites and finished orders |
| Map and unmap Tilo ⇄ Discord ids | `map`, `unmap` | `keep_apart` marks one-sided channels |
| Send, edit, delete a message; react and unreact | `send`, `edit`, `delete`, `react`, `unreact` | length and upload limits; one-channel confinement |
| Start a thread, create a webhook | `start_thread`, `create_webhook` | |
| Create, delete, arrange a channel | `apply_channel`, `delete_channel`, `arrange_channels` | mirroring only; only bridge-made channels move between categories |
| Create, delete a role; set member roles | `apply_role`, `delete_role`, `set_member_roles` | only roles the bridge made |
| Kick, ban, unban a linked member | `kick`, `ban`, `unban` | |
| Set a permission overwrite | `set_overwrite` | only the bridge's roles |
| Copy a file from Discord's CDN | `fetch_asset` | 32 MiB cap |
| Start linking an account; offer a link; offer a sign-in; complete a link; offer signing in with Discord | `begin_connect`, `offer_link`, `offer_sign_in`, `complete_link`, `set_sign_in` | ten-minute offers; state hashed |
| Answer a slash command | `answer_interaction` | 1–2000 characters |
| Rename a link; keep its photo; unlink; unlink mine | `rename_link`, `set_link_photo`, `unlink`, `unlink_mine` | |
| Block and unblock a Discord author | `block_persona`, `unblock_persona` | |
| Remember an avatar; finish an import; settle an emoji; report voice | `remember_avatar`, `finish_import`, `settle_emoji`, `report_voice` | |
| Read my link, a link offer, and plan an event | `my_link`, `link_offer`, `plan_event` | `plan_event` asks the engine's `plan`, which is the moved planner |
| Set the bot token; find the bot's servers | handled by the engine (`listen`, `find_servers`); the token never enters the resource | |

## Views and platform

| Row | Tl home |
| --- | --- |
| The page: server name, whether it is importing or bridged, channel and account counts, the two problem banners, the linked accounts (mine with link/unlink, everyone else's), the channels in Discord's order with kind marks | `default view` of `Guild` |
| The empty state before a server is bridged, with the one action that starts it | `ui.EmptyState` in the same view |
| The setup sheet: token, finding servers, choosing one, asking the host, asked, failed | view state and a modal, actions `find_servers` and `start_bridge` |
| One server per workspace | `root guild: Guild` |
| German | `locales/de-DE.ftl` |
| The bridge bot's program (`bridge/program.json`) | kept: it sequences the plan `plan_event` answers |

## Tests

| Row | Test |
| --- | --- |
| Every verb's refusal and its queued op | `e2e_discord` cases, moved from `apps/discord/src/guild/tests.rs` |
| Plans: what a Discord event and a workspace change mean | `crates/engines/discord-bridge` tests (21, green) |
| The page, empty state and setup flow | `e2e_discord` view cases |
| First-hour walk | `scripts/headless-examples/app-walks/discord.json`, `t test app-walk/discord` |
