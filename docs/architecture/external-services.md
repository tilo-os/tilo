# External services as typed resources

How a service outside Tilo — mail, calendars, source control, a bank, a SaaS
API — appears to people, tasks, Shortcuts and agents, and where its
credentials live. Status: **decided 2026-10-27; nothing here is implemented as
a general facility yet.** What exists is named in §6.

The short answer: **every external service reaches Tilo as a resource kind
with typed actions, published by an installed connector package. There is no
second surface.** MCP is a door onto the same catalogue, on either side of it:
foreign MCP servers are projected into it, and Tilo's catalogue is projected
out over MCP. What differs between a first-party connector and a third-party
MCP server is not the API shape but how much of the contract can be
guaranteed, and that difference is visible, per action, in the catalogue —
never hidden behind a different permission model.

## 1. What a connector is

A **connector** is an ordinary Tilo app package that speaks one external
service. It is written exactly once, against the ordinary app model
(`resources.md`), and it is the only component allowed to hold that service's
credential:

- It **publishes a resource kind** in the ordinary tree — `mail.mailbox` and
  `mail.message`, `calendar.calendar`, `code.issue`, `bank.account` — with the
  same kinds as anything else: `Read`, `Undoable`, `Outside`, derived from the
  action's code, never declared by the connector.
- Its actions take and return the same typed values every other app's do. A
  reference to a message is a resource reference (`resource_reference_schema`),
  an amount is a decimal with a currency, a time is a `DateTime`, an arbitrary
  JSON body is a schema `tilo_actions` publishes. Nothing is a stringly-typed
  blob passed to an agent that hopes for the best.
- Content that arrived from the service is tagged
  `content:untrusted` (`tilo_actions::UNTRUSTED_CONTENT_TAG`). Mail bodies, issue
  comments, bank memos and web pages are data for the caller to reason about,
  never instructions for it.
- Changes the person or an agent makes locally merge as CRDT state; changes
  the service owns are **intents** carried in an `Outbox` and settled by the
  connector, as `storage.md` §13.10 prescribes for connector peers. The field
  says who owns it, so "archive this" shows at once and settles against the
  server without rewriting the server's state into the document.
- It never acts in a room. Everything it does in a room happens because a
  person, an agent, a task, a Shortcut or a bot invoked one of its actions, and
  each such change is attributed to that invoker (`bots.md` §6 already says
  this for the bot case; it is the rule for all of them).

Two placements are the same code:

| Placement | Who runs it | Where a credential lives | When it's right |
|---|---|---|---|
| **On the device** | the person's own Tilo, under the launcher sandbox | `accounts::CredentialOwner` / `ServiceCredentials` | the person's own accounts: mail, a calendar, their code host, their bank |
| **On a room's host** | the room's host, beside a bot | `tilo-secrets`, scoped to the package | a shared service, reachable while the person's device sleeps |

The choice is a property of the installation, not of a second kind of app.

## 2. The typed/untyped boundary

A **typed action** is one whose input and output a caller can read before
invoking it: a descriptor with a title, a description, `ActionParam`s with
types, choices and schemas, a derived kind, an `Availability`, a confirmation
policy and a result spec. That is `tilo_actions::ActionSpec` /
`ActionDescriptor`, and it is what `tilo-control actions list`, the palette,
the Tasks parameter forms and Shortcuts' step editor all consume.

The rule, in one sentence: **an action reaches a person, a Shortcut or an
agent only through a typed descriptor, and an untyped external tool is
represented by a typed action that invested its parameters into a schema
Tilo can enforce.**

Concretely:

- **Every MCP tool becomes one typed action** in its connector's kind, with a
  parameter list built from the tool's JSON Schema and a result declared as
  JSON. "Untyped" describes the *service's* vocabulary, not ours: the tool's
  arguments are validated by `ActionArgs::bind` like any others, its address is
  `(provider, id)` like any others, and its kind is `Outside` unless the
  connector can prove otherwise.
- **A tool whose schema Tilo cannot express** (freeform recursive objects it
  cannot bound, `additionalProperties: true` on every level) is still published
  — the person may need it — but its arguments are declared as the JSON
  parameters the tool publishes **and the action is tagged** so every surface
  marks it:
  *parameters not described*, and not offered to an agent that has no way to
  read the service's documentation. It is `Read`-listed in discovery, and
  invoking it asks first, every time, like an attended action.
