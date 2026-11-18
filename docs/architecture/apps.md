# What an app is, and what runs where

An app is code the person did not write and should not have to trust, often
generated, about as trustworthy as a web page. So an app holds nothing that
matters: no identity, no keys, no documents, no transport, no history. It
declares its resources, actions and views ([resources.md](resources.md)), and
the system does everything else. This keeps apps small, makes the safe way the
only way, and limits what a broken or hostile app can reach to what it was
shown and the actions it was granted.

## 1. What an app ships

| Part | What it is | Runs where |
|---|---|---|
| **Model and verbs** | `#[derive(Model)]` types and their `#[resource]` verbs: deterministic changes to the app's own documents | in the system's document host, as a WebAssembly module with no imports beyond the host's, on every device that holds the workspace's keys |
| **Views** | what draws the app's resources: a tilolang package, or a native surface | the tilolang process, or a sandboxed surface process |
| **Outbox handlers** | what an `Outbox<T>` entry does outside (send a mail, sync a calendar) | a sandboxed worker that runs only while there are entries to drain |

The model is one WebAssembly module per app version, the same bytes on a
laptop, a phone, the page and a server, on x86 and ARM. Every key holder
replays the other devices' changes with it ([resources.md](resources.md) §5),
which only holds if they all apply the same deterministic code; a native build
of the model on one device and another build elsewhere could disagree about
what is valid. A package (`.tapp`) therefore carries the model as WebAssembly,
and its views and outbox workers as tilolang, WebAssembly, or native code
built per architecture. A device that holds the keys of a workspace applies
any installed app's actions from that package, so a server takes an app's work
the moment the app is installed, with no build of its own.

An app's model is one crate, `apps/<app>-model`, and the module is one guest
crate beside it, `apps/<app>-model-wasm`, whose whole content is the app's
resource served through `tilo-model-guest`. One mechanism builds every app's
module: `tilo-app model` builds that guest crate for `wasm32-unknown-unknown`,
with the crate's own `.cargo` config saying what makes it a module rather than
a page (no JavaScript glue, no browser clocks). `t test app-models` builds them
all, once, and keeps each under the target directory. Both pack paths take
that output rather than building it again: the release pack
(`dist/apps/package-apps.sh`) when it signs, and the image pack
(`os/scripts/pack-apps.sh`), whose app store a freshly installed system boots
with. Each writes it into the payload as `model.wasm`, signed with the rest, and
refuses an app whose guest crate it finds and whose module is not there.

An app written against the framework writes none of the following: storage, sync, merge,
undo, history, presence, identity, signing, encryption, key handling, transport,
permission checks, coalescing, retries for documents, or a server for its
actions. There is no API for any of them.

## 2. The document host owns every document

The session (on a device without a screen, the same code in its host) runs one
document host per workspace. It is the only thing that opens, decrypts, merges,
signs, stores and syncs an app's documents:

