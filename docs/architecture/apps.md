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

The module carries the app's model logic and nothing of a document's
storage: the device holds the document. Built with `--cfg tilo_model_guest`,
the module's `Doc` (Automerge's `AutoCommit`) is a handle on the device's
document, and every read and write a verb makes (a field's value, a
collection's entries, a put, an insert, a delete) is one call to the device,
answered from the device's own Automerge at the resource's current heads
(`vendor/automerge/src/model_device`). So the change a verb makes is made by the
device's Automerge, which signs it and keeps its history, and the module is
the size of the app's own code rather than one copy of Automerge per app.
`#[derive(Model)]` and `#[resource]` generate the same code for both builds;
only what `Doc` is differs.

The device, not the module, writes an action's history (`tilo-model-engine`). It
checks an invocation's arguments against the verb the package's signed
surface declares, builds the envelope (an id minted from the request, the
base heads, the invoker's actor, the origin time), writes the history entry
and commits the change through the same executor an app compiled in uses, and
replays another holder's invocation at the heads it was made on. The module
does four things only: write a new resource's initial content, build an
action from the checked arguments through the app's own declarations, say
what a recorded action is (the bytes it is recorded as, its change and action
kinds, what history describes it as, whether its undo is its own code's), and
apply a recorded action's verb under the context the envelope pins. On the
request's document the device refuses a module's commit, empty change, actor
and changes loaded or merged in, so a module cannot write history of its own.
History's own verbs, such as reverting a step, are the device's and never
reach a module.

An app's model is one crate, `apps/<app>-model`, and the module is one guest
crate beside it, `apps/<app>-model-wasm`, whose whole content is the app's
resources, `tilo_model_guest::model!("<app>", <resource>, ...)`, served
through `tilo-model-guest`. A module is one per app, whatever number of
resource kinds the app declares: each request names the resource kind it is for,
and the module routes it to that resource's verbs. The guest crates are
workspace members, so every app's module shares one lockfile and one wasm32
target directory, and each is built for size (opt-level `z`, no unwinding, no
debug info, then `wasm-opt`), since it is the same bytes on every device. The
crate's own `.cargo` config says what makes a module rather than a page (no
JavaScript glue, no browser clocks). Built alone (`tilolang model`), a guest is
a whole module, and `tilolang package` ships it so: any device calls it as it
is.

Nearly all of such a module is code every app's model shares: automerge, std,
the resource layer, serde_json. So the apps this repository ships are built
together. `t test app-models` (tools/ci/app-models.sh) links every guest crate
into one module (crates/sdk/tilo-models-wasm) and splits it with
`wasm-split --models` (tools/web/wasm-split): what only one app's entry
reaches becomes that app's `model.wasm`, tens of KB, and everything else the
model runtime, kept once beside them as `app-model-runtime/` (its modules and a
`runtime.json` naming it by a BLAKE3 hash of their bytes). Each app's module
names that hash, and the export to call, in a `tilo_model_runtime` section.
Both pack paths take that output rather than building it again: the release
pack (`dist/apps/package-apps.sh`) when it signs, and the image pack
(`os/scripts/pack-apps.sh`), whose app store a freshly installed system boots
with. Each writes an app's module into its payload as `model.wasm`, signed with
the rest, and refuses an app whose guest crate it finds and whose module is not
there. The model runtime is in no app's package: the runtime bundle every app
of the release names carries it, signed, as `lib/app-model-runtime`, and
`tools/perf/runtime-size-budget/` holds its bytes. An app's package therefore
counts only its own model code against its budget.

A device links a split module after the runtime it names
(`tilo-model-engine`, on the engine that executes every WebAssembly module, which
caches the runtime's compiled modules once per process): the runtime's start
module with the model ABI, its shared modules, then the app's module, each
import answered by the exports of the modules before it, as the page links a
split bundle. It finds the runtime by its hash in the directories
`TILO_APP_MODEL_RUNTIME` lists, then under `TILO_APP_MODELS` (the app-models
output the tests are given), then in every installed runtime bundle (the
person's and `/usr/lib/tilo-runtime`). The server's image carries the runtime
of the release it was built with and names it in `TILO_APP_MODEL_RUNTIME`.
A package built by another release names another runtime, and the bundle that
carries it in its manifest (`[package].runtime`, `tilo-runtime/<version>`). A
server lacking that runtime fetches it before it applies the app's first action
(`app_cli::model_runtime`): it asks the enabled registries for the runtime
package that provides that version, takes the package only as the registry's
signed entry names it (length, BLAKE3, root and publisher) and when its
signature verifies, checks that the bundle manifest inside is signed by a
runtime publisher the device trusts (as the launcher checks an installed
bundle) and lists every file it takes, and downloads only
`lib/app-model-runtime`, never the bundle's libraries. The engine then reads
the runtime and computes its hash from its bytes; it must be the hash the
module names. The runtime is kept on disk under the store's cache
(`app-model-runtime/<hash>`, the four most recently used) and in memory for
the process, so later actions ask the registry nothing. A runtime that cannot
be fetched or verified is refused with the hash the module names, the ones
this server holds and why the fetch failed, and is not asked for again for five
minutes; it is never linked against other code. A whole module names none and
is called as before.

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
  change with its history entry, and signs it as the actor. The verb reads and
  writes the document through the device's document calls, so what it writes
  lands in the device's Automerge. A verb has no clock, randomness, file, network
  or process access because its module has no such imports (only its request,
  its answer and the document calls); that's enforced by what the module can
  link, not by a lint.
- **Replay.** The same module replays a verb received from another device to
  verify it ([resources.md](resources.md) §5): the device gives it the document
  at the change's parent heads, the verb goes through the same document calls,
  and the device compares the change it makes with the one received. The app's
  process is never involved.
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

The steps from today's code to this model are in [the core](core.md),
section 4.

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