- **An untyped tool is never inside a Shortcut's always-run run and never a
  trigger.** Shortcuts' saved program may call a typed action; a step whose
  parameters cannot be checked at save time is refused when saved, not at 3 a.m.
- **Untrusted results stay untrusted across the boundary.** A tool's output is
  `content:untrusted`, and the agent-side MCP projection says so
  (`mcp_page` already renders `[untrusted content]`).
- **Nothing is discoverable-but-uninvocable, and nothing is invocable by a
  click only.** An MCP tool that a connector publishes is a row in the
  catalogue whether it is drawn as a button in the connector's view, offered to
  an agent, or called from `tilo-control`; §"one door" of `resources.md` is not
  suspended for external services.

Permissioning an untyped tool is therefore **not** a different model:

- Its node is the ordinary tree node — `Connector › service › tool` — so a
  grant on "GitHub" covers its tools, and a grant on "GitHub › merge" covers
  one.
- Its kind is `Outside`. A delegation's Ask on it asks the person before the
  call leaves the device; a person whose run is paused sees the parameters
  rendered as a draft (`tilo_actions::draft`) exactly as for a typed verb.
- Because the arguments are not described, the **derived** contract cannot know
  whether a call is a read or a write. It therefore never guesses `Undoable`,
  never offers an Undo, and never claims `RiskLevel::None`: an untyped tool is
  at minimum `Sensitive` and confirms by default. The person may answer
  "Always allow this" for it, which is the informed choice the model already
  gives them for any confirmation-bearing action (`agents.md`, *Allow, Ask
  first, Deny*).
- A tool the connector itself classifies as `Destructive` or `Privileged`
  keeps that classification through the projection; the connector cannot
  relabel a foreign tool as safe, and a connector that mislabels one is a
  finding against the connector, not a loophole for the caller.

This is the whole boundary: **typed is what the caller can inspect; the
permission model is the same for both.** A catalogue that needed a separate
permission model for untyped tools would be a second action system, which is
the thing Tilo exists to not have.

## 3. Credentials

Credentials are ordinary secrets, held once, and referenced everywhere else:

- A service login or API key lives in the **accounts vault** through
  `accounts::CredentialOwner` / `ServiceCredentials`, keyed by
  `ServiceAuthorization { service, purpose, origin }`. A record, a setting, a
  Shortcut or an agent profile holds only the opaque `CredentialRef`.
- Changing a profile's endpoint never re-authorizes the key stored at the old
  origin; resolution returns `Zeroizing<String>`; status never returns the key
  (`docs/security/passwords-and-secrets.md`). A headless host with no keychain
  uses `tilo-secrets` in `TILO_SECRETS_DIR` on an encrypted volume.
- **Setting a credential is an action on a resource, with an attended,
  always-confirmed field.** A connector publishes `set-credential(secret:
  ParamType::Secret)`; an invocation of it creates **no** change and **no**
  history entry, is redacted wherever shown or recorded, and needs the person
  at that device. An agent can propose that the person connect an account; an
  agent never types a secret, and no path lets one.
- **Secrets never enter prompts, records, history, Shortcut files, action
  arguments in a catalogue, logs, or the relay.** A Shortcut that uses a
  service names the credential; the connector resolves it at run time on the
  device that runs the step.
- **Third-party credentials stay third-party.** Tilo's catalogue does not
  broker a partner's tokens for other services, and an MCP server's own
  `OAuth`/`client_secret` fields are stored as that connector's secret, not as
  a Tilo identity.

## 4. Sync versus live calls, and offline behaviour

Both are allowed, and which one applies is visible in the resource:

- **A connector that syncs** (IMAP, JMAP, CalDAV, CardDAV, feeds, a Git host's
  issues) materialises the service's objects as records, per `storage.md`
  §13.10: service-owned fields carry the server's truth, person-owned fields
  merge and are never sent to the service, identity is stable across devices,
  and reconciliation is leased so one device per account talks to the server.
  Its actions are ordinary record verbs and work offline where the change
  can be expressed as an intent in an `Outbox`.