- **Applying an action.** An action arrives at the session's action API with its
  actor ([§4](#4-who-did-it)). The host checks the grant
  ([security architecture](../security/architecture.md) §6), runs the verb from
  the app's module against the document, runs the model's `check`, records the
  change with its history entry, and signs it as the actor. A verb has no clock,
  randomness, file, network or process access because its module has no such
  imports; that's enforced by what the module can link, not by a lint.
- **Replay.** The same module replays a verb received from another device to
  verify it ([resources.md](resources.md) §5). The app's process is never
  involved.
- **Readings.** A view receives readings: the decrypted, typed state of the
  resources it shows and was granted, delivered per changed path. A collection
  arrives as its item ids, and each item as its own reading when a view reads
  it. A view never receives a document, a change graph or a key.
- **Sync.** Devices sync documents with each other over iroh, authenticated by
  device certificates ([workspace.md](workspace.md)). No app is a peer, and no app
  holds a connection to another device or to the mesh.

## 3. One connection per app process

The launcher creates the app's processes and hands each of them its one end of
a connection to the session's action API. The connection is the app's only way into
the system, and the session knows which app and which launch is on it because it
made it:

- A **view process** receives readings for what it shows and submits actions
  (field edits and verbs). It reaches no document it wasn't given a reading of.
- An **outbox worker** receives the entries of its own outbox. It gets network
  access only if the package declares it and the person granted it, and the
  outside secrets it needs (an IMAP password, an OAuth token) as handles from the
  secret store ([passwords-and-secrets.md](../security/passwords-and-secrets.md)).
  It records each result as an action.

No app process receives a device certificate, a join proof, a signing service,
a content key, a document, the identity store, or the address of another
device. The SDK has no code that could use them.

## 4. Who did it

Every change has an actor, decided by the session, never claimed by the app:

| How the action arrived | Actor | Authority |
|---|---|---|
| The person's input to the app's view (a press, typing, a drag) within that gesture | the person, "in Mail" | the person's, limited to the app's grants |
| Invoked through the action API by the palette, a shortcut, the shell, another app's view, an agent | whoever invoked it | the invoker's, and a child invocation carries both |
| Submitted by the app on its own (no input, no invocation) | the app itself | the app's grants only, shown as the app's in history and presence |
| An outbox worker recording a result | the app itself | only the result action its outbox declared |

The session gives each input event it routes to a view process a token for that
gesture. An action that carries a live token is the person's; without one it is
the app's. An app cannot make a background write look like the person's.

A token says the person was acting in that view. It does not say which action they meant,
because the app chose what to submit. So an action that the policy says
needs the person's consent (an outside action, a destructive one, anything beyond the
app's access grants) is confirmed on a sheet the system draws, which the app
can't draw over or answer. The app's own pixels never decide consent.

The session signs every change as its actor. The person's changes are signed with this
device's key, which their identity certified ([security architecture](../security/architecture.md) §3).
An agent's changes are signed with the key the session mints for that agent session,
under an agent certificate the device key signs (`task_agents.rs`, `agent_cert.rs`),
so history names the agent and on whose behalf it acted (`ActionInvoker`).
Authorship verification (§7.2 there) is therefore unchanged for other devices;
the only change is that no key is ever in an app process. An agent's key
stays in the session too, and the agent reaches it through its action API like any
other caller.

## 5. Calling other apps

A view uses another app's resources and actions the way the shell does: it
reads readings it was granted, and invokes published actions through its
connection. A tilolang view does this by `use`ing the app; a native view does it
through the typed action values the other app publishes. Composing another app's
view inline (the canvas) runs that app's view code under that app's authority,
not the caller's ([shell.md §8](../designs/app-language/shell.md)).

## 6. Permissions change without app code

No app code checks a permission or knows that one exists. The session decides
at three points, and only there: when it delivers a reading, when it admits an
action, and when it drains an outbox entry. Grants are policy data
([security architecture](../security/architecture.md) §6), so a new kind of grant
is a new rule at those points, never an app change:

- **Scopes are whatever the session knows at those points:** the resource or
  path, the actor, the place the view shows it in (a strip, a window, a canvas
  place), the device, and the time. "Only in this strip", "for the next hour",
  "read-only while shared in a call", "this agent, these three resources": each is
  a grant over those facts. Today a grant (`action-policy` `Grant`) names a path
  at seven levels (workspace, app, kind, resource, item, field, action), a subject
  (person, role, agent, task, shortcut, app, bot, foreign device) and a lifetime
  (once, session, until a time, until revoked). The place and a specific device
  are the dimensions still to add, in the policy, not in any app.
- **The place travels with every reading and action.** The session tags each
  reading it delivers, and each action it admits, with the place it belongs to,
  even when one view process serves several places. A per-place grant therefore
  needs nothing the protocol doesn't already carry.
- **Denial is absence** (§6.2 there): a reading that isn't delivered, an action
  that isn't offered. An app already handles "nothing here" and "no such
  action", so it can't tell a new restriction from a quiet state, and needs no
  code for it.
- **Creators, not owners.** A workspace's resources belong to the workspace. Who
  made or changed something is in its history, and a person's own documents
  (`resources: .own`) are theirs. A grant can name any of these facts; no app
  declares an owner.

## 7. What a hostile app can do

- See what it was shown: the readings of its own resources, and of others' it was granted.
- Submit actions as itself within its grants, and as the person only during a
  gesture the person made in its own view. Anything needing consent still goes
  through a sheet the system draws.
- Reach the network from its outbox worker only, and only where granted.

It cannot read another app's or workspace's state, act as the person in the
background, sign anything, learn a key, or talk to another device. Its sandbox
([threat-model.md](../security/threat-model.md)) still bounds what its native
code can do to the machine.

## 8. What changes from today

Today the framework runs inside the app process:
- **Native apps:** every one built on the SDK's runner (about sixty under `apps/`,
  from activity to word-processor) loads the identity broker the launcher passed
  in, obtains a signed join proof for the workspace, requests content keys
  (`tilo-identity` `group_keys`), and opens, signs and syncs documents
  itself, through `crates/collab/collaboration` (`sdk_join_identity`, `join_native`).
- **Tilolang apps:** they are closer. Their code is a WebAssembly module that
  imports only the `tl` operations, and it runs in a tilolang process per workspace
  and author. The process's own Rust code still holds the identity service, join
  proofs and content keys, in the same process as the modules.
- **Permission checks:** the SDK checks a `PermissionContext` before submitting,
  and Chat calls it itself (`apps/chat/src/permissions.rs`).
- **Outbox drains:** they run in the app's own process
  (`tilo-app` `framework/effect.rs`), with no grant consulted per entry.
- **Peer capability:** security §6.5 grants apps one on the collaboration mesh.

All of that moves to the document host:

1. A Rust app's model may later run as an instance kind of the workspace's tilolang process,
   beside the tilolang packages'; its windows and an agent's windowless launch reach it.
2. Outbox workers become their own sandboxed launch, with their grants.
3. Input gesture tokens and actor attribution move into the session's action API.
4. The identity broker connection, join proofs, channel keys and the peer
   capability are removed from app launches, from the SDK and from the
   tilolang process; §6.5 of the security architecture is replaced by this document.
5. Permission checks leave the SDK and the apps: a view sees what it is offered
   and nothing else, and Chat's own `ChatPermissions` goes.
6. The outbox drain is decided per entry by the session's policy, like an action.
7. A server applies the actions of any installed app from its package. Today
   `tilo-workspace-service` knows only the app models compiled into it (Chat
   and Calendar, behind its `agents` feature), and `tilo-agent-session` links
   the models of most apps natively; both go once packages carry the model as
   WebAssembly. Most apps' models already build for WebAssembly, for the page.
8. A member lets a server use one secret, and a consent sheet raised on a
   server reaches that member's devices.

Each step keeps apps working while it lands, and each ends by deleting the app-side
code it replaced.

## 9. Which device does what

Because an app holds nothing, letting an app's parts work on a device widens
that device's power, not the app's: what matters is what the device may
already read, and whose authority it holds ([workspace.md](workspace.md),
roles and work). Per part:

| Part | A member's own device | A server that holds the keys | A store |
|---|---|---|---|
| **Model and verbs** | yes | yes: applies actions placed on it and replays everyone's changes | no |
| **Views** | yes | only in thin mode, for a slow device, in a sandbox per workspace | no |
| **Outbox workers** | yes | only for a member who delegated that work and whose secret the server was allowed to use; otherwise the entry waits for one of that member's devices | no |
| **Agents** | yes | as workspace work, or for a member under their delegation, signed as that member's agent on that server | no |
| **Browser** | yes | for a member under their delegation; the server then holds that member's web sessions | no |
| **Linux apps and machines** | yes | in a machine per workspace; whatever is handed to the machine is what the server can see | no |

- **Consent travels to the person.** On a server nobody makes a gesture, so
  everything there acts as the app or as a member's delegated work. An action
  that needs consent sends its sheet to one of that member's devices, or uses
  a grant they gave in advance; it never waits unseen on the server.
- **Secrets stay on a member's own devices** unless that member lets a server
  use one particular secret. Holding a workspace's keys reveals its documents;
  holding a secret lets a server act as the person elsewhere, so each is a
  separate choice. The member's delegation names the vault items a server may
  use, and it stops naming them when they revoke it: an item it does not name is
  refused before the vault is opened and stays on their own devices, and the
  item's own leave and their answer still decide each use.
- **A server holding several people's workspaces keeps each one apart:** its
  documents, views, workers, agents and machines in that workspace's sandbox,
  with that workspace's network grants and limits.
- **Linux apps and machines are outside this model.** Tilo cannot see what
  they do with what they were given; it decides only what goes in.
