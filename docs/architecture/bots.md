# Bots

A bot is a Shortcut that runs on its own. It is a Shortcut program
(`tilo_automation::Shortcut`) installed into a room. It runs headless on the
room's host every time one of its triggers fires, and it acts under an identity
of its own with exactly the authority its steps need. A connector is how a bot
reaches an outside service. It is an ordinary app on the host, a native
integration (Discord, a mail server, a webhook endpoint), that holds the
service's secret, exposes typed actions and publishes trigger sources for the
service's events. A connector never acts in a room itself. The bot that invokes
its actions is the only actor, so everything that happens in the room is
attributed to an installation someone reviewed.

Bots add no second way to act. A bot's step is an invocation on the action
door, and that invocation goes through the same `Caller` → broker → grant path
a person's click, an agent's plan and a Shortcut's step take
([permissions.md](permissions.md)). It is recorded the same way
([resources.md](resources.md) §5) and shown in the same views. Agents, bots
and a person's Shortcuts differ only in who decides what happens next:

| | Who decides the steps | Identity | Runs where | Authority |
|---|---|---|---|---|
| A person's Shortcut | the saved program | the person (`ActorKind::Shortcut`, for them) | the person's devices | person ∧ Shortcut grant |
| Agent | a model or a task, at run time | its own device and delegation (`Agent`) | an agent host | person ∧ agent delegation, capped at Editor |
| Bot | the installed program, reviewed once | its own device and delegation (`Bot`) | the room's host | installer ∧ derived grant |
| Connector | nobody: it only answers the bots that invoke it | none in rooms; its code runs as its app package | the room's host, as an outside worker | none of its own: the invoking bot's grant decides |

## 1. One principal model

A bot is a principal. Four enums name principals today, and each of them
changes as follows. No new enum is added.

| Enum | Crate | Today | Change |
|---|---|---|---|
| `Subject` (grant subject) | `tilo-action-policy` (`grant.rs`) | `Person`, `Role`, `Agent`, `Task`, `Shortcut`, `App`, `ForeignDevice` | add `Bot(String)` |
| `CallerPrincipal` (who a request is from) | `tilo-actions` (`remote.rs`) | `Person`, `Agent`, `App` | add `Bot(String)` and `CallerPrincipal::subject()` |
| `ActorKind` (presence, history) | `tilo-actions` (`interaction.rs`) | `Person`, `Agent`, `Shortcut`, `App` | add `Bot` |
| `Principal` (sandbox subject) | `collab-types` (`caps.rs`) | `Person`, `App`, `Agent`, `Token`, `Room` | unchanged |

The string in `Subject::Bot`, `CallerPrincipal::Bot` and the actor id
`bot:<id>` is the installation id (§2). It is a UUID minted when the bot is
installed. A package id would not work here: the same bot installed twice in a
room is two principals, each with its own grant and its own history.

```rust
// tilo-action-policy::grant
pub enum Subject {
    Person(String), Role(Role), Agent(String), Task(String),
    Shortcut(String), App(String),
    /// An installed bot, by installation id.
    Bot(String),
    ForeignDevice,
}

// tilo-actions::remote
pub enum CallerPrincipal { Person(String), Agent(String), App(String), Bot(String) }

impl CallerPrincipal {
    /// The grant subject this principal is decided as. The one mapping every
    /// door uses, instead of each matching on the principal by hand.
    pub fn subject(&self) -> Subject {
        match self {
            Self::Person(id) => Subject::Person(id.clone()),
            Self::Agent(id) => Subject::Agent(id.clone()),
            Self::App(id) => Subject::App(id.clone()),
            Self::Bot(id) => Subject::Bot(id.clone()),
        }
    }
}

impl Caller {
    /// A bot device whose certificate chains to `user_public_key` (its
    /// installer) and whose root-signed delegation names it installation
    /// `installation`. It acts for its installer and is never that person.
    pub fn bot(installation: impl Into<String>, user_public_key: [u8; 32],
               device_public_key: [u8; 32]) -> Self;
}
```