- **A connector that calls live** (a bank transfer, a payment authorisation, an
  LLM tool call) publishes actions whose contract reports
  `Availability::Unavailable` when the service is unreachable. Nothing is faked,
  queued into a fake success or replayed by guessing: an action that could not
  reach its service fails visibly with the reason, and a retry is a new
  invocation the person or the agent makes.
- **Offline is a state of the data, not of the app.** A message whose body is
  not on this device hydrates from a peer or the service (`storage.md` §12);
  "available offline" pins the mailbox or calendar. A live-only action shows
  what it needs instead of an empty screen.

## 5. Attribution

An action taken on an external service is attributed like any other:

- The history entry records the actor (a person, an agent, a Shortcut, a bot,
  or the app itself), the person it acted for, the task or run it belongs to,
  and that the change is external (`resources.md` §5.1, `storage.md` §13.10).
- The service's side sees whatever the connector's credential can say — often
  nothing beyond "this app". Tilo therefore records, locally and in the room,
  the invoker; the connector's settings page shows the recent external calls
  with their invoker, and a bot-hosted connector attributes each to the bot.
- A run that carried a credential ref never shows the credential; a call that
  failed at the service shows the service's refusal in the invoker's history.

## 6. What exists today, and what this decision changes

Exists and is kept unchanged:

- `crates/collab/actions`: the one action schema; `content:untrusted`,
  `ParamType::Secret`, `tilo.attended`, `resource_reference_schema`,
  `ActionKind::Outside`, `Outbox`. (`ParamType` has no generic JSON parameter
  today; `Object`/`Array` with a JSON Schema fragment is how a foreign tool's
  arguments are carried, and a schema Tilo cannot bound is what §2's marking
  exists for.)
- `accounts::CredentialOwner`, `ServiceCredentials`, `CredentialRef`, and
  `tilo-secrets` for package-scoped custody (`docs/security/passwords-and-secrets.md`).
- `crates/agent/connector`: the outside worker protocol (`Perform`, `Result`,
  `Event`, `Secret`, `NeedSecret`, `StoreSecret`) and its event log, as
  specified in `bots.md` §6.
- `crates/runtime/tilo-runtime`: the MCP projection of the catalogue outward
  (`mcp.rs`, `mcp_page.rs`) — typed parameters, risk, confirmation, the
  untrusted tag, `tilo://` addresses — and the agent door.
- `docs/architecture/storage.md` §13.10 (connector peers) for the sync shape.

Decided here, and new:

1. **MCP *in* is a connector kind, not a second catalogue.** A connector whose
   service happens to be an MCP server publishes its tools as typed actions in
   its own resource kind, exactly like one that speaks a REST API. There is one
   registry, one grant tree, one history and one Untrusted/Secret contract.
2. **The untyped-action marking of §2 is part of the contract**, so it crosses
   every door: the catalogue, the palette, the agent, `tilo-control`, Shortcuts'
   save-time check and the tasks' parameter forms read it from the same place.
3. **The catalogue outward stays the only outbound shape.** The MCP projection
   of Tilo's actions is a door onto the grants-filtered catalogue; a catalogue
   feed offered to a partner directory is a filtered view of the same, and no
   catalogue may describe an action Tilo cannot invoke.
4. **A connector never receives a device key, a replica or room content** beyond
   the operations it is asked to perform, and never writes a record its host
   owns (`bots.md` §6). That includes MCP servers: an agent cannot use one to
   reach a room the invoker could not reach.

Open questions this decision deliberately leaves, each its own issue rather
than part of this document:

- Which connectors ship first-party, and in what order.
- Whether a partner directory's connectors may be installed on a room's host by
  the host's administrator without the room's owners seeing their code.
- The certificate/allowlist story for a room host that reaches an MCP server
  over the open internet (it is an `EgressPolicy` question, `security.md`).

## Related

- [`resources.md`](resources.md) — resources, actions, kinds, views, history.
- [`permissions.md`](permissions.md) — grants over the published tree.
- [`storage.md`](storage.md) §13.10 — connector peers, field ownership, leases.
- [`bots.md`](bots.md) §6 — connectors beside a bot, the outside worker, secrets.
- [`agents.md`](agents.md) — agent access, unattended actions, MCP projection.
- [`../security/passwords-and-secrets.md`](../security/passwords-and-secrets.md) — the vault,
  the credential broker, `ServiceCredentials`.
- [`../agent-action-api.md`](../agent-action-api.md) — the semantic door.