`Caller::actor()` maps `Bot` to
`InteractionActor { id: "bot:<installation>", kind: ActorKind::Bot, for_person:
Some(<installer>) }`. `ActorKind::acts_for_someone` holds for `Bot`.
`BrokerActor::for_caller` and the other hand-written matches
(`execution_activity.rs`, `witness.rs`, the relay's `agents.rs`) call
`subject()` or match the new arm.

A person's Shortcut keeps its current shape: the principal is the person and
`Caller::running_shortcut` names the run. It acts *as the person*, bounded by
the Shortcut. A bot acts *as itself*, for its installer. That difference is
the whole reason for a separate kind: history, presence and moderation can then
tell "Ada's Shortcut did this" apart from "the Discord Bridge Ada added did
this".

`collab_types::caps::Principal` stays as it is. It names who holds sandbox
capabilities (paths, devices, network hosts), and a bot holds none: its steps
run app code in confined workers that may touch nothing ([§5](#5-the-headless-runner)).
A connector's outside worker gets network access as its *package*, through the
`App(package)` principal every app already has. That is a sandbox grant, not
room authority: the connector has no action principal and never appears as an
actor in a room. If `caps::Principal` had a
`Bot` variant, a room grant could hand a bot filesystem or network access past
the action door. Leaving it out keeps the action grant (what a bot may do) and
the sandbox (what code may touch) apart, as [permissions.md](permissions.md) §7
requires.

## 2. Installations

An installation is authority, so it lives where a room's authority already
lives: in the room's governed state, beside its members and its delegated
agents (`tilo-peer-state`). It is published through the room's own resource
(`tilo.room`), so people, agents and other bots read it through the action door
like any other resource.

```rust
// tilo-peer-state (new): the governed row of an installed bot.
pub struct Installation {
    pub id: InstallationId,              // UUID, the Bot principal's id
    pub program: String,                 // BLAKE3 of the Shortcut program in the room's content store
    pub name: String,                    // shown name, from the program or entry
    pub icon: Option<String>,            // BLAKE3 of an image in the content store
    pub installed_by: String,            // the installer's user root, hex
    pub installed_at_ms: i64,
    pub appearance: AppearancePolicy,    // §7
    pub source: Option<DirectoryRef>,    // the bot directory entry it came from (§10)
    pub armed_at: BTreeMap<RecordId, Vec<ChangeHash>>, // heads at install: catch-up floor (§9)
    pub connector_cursors_from_ms: i64,  // connector events before this are never delivered (§9)
    pub paused: bool,
}

pub enum AppearancePolicy { Own, Personas }

// The program bytes are kept in the room's content store, so every member can
// read and check exactly what runs.

pub struct DirectoryRef { pub registry_owner: String, pub entry: String, pub version: String }
```

The installation's grants are not stored in this row. They are ordinary
`Grant`s in `RoomPolicy::grants` with `subject: Subject::Bot(id)` and
`granted_by: Subject::Person(installed_by)`. That way the relay's receipt check
(permissions.md §5.2), which already reads the room's grants, enforces them
with no new code path.

### Governance actions

Installing changes who may act in the room, so it is a governance action on
the room resource, decided like `grant` and `set-role`. These are new entries
in `tilo_action_policy::roles::governance`:

| Action | What it does | Privileged |
|---|---|---|
| `install-bot` | Adds the row, its grants and its delegation (below) in one governed change. | yes |
| `uninstall-bot` | Withdraws the delegation, revokes every `Bot(id)` grant and keeps the row as withdrawn, so history stays attributed. | yes |
| `set-bot-appearance` | Changes `AppearancePolicy`. | yes |
| `pause-bot`, `resume-bot` | Sets `paused`. The runner fires nothing while a bot is paused. | no |
| `adopt-bot` | Another member becomes `installed_by` after reviewing the grant, and re-signs the delegation. | yes |

Privileged means the installer's own grants must Allow the action; an Ask in
those grants does not count (permissions.md §1). By default only the Owner
installs. An Owner can grant `install-bot` to a role or a person like any other
governance action. Pausing is not privileged: a Moderator pauses a misbehaving
bot without being able to change what it may do.

`install-bot` requires `installed_by` to be the author of the change. The row
must also match the delegation that comes with it.

### Device and delegation

A bot acts from its own device key, as an agent does, and is never the device
of the person who installed it. The runner mints the key on the room's host.
The installer's root signs a delegation that names the device as installation
`id` of this room. The delegation goes through the same certification and
approval path as an agent's (`ApproverDecision::IssueAgent`, the
certification ALPN). Consent is the installer's install gesture on their
approving device. By default the host never holds the installer's root: in a
personal or team room the root stays on its owner's devices, and the host only
asks them to sign.

Public hosted rooms are the documented exception. A persistent public
room ([discord-bridge.md](discord-bridge.md) §9) has no owner present to
approve anything, so its owner root is generated for the room and kept on the
host in `tilo-secrets` (mounted from the operator's encrypted secrets, never in
a record). Its bots are installed by that root. The people who run the room hold
ordinary Owner or Moderator grants on their own identities, so moderation and
installs are attributed to them, not to the host's root.

Agents and bots share that delegation type. In `tilo-relay-protocol`,
`AgentDelegation` becomes `Delegation`, and `agent_id` becomes a typed
`delegate`:

```rust
pub enum Delegate {
    Agent { agent_id: String },
    Bot { installation: String },
}

pub struct Delegation {
    pub user_public_key: [u8; 32],         // the installer / delegating person
    pub device_public_key: [u8; 32],
    pub delegate: Delegate,
    pub label: String,
    pub color: [u8; 3],
    pub scope: DelegateScope,              // was AgentScope: rooms, apps
    pub issued_at_ns: u64,
    pub expires_at_ns: u64,                // delegations always expire
    pub signature: [u8; 64],
}
```

The signing domain changes with the type, so a delegation signed in the old
shape never verifies. `GovernedAgentRow` becomes `GovernedDelegateRow`
(`delegation`, `withdrawn_at_ns`, and for a bot the `Installation`), and
`RoomOperation::AgentMembership` becomes `DelegateMembership`. The rule
in `GovernedDelegateRow::acting` and `room_access::decide` stays the same for
both kinds. The delegation must verify against the certificate's own root,
name that device and this room, sit inside its validity window, and not have
been withdrawn. A bot row additionally needs its installation to be live and
`installed_by` to be the delegation's root. The governor admits a bot of any
member. `OTHER_PEOPLES_AGENTS_JOIN` still governs agents only, because the room
already decided on a bot when it allowed `install-bot`.

A bot delegation is issued for at most 30 days. Once less than a third of that
remains, the runner asks the installer's approving devices to renew it through
the same ALPN. In a public hosted room, where the installer is the host-held
root, the runner renews it on the host itself, with the same 30-day limit. Each
renewal is a governed `DelegateMembership` change, so it shows in the room's
history like any other. A delegation that expires leaves the bot paused with the visible
reason "its authorization expired". It never keeps acting on the old one.

### History and presence

Every change a bot commits names `ActorKind::Bot`, `bot:<id>` and its
installer (`ActionInvoker::on_behalf_of`). Readers render the actor from the
installation row: "Discord Bridge (bot, added by Ada) posted in #general". The
Fluent arguments are the existing `$actor` and `$for`, plus `$as` when the
action carried an appearance (§7). The installation's own page (from the room
resource: `tilo.room › bots › <id>`) lists the program, the derived grant as a
sentence, triggers, status, recent runs and the dead-letter list (§9).

## 3. Grants

A bot gets exactly the authority its program needs. Nobody writes that grant by
hand. It is derived from the program and reviewed once, at install.

- **Derivation.** `least_privilege(steps, catalog)` and `rules(permissions)`
  move from `apps/shortcuts/src/grant.rs` into `tilo_automation::grant`, so
  the Shortcuts editor and a headless install compute the same rules. Each
  `Invoke` step yields one rule: that action, on the one resource the step
  names (`Selector::Fixed`) or every resource of its kind
  (`FirstOfKind`, `FromBinding`), for the kind its descriptor has, with the
  answer Allow. Each `RecordChanged` trigger (§4) yields Read on the record
  kind it watches, and each `Connector` trigger yields Read on that connector's
  app node. An `Ai` step yields its lowered agent transform action.
- **What is stored.** For a bot, `Shortcut::grant` must equal the derived set
  exactly. `tilo_automation::check` refuses a bot program whose grant names
  anything its steps do not use. Room-scoped nodes replace the program's
  any-room nodes (`TreePath::root().any(Level::Room)`) at install, so a grant
  never reaches past the room it was installed in.
- **Review.** The install screen renders the grant with
  `tilo_action_policy::describe` ("Chat: post and edit its own messages; Discord:
  send messages as other names (outside Tilo)"). It also shows the triggers,
  the connectors the program needs, and the appearance policy. The installer
  approves all of it in one gesture.
- **Bound.** At every door and on receipt a bot acts with
  `Standing::person(installer).with_bound(Bound { subject: Subject::Bot(id),
  grants: <the installation's grants> })`. That is
  `min(decide(installer), decide(Bot(id)))`, the same formula as any delegation
  (permissions.md §4). A bot never does more than its installer can do right
  now. Demoting the installer narrows the bot at once; removing the installer
  pauses it until someone adopts it.
- **Unattended.** A bot is always unattended, and nobody is there to answer an
  Ask. An action that comes out as Ask is refused, which is the existing rule
  for unattended Shortcut runs (`RunMode::Unattended`). In practice an Outside
  step works only when the installer's own grants Allow Outside on that node.
  That is why an Owner, not an Editor, installs a bridge.
- **Never governance.** The derived grant may name no governance action on the
  room resource, and `install-bot` adds the `no-governance` Deny that agent
  admissions carry (`AgentPreset::grants`). A bot reads the room's roles; it
  never changes them.
- **Updates.** A new version of the program is a new `install-bot` over the same
  installation id. If its derived grant fits inside the old one, any member who
  may `pause-bot` can apply it. If it is wider, it needs the privileged review
  again, and until then the old version keeps running.

## 4. Triggers and the dispatcher

A trigger is a description of an event. The dispatcher turns events into runs,
and the same dispatcher serves a person's Shortcuts and bots.

### Two sources of events

The dispatcher takes events from two sources:

- **Record changes.** A chat message posted, edited or deleted, a reaction, a
  renamed channel or a changed role is a committed change to a record in the
  room. Such an event is durable, ordered, signed and permission-checked. Missed
  ones are the changes after a cursor (§9), and anyone in the room can inspect
  what fired a run.
- **Connector events.** A message in Discord or a mail arriving is not a room
  change. A connector publishes it as an event of one of its declared trigger
  sources (§6). The event never becomes a record change by itself, because a
  connector does not act in rooms. The bot it fires records whatever the room
  should keep.

`TriggerSpec` gains two variants, and `IncomingMessage` becomes a filter over
record changes:

```rust
// tilo_automation::shortcut
pub enum TriggerSpec {
    // …existing variants…
    /// A committed change to a record of `kind` in the room.
    RecordChanged {
        kind: String,                      // "chat.workspace", "discord.guild", "tilo.room"
        #[serde(default)] actions: Vec<String>,  // verb public ids; empty with `fields` empty = any
        #[serde(default)] fields: Vec<String>,   // field-edit path patterns: "channels.*.name"
        #[serde(default)] by: ActorFilter,       // Anyone (default), People, NotBots
    },
    /// An event from a connector's trigger source on this host.
    Connector {
        app_id: String,                    // "so.tilo.discord"
        source: String,                    // a declared source: "message-created"
        #[serde(default)] filter: Map<String, Value>, // equality on top-level payload fields
    },
    IncomingMessage { app_id: Option<String>, from: Option<String> },
}
```

`IncomingMessage` matches changes made by a verb its app marks
`#[action(incoming_message)]` (a new flag in `tilo-resource-macros`, written
into `surface.toml`). Chat marks `post` and `share-resource`; Messages marks the
verb that records a received message. It matches only changes by someone other
than the Shortcut's saver. The trigger has existed since Shortcuts were
introduced, but nothing ever fired it. This flag is what lets the dispatcher
fire it without knowing any app's schema.

### What a firing carries

The dispatcher reads the history entry the change carries
(`tilo_actions::entry`, `tilo.entry/3`). It also reads the full recorded action
(`ActionContext::action`, with its content references resolved) and the item it
touched, read through the resource's own read door at the change's parent heads
and at its own heads:

```rust
// tilo_automation::dispatch (new)
pub struct RecordEvent {
    pub room: String,
    pub record: RecordId,
    pub kind: String,
    pub change: ChangeHash,               // the event's identity
    pub actor: InteractionActor,
    pub appearance: Option<Appearance>,   // §7
    pub did: Did,                         // verb (ActionKey) or field edit (path)
    pub arguments: Map<String, Value>,    // the full recorded arguments
    pub item: Option<ItemRef>,            // kind, id, label
    pub before: Option<Value>,            // the item as read before the change
    pub after: Option<Value>,             // and after (None when it was removed)
    pub caused_by: Option<Cause>,         // the run that made this change, if any
}

pub struct Cause { pub installation: String, pub event: ChangeHash, pub depth: u8 }
```

A connector event carries the connector's declared payload with a stable id
from the service:

```rust
pub struct ConnectorEvent {
    pub app_id: String,
    pub source: String,
    pub id: String,                       // the service's id for the event: its identity
    pub at_ms: i64,
    pub payload: Map<String, Value>,      // typed by the source's declared parameters
}
```

The payload binds to the program's inputs by name and type, as
`TriggerSpec::payload` already does for other triggers. A bot declares an input
`event` of type object and reads `event.after.text`, `event.actor.id` and so on
with `Binding::StepOutput`-style pointers.

### Rules the dispatcher applies

The matching core is pure and lives in a new module, `tilo_automation::dispatch`:

1. **No self-triggering.** A change whose actor is `bot:<id>` never fires
   installation `id`. For a person's Shortcut, a change committed as one of
   that Shortcut's runs never fires the same Shortcut.
   `tilo_automation::check` already refuses a Shortcut that could invoke
   itself. This rule covers the indirect case of a run whose output matches its
   own trigger.
2. **Bounded chains.** A run started by a change that itself came from a run
   records `Cause { depth: parent + 1 }` on every change it commits (a new
   optional `ActionContext::caused_by`). The dispatcher fires nothing at depth
   above 4. Two bots echoing each other therefore stop after four hops, and
   the dead-letter list shows where.
3. **Deterministic runs.** A run's id is derived from what fired it:
   `RunId = uuid(blake3("tilo.automation.run\0" ‖ installation-or-shortcut ‖
   trigger index ‖ change hash or connector event id))`. `invocation_id(run, path)` is already
   derived from the run, so a run fired twice by the same change invokes the
   same `InvocationId`s (§9).
4. **Order.** Firings of one installation run one at a time, in the causal
   order of the changes that fired them (Automerge change order within a
   record, then by the time the change was received), and connector events in
   the order their source delivered them. Different installations
   run concurrently. This ordering is what lets a bridge rely on a post being
   mirrored before its edit.
5. **Authority at fire time.** When a run starts, the dispatcher checks that the
   installation is live, not paused, and that its delegation is current. A
   change that was admitted is not by that fact a reason to act.

### Hosts

- **The room's host** runs bots (§5). It feeds the dispatcher from the changes
  its participants merge and from the trigger sources of the connectors
  installed on it.
- **A person's devices** run that person's Shortcuts. The shell's
  `automation_triggers.rs` gains `RecordChangedTriggers` and
  `IncomingMessageTriggers` sources, fed from the shell's record replication,
  next to the existing keyboard, schedule and file sources. They arm only the
  saver's Shortcuts in their room (`armed`). A Shortcut reaches all of its
  saver's devices, so a record trigger uses the schedule election that already
  exists: online devices rank themselves by a hash of the change and the first
  one runs it. The others wait `CLAIM_GRACE` per rank, then look for the run's
  first change (its derived `source_invocation`) before taking over. The run's
  own first change is the claim, so record triggers need no separate claim
  record.

## 5. The headless runner

Bots run as replicating participants on the room's host, in the same process
and with the same machinery that runs agents there.

- **Where.** A `BotHost` (new, `crates/agent/agent-participant/src/bot.rs`)
  sits beside `AgentHost`, and `tilo_relay::bots::start` (new, feature `bots`)
  wires it into the relay as `tilo_relay::agents::start` does for agents.
  `tilo-relay run --bots`, `dev --bots` and `room --bots` enable it. A relay
  runs bots only for rooms it governs. A room has one governor, so it has one
  runner and needs no election.
- **Participation.** Each installation is a participant with its own session,
  device key and delegation (`AgentParticipant` with a Shortcut interpreter in
  place of the planner). It keeps replicas of the records its triggers and
  steps name, each in its own confined app worker (`ConfinedReplica`). That
  worker can open no path or socket, exec nothing and signal no other process.
  The host keeps the keys and signs only what it has admitted through
  `tilo-automerge-isolate` (browser-runtime.md, "Agents on a host";
  security.md, "App workers for host agents and room actions").
- **Running.** A firing becomes `tilo_automation::run` with
  `RunMode::Unattended`, `RunTrigger::Fired(trigger)`, `Caller::bot(..)`
  confined to the room, and the event bound to the inputs. Each step is an
  invocation through the participant's `HostActions` door, which decides with
  the bot's `Standing` (§3) before dispatch. An Outside step completes when its
  outbox entry settles (`cx.outbox_awaited`), so the next step can use the
  result, such as a Discord message id. The runner settles it after the
  connector has performed the op (§6).
- **Visible.** Because actions happen in views (resources.md §4), the bot's
  participant publishes presence (a participant of kind bot, labelled with the
  installation's name) on the record it works in. People see it act there the
  way they see an agent.
- **State.** `<state>/bots/bots.json` (owner-only, written as `agents.json` is)
  holds the bot device keys, delegations, per-installation cursors (§9),
  backoff timers and the dead-letter list. On start the file is restored, each
  live installation resumes from its cursor, and a run that was in flight is
  run again under its deterministic id.
- **Limits.** At most 64 queued firings per installation. Beyond that the oldest
  are dropped into the dead-letter list with the reason "fell behind", and the
  bot's page shows it. Each run has a 60 s budget per step (Outside steps
  included), and a step that runs out fails the run.

## 6. Connectors

A connector is how a bot reaches a service outside Tilo. It is an ordinary app
package installed on the room's host. It holds the service's credentials,
exposes typed actions, and publishes trigger sources for the service's events.
It never acts in a room. It has no installation, no device key, no delegation
and no grant of its own, and no change in any room names it as its actor.
Everything a connector does in a room happens because a bot invoked one of its
actions or ran on one of its events. Each such change is the bot's, decided by
the bot's grant and shown with the bot's name. A connector exposes power. Only
the reviewed bot decides when it is used.

```text
            room host (tilo-relay --bots)
 ┌──────────────────────────────────────────────────────────────┐
 │  BotHost                                                     │
 │   ├─ bot participants (Bot principals, the only room actors) │
 │   │    └─ ConfinedReplica per record                         │
 │   ├─ dispatcher ◄── trigger sources ──┐                      │
 │   └─ outbox drain ── actions ───────► connector (App)        │
 │                                        └─ outside worker     │  secret, egress allowlist
 └──────────────────────────────────────────────────────────────┘
```

- **Record.** A connector may publish a resource kind for the state bots keep
  about the service, for example `discord.guild` with the bridge's id map and
  account links. Its verbs are pure, as every verb is (resources.md §2.3), and
  people and bots invoke them like any app's. The connector's host process
  never writes to the record.
- **Actions.** An Outside verb on that record pushes an `Outbox<Op, Result>`
  entry, owned by the installation that invoked it (resources.md §2.4). The
  bot's runner drains its own entries. It hands each op to the connector on the
  same host through the connector's host action `perform(op)`, gets the result
  back, and settles the entry as the bot (`Settled::Done` / `Settled::Failed`).
  The request to the outside service, its result and the settle are therefore
  all attributed to the bot. `perform` is published only on the host's local
  action door. It accepts only a `Caller::bot` whose installation holds a live
  grant on the verb that queued the entry, and it never takes arguments the
  record did not queue.
- **Trigger sources.** The connector's manifest declares its sources (name,
  payload parameters, the service id that identifies an event), as an app
  declares its actions. Its outside worker keeps the service's event stream
  open (a WebSocket gateway, IMAP IDLE, a webhook listener). It appends each
  event to a host-private log (`<state>/apps/<package>/events`, at most 24 hours
  or 10 000 events) and hands it to the dispatcher (§4). Each bot installation
  acknowledges events with its own cursor in `bots.json`, so an event delivered
  while a bot was down or paused is still there when it resumes. Events are
  deduplicated by their service id before they are appended. Nothing in a room
  can produce a connector event, so a member cannot forge one.
- **Who may write the record.** Verbs only bots may use (the mapping verbs of
  [discord-bridge.md](discord-bridge.md) §6) check in the app that the author's
  kind is `ActorKind::Bot` (`ActionCx::invoker()`, new, which returns the
  `ActionInvoker` that `ActionCx::author()` already reads from). Every device
  replays these checks, so a member who forges a mapping is refused everywhere.
  Their outbox entry is never drained either.
- **Outside worker.** The worker is the connector package's own binary, started
  by the host. On a Tilo OS host it runs under the launcher with
  `[sandbox.net] mode = "allowlist"` and the hosts its manifest declares, pinned
  to the egress gate ([security.md](security.md)). In a container it is the same
  binary with no writable path and its only route out through `tilo-egress` with
  the same `EgressPolicy`. It talks to the host over its stdin and stdout with a
  typed, versioned protocol (`Perform { op }`, `Result { result }`,
  `Event { source, event }`, `Secret { purpose, bytes }`). It never sees a device
  key, a replica or any room's data beyond the ops it is asked to perform.
- **Secrets.** A connector's credentials (a bot token, a webhook token) are
  stored with `tilo-secrets`, scoped to the connector's package
  (`SecretStore::for_app(package)`). Each one is keyed by the item it belongs to
  (`SecretStore::item(purpose, id)`). A record names the item and the secret
  stays in the host's store. This is the pattern storage.md §13.6 prescribes,
  and the one the AI settings hub uses for provider keys (a non-secret
  reference in the profile, resolved by the process that owns the store). A
  headless host keeps that store in `TILO_SECRETS_DIR` on an encrypted volume.
  The host's administrator sets a credential through a device-only effect the
  connector publishes, such as `set-bot-token(token: Secret)`, invoked on the
  host. A `ParamType::Secret` argument is redacted wherever an invocation is
  shown or recorded, and an effect writes no change, so the secret never
  reaches a record, history or a relay.
- **Grant.** A connector has none. A bot that uses it holds grants on the
  connector's verbs in its room, derived from its steps like any other (§3).
  The connector's trigger sources are covered by a Read rule on the connector's
  app node, which the derivation adds for each `Connector` trigger.
- **Directory.** A bot entry names the connectors it needs (§10). The install
  screen lists them, and installing the bot needs the room's host to have them
  installed and configured. A host's administrator installs a connector like
  any other app on that host.

## 7. Appearance

A bot may act under another name and avatar, as a Discord webhook does. A
bridge uses this to post a Discord user's message under that user's name. The
bot remains the author. Appearance is presentation recorded in provenance,
never identity.

```rust
// tilo_actions::context (new)
pub struct Appearance {
    pub name: String,                 // 1..=80 chars after trimming
    pub avatar: Option<String>,       // BLAKE3 hex of an image in the content store
    pub persona: Option<Persona>,     // the outside account this stands for
}

pub struct Persona { pub service: String, pub account: String }  // "discord", "80351110224678912"

pub struct ActionInvoker {
    pub actor: String,
    pub kind: ActorKind,
    pub on_behalf_of: Option<String>,
    pub participant: Option<ParticipantId>,
    /// The name and avatar a bot showed for this action. Set only by the door
    /// that admitted the invocation, and only for a bot allowed it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub appearance: Option<Appearance>,
}
```

- **Where it comes from.** `InvokeRequest` gains an optional `appearance`. A
  Shortcut's `StepKind::Invoke` gains an optional `appearance` with bindings for
  its name, avatar and persona. The door accepts an appearance only when the
  caller is `CallerPrincipal::Bot` and its installation's policy is `Personas`.
  For any other caller, a person or agent included, the door refuses the
  invocation rather than dropping the field. `check` refuses a Shortcut whose
  steps carry an appearance unless it is being installed as a bot that asks for
  `Personas`, and the install screen shows that request on its own line:
  "May post under other names and avatars. Always labelled BOT."
- **Who may allow it.** Allowing appearance is part of the privileged
  `install-bot` review, and changing it later is `set-bot-appearance`, also
  privileged. A connector has no changes of its own to carry one.
- **Recorded.** The appearance is part of the change's `ActionContext`, and so of
  its history entry. The compact entry codec (`tilo_actions::compact`) gains
  the words for its fields. Apps read it as `ActionCx::appearance()`. An app may
  keep it on the item it creates. Chat stores it on the message
  ([chat-shared-space.md](chat-shared-space.md), "Bridges").
- **Checked on receipt.** A device that replays a change carrying an appearance
  refuses it unless the change's author is a bot device whose installation
  allows `Personas`. The relay cannot see the context in a sealed room, so this
  check belongs to devices, like every other replay check (permissions.md
  §5.2).
- **Validated.** The name is NFC-normalized, may not be empty, and may not
  contain control, bidirectional-override or zero-width characters. The avatar
  must name a blob of at most 1 MiB whose sniffed type is PNG, JPEG, WebP or
  GIF. A failing value refuses the invocation and is never silently cleaned up.
- **Rendered.** One shared helper renders every author, so no app can forget
  the label: `ui::author` (new, `crates/sdk/ui-core/src/author.rs`) next to
  the existing `agent_identity` mark.

  ```rust
  pub enum Author<'a> {
      Person { name: &'a str, avatar: Option<&'a BlobRef>, initials: &'a str, color: Color },
      Agent  { name: &'a str, for_person: Option<&'a str>, color: Color },
      Bot    { bot_name: &'a str, installed_by: &'a str,
               appearance: Option<&'a Appearance>, linked: Option<&'a str> },
  }
  ```

  A person gets the circular `widget::Avatar`, which now draws their image when
  they set one. A bot gets a rounded-square tile, a shape no person's avatar
  uses. The tile shows the appearance's avatar or the bot's icon and carries a
  `BOT` chip that always stays visible, even when the name is truncated. The
  shown name is the appearance's name if there is one, otherwise the bot's
  name. The chip opens the installation's page and names the bot and its
  installer: "alice, via Discord Bridge (bot, added by Ada)". If the appearance's
  name matches the display name of a current room member, the helper appends
  the persona's service ("alice · Discord"). The match is case-folded and
  compared by UTS #39 confusable skeleton. If the persona is linked to a Tilo
  person ([discord-bridge.md](discord-bridge.md) §5), the profile says so,
  but the author is still the bot.

  Not yet built: apps do not receive installation rows. Today a member's
  session passes apps the room's verified agents (`HistoryAgents`), but no
  bots, and a browser member keeps no governance replica at all. Until
  installations reach apps the same way, the chip is drawn but opens nothing,
  and the installer and grant are not shown.

## 8. Security model

What a bot can never do:

- Act beyond its installer's current authority, or beyond its derived grant.
  Both are checked at the runner's door and again on receipt, by the relay
  (grants and storage units) and by devices (replay).
- Govern the room. It cannot invite, remove, change roles, grant, revoke,
  install or uninstall, whatever its installer may do.
- Act without being visible. Every change names `ActorKind::Bot`, its
  installation and its installer, and every appearance carries the `BOT` label.
- Appear as a Tilo person. Appearance is refused for non-bots, rendered only
  through `ui::author`, and never changes the author. A person's display name,
  avatar and presence come only from their own changes.
- Answer an Ask, or confirm its own destructive or privileged action.
  Unattended runs refuse Ask, and privileged actions need the person's own
  Allow.
- Touch files, devices or the network. It has no sandbox capability, and its
  steps run in confined workers. Only a connector's outside worker reaches the
  network, only to its declared hosts, and it holds no keys.
- See a secret. `ParamType::Secret` values cannot be bound into text
  (`Ty::renders_as_text`), and connector secrets never leave the host's store.
- Outlive its authority. Uninstalling, withdrawing the delegation, expiry, or
  the installer losing membership stop it at the next run start. Members refuse
  its changes from that point on.
- Trigger itself, or chain with other bots past depth 4.

Invariants the implementation keeps:

- One function decides (`tilo-action-policy`), and every door builds the bot's
  `Standing` the same way.
- An installation, its grants and its delegation are added, withdrawn and
  narrowed by one governed change, never by host-local state alone.
- A bot's program is content-addressed in the room. What members read is what
  runs, and an update is a reviewed governance change.

## 9. Failure handling

- **Runner restart.** Cursors and in-flight runs are in `bots.json`. On start
  each installation computes, per watched record, the changes after its cursor
  heads (Automerge `get_changes(heads)`), and the connector events after its
  cursor in each connector's event log. It dispatches them in order (§4) and
  runs any interrupted run again under the same `RunId`.
- **Lost cursor.** If the cursor is missing or unreadable, the runner starts from
  the installation's `armed_at` heads, which the governed row records at
  install. It never starts from the beginning of the record, so installing a
  bot does not replay a room's whole past through it. For connector events the
  floor is `connector_cursors_from_ms`, the install time: the runner delivers
  the events received after it that are still in the log. An event older than
  the log's window is gone. A connector's own backfill (below) covers that gap where the service
  allows it.
- **Idempotent steps.** Every step's `InvocationId` is derived from the run,
  and the run from the firing change or connector event. Before invoking, the runner looks in the
  target record's history for a change by this installation whose
  `source_invocation` is that id. If it finds one, the step observes it and
  takes its output, as a retried invocation would from the host's
  `InvocationLedger`. The ledger itself is memory-bounded, but the room's
  history is the durable journal for record actions, so the check survives
  restarts and eviction.
- **Outside effects.** An outbox entry is settled once, and only by the bot
  that owns it.
  If the host fails after the outside request succeeded but before the settle
  was committed, the request may be repeated when the entry is drained again.
  A connector closes that gap with the service's own deduplication where one
  exists (a nonce or idempotency key). Otherwise it reconciles before
  re-sending: it reads what the service holds for that entry
  ([discord-bridge.md](discord-bridge.md) §8). The worst case is stated per
  connector, never hidden.
- **Failed runs.** A step failure stops the run (`StepFailure`). A transient
  failure (the service unavailable, a timeout, `ActionHostError::Unavailable`)
  is retried with exponential backoff from 1 s to 10 min, up to 8 attempts,
  holding the installation's later firings back so order is kept. A refusal
  (Deny, an invalid argument, an app's `Refusal`) is not retried. It goes to
  the dead-letter list with the event, the step and the reason, and the
  installation moves on. The bot's page shows the list, with "run again" for
  each entry (a person's ordinary invocation of the bot's `run` action with
  that event).
- **Poison events.** A firing that keeps failing transiently after 8 attempts is
  dead-lettered too, so one bad event cannot stall a bot forever.
- **Installer gone.** If the installer is removed from the room or blocked, the
  bot pauses with that reason. An Owner may `adopt-bot`.
- **Connector down.** If the outside worker exits, it is restarted with backoff.
  Outbox entries wait, and bots whose steps are waiting on them hold their
  queue. Events the service resends on reconnect are deduplicated by their
  service id before they reach the event log. Events the service does not
  resend are fetched by the connector's own backfill (per connector).

## 10. The bot directory

Bots are published in the same registry as apps (`apps.tilo.so`,
[../registry.md](../registry.md)), as a new entry kind. Publishers, namespaces,
signing, review and moderation are all shared. What differs is the artifact: a
signed program as data, not a `.tapp`.

```rust
// registry-types::entry
pub enum EntryKind { App, Bot }

pub struct RegistryEntry {
    pub package_id: String,
    pub kind: EntryKind,                  // new; omitted on the wire for apps
    // …name, description, tagline, images, languages, categories, urls,
    //    publisher_pubkey_hex…
    pub versions: Vec<RegistryVersion>,
    pub updated_at_unix: u64,
    pub signature_hex: String,
}

pub struct RegistryVersion {
    pub version: String,
    pub released_at_unix: u64,
    pub artifact: VersionArtifact,        // a tapp encodes as the flat tapp fields
}

pub enum VersionArtifact {
    Tapp {
        tapp_url: String, tapp_root_hash_hex: String, tapp_size_bytes: u64,
        tapp_blake3_hex: String, target: RegistryTarget,
    },
    Bot {
        program_url: String,              // HTTPS, content-addressed under /blobs/
        program_blake3_hex: String,
        program_size_bytes: u64,          // at most 256 KiB
        requires: BotRequirements,
    },
}

pub struct BotRequirements {
    pub connectors: Vec<PackageRequirement>,  // package id + minimum version
    pub apps: Vec<String>,                    // apps whose actions the steps invoke
    pub appearance: bool,                     // asks for `Personas`
    pub triggers: Vec<String>,                // trigger kinds, for the listing
}
```

- **Signing.** A bot entry signs under `tilo-registry-bot-entry-v1\0`, and
  snapshots list it among the app entries, under the snapshot's one
  signature ([../registry.md](../registry.md)).
  The program file is the Shortcut's canonical
  JSON. Its BLAKE3 is signed in the entry, and the registry stores the bytes as
  a content-addressed blob like a thin package's payload. Publishers build
  the entry with `tilo-app-package bot-entry` and upload the program to
  `PUT /uploads/<blake3>.program` before submitting it.
- **Validation.** The registry checks `requires` against the program on
  submission: every app and connector the steps invoke or the triggers watch is
  listed, `appearance` is set exactly when a step carries an appearance, and
  the program passes `tilo_automation::check`'s structural checks. Typing
  against live descriptors happens at install, against the installing room's
  catalogue.
- **Install.** The Store shows a bot entry with its publisher, triggers and
  connectors, plus the grant it would derive in the selected room, rendered as
  the sentence the installer approves (§3). Installing fetches the program,
  verifies its hash against the entry, and checks that the room's host has the
  required connectors installed and configured. If one is missing, the screen
  says which, and the host's administrator installs it from its app entry. It
  then invokes `install-bot` with `source: Some(DirectoryRef)`.
  `tilo_bot_directory` does the fetch, verification, grant derivation and
  connector check for the Store and the room service alike;
  `registry_types::DirectoryRef` names the listing an installation came from.
- **Updates.** A newer version is offered like an app update. Applying it
  follows §3: automatic when the grant fits inside the old one, reviewed when it
  is wider.
- **Moderation.** Publisher allowlists, staged review, takedowns and the
  moderation log apply unchanged (registry.md, "Moderation policy and
  operations"). A takedown marks installations whose `source` names the entry,
  and their rooms show it. The room decides whether to uninstall, because a
  registry cannot reach into a room.

## 11. Where the code lives

| Crate / path | Change |
|---|---|
| `crates/collab/action-policy` | `Subject::Bot`. New governance actions (§2) and the `no-governance` bound for bots. `Role::Moderator` and the `moderation` field convention ([permissions.md](permissions.md) §3). |
| `crates/collab/actions` | `CallerPrincipal::Bot`, `CallerPrincipal::subject`, `Caller::bot`, `ActorKind::Bot`. `Appearance`, `Persona`, `ActionInvoker::appearance`, `ActionContext::caused_by`, `InvokeRequest::appearance`. Compact codec words. |
| `crates/collab/automation` | `TriggerSpec::RecordChanged`, `IncomingMessage` matching, `dispatch` module (new), `grant::least_privilege` / `rules` (moved from `apps/shortcuts`), appearance on `StepKind::Invoke`, bot rules in `check`. |
| `crates/collab/tilo-peer-state` | `Installation`, `GovernedDelegateRow` (was `GovernedAgentRow`). |
| `crates/collab/workspace-join` | `delegation::{Delegation, Delegate}` (was `AgentDelegation`), below `tilo-peer-state`; `tilo-relay-protocol` re-exports the module as `tilo_relay_protocol::delegation`. |
| `crates/hosting/tilo-relay` | `bots` feature and `--bots`. `room_access` and certification accept bot delegations. |
| `crates/hosting/tilo-relay` `room_bots/news_club` | The News Club (`--news-club-key-file`): a hosted room's bot whose personas discuss the Hacker News front page in its chat. Its installed program only posts in Chat under a persona's name (`Personas`), so the room records exactly that grant and every member draws the `BOT` chip; the host decides what to post and when (`pace`), and the model writes it. |
| `crates/agent/agent-participant` | `bot.rs` (new): `BotHost`, bot participants, connector outside worker supervision, `bots.json`. |
| `crates/sdk/tilo-resource`, `tilo-resource-macros` | `ActionCx::invoker`, `ActionCx::appearance`, `#[action(incoming_message)]`, `#[action(moderation)]`. |
| `crates/sdk/ui-core` | `author.rs` (new), image support in `widget::Avatar`. |
| `crates/session/shell` | `RecordChangedTriggers`, `IncomingMessageTriggers`. |
| `crates/packaging/registry-types`, `registry-server`, `registry-client`, `apps/store` | `EntryKind::Bot`, `VersionArtifact`, bot install screen. |
| `apps/shortcuts` | Uses `tilo_automation::grant`. Shows installed bots of the room. |

## 12. Testing

- **Policy.** `tilo-action-policy` unit tests: a bot's answer is the minimum of
  installer and grant across Allow, Ask and Deny. Demoting the installer narrows
  the bot. Governance is denied whatever the installer holds. Removal pauses.
- **Derivation.** `tilo_automation` tests: the derived grant for each step shape
  and trigger. `check` refuses a bot grant wider than derived, and refuses
  appearance without `Personas`.
- **Dispatcher.** Pure tests over synthetic `RecordEvent` and `ConnectorEvent`
  streams:
  self-triggering is skipped, depth is bounded, the same change gives the same
  `RunId`, per-installation order is kept, and `IncomingMessage` matches only
  flagged verbs by others.
- **Receipt.** `tilo-peer-state` and relay fixtures: changes by an uninstalled,
  expired or withdrawn bot device are refused. A person's change carrying an
  appearance is refused on replay. A mapping verb invoked by a member is
  refused by every replica, and its outbox entry is never drained.
- **Runner.** `agent-participant` integration tests with a real relay in room
  mode and a scripted connector worker: install, fire from a record change and
  from a connector event, restart mid-run (no duplicate change, run
  completes), cursor loss (catch-up from `armed_at`), `perform` refusing a
  caller that is not a bot or lacks the grant,
  dead-lettering, uninstall stops the bot and its device is refused.
- **Appearance rendering.** `ui-core` render tests: the `BOT` chip survives
  truncation at every size, the confusable-name suffix is applied, and
  screenshots compare person, agent and bot authors side by side.
- **End to end.** The Discord bridge's suite
  ([discord-bridge.md](discord-bridge.md) §10) is the first real-boot journey
  of the framework.
