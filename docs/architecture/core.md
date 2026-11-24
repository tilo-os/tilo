# The core

Tilo is a small core and thin products on top of it. This page is the
specification of the core. For each piece it gives how the code works on
`main` today, every copy of the piece with its size, the one design that
survives, and what is deleted to get there.

The core is six pieces and nothing else:

1. **Devices.** A device has its own key, certified by a person's identity.
2. **Identity.** A person is one identity root. It certifies, revokes and
   configures that person's devices.
3. **Sync.** Resources and files replicate between the devices of a
   workspace, end to end encrypted.
4. **The app framework.** An app declares resources, actions and views, and
   each action is tied to the view and controls that show it.
5. **The workspace.** A workspace holds apps, resources, files and
   collaborators (people and agents), under the permissions its owners set.
6. **Actions everywhere.** An action is called the same way from a view, the
   CLI, MCP, the SDKs, an agent or an automation.

Everything else is a product built on these: the desktop on each platform,
the ARM devices, the server distro, the web version, the native assistants,
the always-on agent, agent sandboxes on tilo.sh, and the productivity apps. A
product adds what is its own. When it needs a second copy of a core piece,
the core piece is improved instead.

Sizes are lines of Rust, TypeScript, JavaScript, Python and tilolang in the
files git tracks, tests included, measured on `main` at `d81cc1750ff`
(2026-11-21). Main holds about 7.48M lines in all, including 807k of
`vendor/`.

## The design

### Resources

A resource is a thing a person or an agent can name, share, link to and
open: a note, a task, a mailbox, a calendar, the workspace's folder, its
member list. It has a resource type, which an app declares (a `Note` has a title and
a body), and a stable id. Resources nest: a task can hold tasks, a mailbox
holds messages, and the folder holds folders and files. There is no second
level of "items" inside a resource; its type is a property of a resource, not a
level of the tree. Every resource, at any depth, can be shared, granted,
linked and shown on its own.

A resource is the one unit of everything the system does for it: sync,
permissions, history and undo, search and views. Its fields are kept in a
mergeable structure (Automerge), so two people editing at once both keep
their edits. A very large resource is split into pieces on disk and is still
one resource to everyone.

### Storage

- A change is a signed fact. A device writes it for a member, signs it,
  seals it under the keys of the resource's readers and keeps it. Changes
  are only appended. The current state, and every past state, come from them.
- Every device runs the same engine. Devices differ only in how much they
  keep and which work they take. A server is a device that keeps
  more. A device without a resource's keys keeps and forwards its sealed changes
  without reading them.
  Every device checks a sealed change's signer, that the signer may write for
  the member the change names (its own person, or an agent it runs), and the
  member's grants. Outside the seal a change written for a caller elsewhere
  names that caller and carries their signature over the call's digest, never
  its arguments, so a device without the keys checks it too. A device that can
  read the change also checks its contents, with the same check admission uses.
  A change that fails a reader's check is refused on its own, and the record
  stays readable without it. One bad change, from a buggy writer or a hostile
  member, never makes a resource unreadable.
  Every change names a member, and a change that names no one is refused.
  A device's own upkeep, such as a server keeping a workspace, names the
  person the device belongs to and is signed by the device.
  A workspace held only by one person's computer is complete.
- Changes move over one link, device to device. Reaching a device through a
  server means reaching it through a device that is online.
- Reads and queries go through one index built from the kept changes. It can
  always be rebuilt from them.
- Files are resources. The folder is one tree, synced and merged like any
  other edit. A file's bytes are sealed, content-addressed blobs the tree
  points at, fetched from any device that has them when they are needed. The
  mount, the native folder and an app's file list are views of that one tree.

### Addresses

An address is the path to a resource, with an action after `#`:
`acme/notes/<note>#update`, `acme/tasks/<task>/<sub-task>#done`,
`acme/files/projects/plan.md#move`. Resource names are user text, so `#` is
the only separator of an action and a name can never be mistaken for one.
People and agents use names, which resolve to stable ids, so renaming
something keeps links and grants. One resolver in `tilo_actions::tree` walks
the published tree from the workspace root for every caller, and grants
use the same addresses. An action belongs to its resource's type, and its id
always names that type and the verb, so a second type adding the same verb
never changes an id that grants and records already name.

### Permissions

- One rule. A grant gives a person, a role or an agent Allow, Ask or Deny on a
  pattern over addresses (`acme/notes/*#update`, `acme/tasks/<list>/**`,
  `acme/files/projects/**#read`), optionally limited to a resource type. The most
  specific grant wins, a person's own grant beats their role's, and no grant
  means no. Roles are named sets of grants.
- Membership and grants are a resource of the workspace, changed by signed
  actions. No device and no server governs a workspace: every device that
  can read it works out the same answer from the same signed list.
- The rule is enforced three times with the same function: when an action is
  called (one admission, `admit_invocation`; an agent acting for a person
  gets the narrower of the two, and the rights its person signed into its
  certificate are that person's Allow for what they cover, so those calls don't
  ask again), when a change arrives (every receiver drops
  a change whose author was not allowed to make it at the time), and through
  the keys (reading means holding the keys of the resource's readers;
  resources with the same readers share one key group, and removing a reader
  rekeys it). A reader set names people, a governance role (`author and
  moderator`) or both. Its group holds whoever the workspace's policy says
  reads it now, so a role granted or taken away moves the group by the same
  commits as a member joining or leaving. No action hands out or rotates
  keys itself. A device creates a record offline: it uses the group key it
  holds for those readers, or, for a new reader set, makes the group key and
  wraps it for each reader itself, and the setup syncs later like any change.
  Nothing waits on a host. What a device already received cannot be taken back.
  Every record in a workspace is sealed; one every member reads is sealed under
  the all-members group, and a clear write is refused. So the store's frames are
  the only copy, and catching up reads them for every record.
- A window does not decide permissions itself. The device's daemon answers
  for the actions a view shows, the window holds those answers for its
  controls and updates them live as grants change, and the call is still
  admitted when it is made. There is no permission check in the UI library,
  the app SDK, tilolang's runtime or an app.
- A file action is an action at the file's address. A grant on a folder
  covers what is below it. The mount is one more caller and decides every
  open from the replicated member list; a path a person may not see does not
  exist for them.
- Privacy between members is a rule on a record: a member's drafts are
  readable by that member alone and sealed under their keys, in the same
  workspace. There is no per-person store.
- What an app's code may reach (paths, devices, network hosts) is a separate
  grant, given at install or through a picker. An app acting for a person
  does only what both may.
- A visitor with no identity (joining a wait list, reading a public
  directory) is a caller too: the page makes a key on the visitor's device
  and signs each call with it, and a grant to any key names exactly the
  actions a workspace opens to visitors, limited per client address. There
  is no unsigned caller. When the visitor later signs up, that key is
  certified as one of their devices, so what they did is already theirs.
- An automation, like a wait list's confirmation mail, runs as the member who
  set it up, within that member's grants.
- Who belongs to a channel, a shared folder or any resource is who may read
  it: the permission system publishes a resource's readers as a set any app
  reads, with typed actions to grant and revoke. No app keeps a membership list
  of its own.

### Apps

An app is a package installed in a workspace, written in tilolang or Rust
against the one app SDK. It declares resource
types, actions (verbs with a typed input, a typed result and a kind:
read, edit or privileged) and views (functions from a resource to UI whose
controls name fields and actions). A bridge to an outside service, such as Discord, is such an app: its protocol is the app's own code over the system's general network, secret and call capabilities, and the system carries no code named for that service. Installing it publishes all three into the
workspace's tree. That tree is the only registry. Installing, updating,
pinning and removing an app are actions on its resource there, admitted by
the ordinary grants. A device keeps the packages its workspaces name in one
content-addressed cache its daemon owns, never a store per person, and the
daemon fetches what the tree names and starts its sandboxes; nothing else
writes that cache while the daemon runs. A new workspace is created with the default apps
installed like any other; an OS image only fills the cache, so signing in on
a device never adds apps to a workspace. A flatpak app is an app like any
other whose package is a flatpak ref.

An app is never "running". Any device that holds the workspace's keys and
takes the work runs an action's code, sandboxed, when it is called; an agent
can complete a task at night with no window open anywhere. Opening an app
means opening a view of a resource, in a window, a chat or a web page.
Storage, sync, permissions, history, undo, search, notifications, presence,
windows and outside effects (sending a mail, calling an API, declared by the
action and carried out by the system under the workspace's grants) are the
system's. An app is its resource types, its actions' code and its views.
An app's own process reaches the records of its app id whose kinds its manifest
declares, and its own windows' records, in any workspace it is installed in;
another app's records or windows only through that app's actions. A file a person chooses for an app is a grant to that app on the file's tree node, so it follows renames, shows in the file's sharing and can be revoked there.

There is no ceiling on what an app can be, and no step down that loses the
model. An app climbs in steps:

- a tilolang view;
- a tilolang app with its own components;
- a Rust app on the SDK;
- Rust that draws its own UI (a game, a 3D scene, a CAD canvas), declaring how
  it maps to resources, actions and views.

Every step keeps actions callable from anywhere and tied to views, history
and undo, identity, permissions, views that can be shown elsewhere, the
shared components, theme and motion. They are the default, never opted into.

### Agents and automations

An agent is not a person. It is shown as itself: its name, the person it
acts for, its turns and what they cost. An automation (a shortcut, a rule, a
script someone set up) is shown as the rule that ran and who set it up.
Underneath, both are actors acting for a person, like a person acting for
themselves, and one call can run through several of them: an automation a
person set up, run by an agent. Every actor in that chain must allow the
call (`tilo_action_policy::Acting`, `admit_invocation`).
The person an agent acts for is the root its key's certificate chains to, read
from each call's signature chain; an agent is the pair of its id and that person,
in grants, reader sets, answers and history alike. Only that person's answers and
stops count for it, so every replica decides the same way without opening the
agent's record, which holds everything else about the agent.

An agent or an automation acts with a subset of its person's permissions,
by default the largest one. Granting it less works the
way granting a person less does. It acts on undoable things without asking,
which is most of what happens in a workspace. It runs on the workspace's
devices, like everything else (see where work runs).

An agent is started by triggers, the same triggers a shortcut or any other
automation uses: anything that can be watched, such as a mention anywhere, a
direct message, a task assigned to it, a change to some resource, a time, or
the weather in Bangalore. A watched event says what happened to the resource,
created, edited or removed, read by the store from the recorded change beside
its id; no app works it out for itself. A new agent comes with the triggers a member would
expect (mentions, direct messages, assigned tasks), and its person changes them
like any automation's. A trigger starts a turn, and the agent answers where it
was triggered: in the chat, on the task, in the note. There is no separate
record of asking. A turn is an invocation of the agent's answer action, so its
state, steps and output are that invocation record's, and its cause is the
address of the change that triggered it. Its triggers are an ordinary shortcut
its person owns. The agent engine runs inside each device's one daemon and runs the
agent's triggers; it never reads an app's own fields. Which eligible device runs a turn is the one placement rule of where work
runs, decided in one function, `tilo_workspace_service::work::takes_turn`. Everything a turn writes
is a member write for the agent, signed by the device that runs it.

There is one trigger runner, and it runs every shortcut: an agent's, a
connector's, a person's. A run that can't go ahead yet (no model, nothing to
take it) is tried again later. A run that fails for good keeps its failed
record, shown wherever runs are listed. A shortcut's counts come from its run
history, and pausing it is a setting on the shortcut. The workspace's host runs
a member's shortcuts for that member, within what that member allowed when
saving them, so events reach members who have no server of their own.

An agent is a reader in its own right. Its member key is in the reader set
of every record it is in: a direct conversation between Ada and her agent
is sealed for Ada and the agent, never for Ada alone. A device that runs the
agent, the tilo.sh server included, holds the agent's keys and so opens
exactly the records the agent reads, and no others. A device that cannot
open the record a trigger is about is not eligible to take its turn.
A sub-agent or coding agent that an agent starts is one more actor in the
chain, so it does no more than everyone before it may. Any device of the
person an agent belongs to may approve that agent, and approving it is the
ticket flow, the one way a key is added: wherever the agent runs (a computer,
the person's server, tilo.sh) asks with a ticket naming the agent's key, the
agent shows that ticket while it waits, and a device holding the person's root
redeems it over `tilo-actions/1` and certifies the key. There is no separate
proposal queue and no certify protocol beside it. Any member who may write where an agent is may talk to it,
whoever the agent belongs to. A turn answering a person runs with no more
than both of them may do: the agent's own grants and the grants of the person
whose message it answers, the smaller of the two on every resource. An
agent's own grants are ordinary grants in the workspace's policy, granted to
the agent's member id, never a separate limit kept in the agent's record, so
every device that admits its changes caps them the same way. A change signed
by an agent's key is weighed as that agent, with the chain it acted for. Ada's
agent answering Bo reads and changes only what Ada's agent and Bo both may. An agent nobody has approved is not
triggered; the agent shows that it waits for approval.

A member change (add, role, remove) is an ordinary action on
`workspace/members`, admitted by grants like every other action (an Ask grant
when someone wants to confirm it). A workspace always keeps one owner, and a
server's person has no special place in it.

A server runs an app's code for the person at the page, under that
person's grants, never as a person of its own. Carrying an app's old data
into its resources is an invocation by a member: it waits until a device
of a member who may write those resources connects, which signs that one
declared action as its person, and it runs once.

An agent sandbox on tilo.sh is one workspace. Whether that workspace also
has a machine for terminal commands, Browser tabs, or Linux and Windows apps
is what the customer pays for.

There is one agent runtime: the Tilo agent. It works on a workspace's files
and Terminal through the same actions the CLI, MCP and SDKs call, signs in
to outside services from the workspace vault, and uses the model provider
chosen in the workspace's AI settings. Tilo has no code for any particular
outside agent. Claude Code, Codex, `t` and the rest are ordinary commands
in a workspace's Terminal, run by a person or by the Tilo agent. A model
provider (Anthropic, OpenAI, a local model) is one entry in AI settings,
not a code path of its own. AI settings are the workspace's `tilo.models`
resource, read on every model call; Settings' AI page shows it.

The proof that this works is our own development: in the company workspace,
the Tilo agent takes a small change to the tilo repo from a chat message,
checks it with `t`, commits it and pushes it.

### Calls

A call is an address, a typed input and who acts. It is admitted the same
way, runs the same code, returns the same typed result and writes the same
history entry whoever makes it. Every entry point is a projection of the
published tree with no code per action: a bound control in a view, a typed
call from tilolang (`tasks.done(task)`, checked at compile time), `tilo ls`
and `tilo do`, a few generic MCP tools, and SDK types generated from the
published types. An action names the view that shows it, so every open view
of the resource shows the change and who made it, and an agent can attach
that view to its answer. A view publishes each action's description once per
resource type; a row publishes only its resource and what differs for it
(whether the action's require passes, and why not), so a list of 10k rows
stays cheap and every row is still reachable by its address.
A row in a reading carries its resource's address as a typed handle, so a
view calls the row's own actions (`network.join()` on a Wi-Fi row) the same
way in Rust and tilolang. An action's name belongs to its resource type, so
`join` on two types never collides, and no root action takes a row's name to
find it.

Our own code calls only over the device link (`tilo-actions/1` on iroh).
That includes signing in: `tilo login <ticket>` connects straight to the
device that issued the ticket (its iroh endpoint id, a relay hint and a
one-time secret) and calls the certify action there, before the new device
holds a certificate. The issuer is any device of the person: a browser tab,
a desktop or a server. A tab has no endpoint, so the page's host serves its
ticket and the tab signs the approval. A tab that holds no root signs in the
other way round: its ticket names the tab's pending request, and the person runs
`tilo device add <ticket>` on a device that is logged in, and the person's root certifies the tab
like any other device. A computer that holds no root hands the signing on: the person's tab that
holds their root, or any of their logged-in devices, sees the request among `devices#pending` and
answers it. `tilo login` is always this computer logging in. A ticket is only how a
device that is online gets its certificate: the Imager, running on a device of
the person, issues the new device's certificate with the same functions and
writes it into the image, so an image written today joins whenever it first
boots. There is no second enrolment protocol. Planning a machine, taking a planned one in and listing
the workspace's servers are actions the workspace's host serves, listed in
every session like any other, so a person or an agent on any device can do them; the installer is
only one caller of them. There is no site lookup. `/v1` REST is only an edge
for callers outside Tilo.

### Where work runs

Everything a person, an agent or an automation does is an action on a resource.
The device it starts on admits it against the workspace's synced grants, bound
by every actor in the chain. Most actions are a change to a synced resource:
that device makes the change, signs it and syncs it, and nothing is sent
anywhere to run.

An action that needs what not every device has (a secret it may not read, a
local model or GPU, Linux, a particular piece of hardware, a connection that
stays up such as fetching mail) runs on a device of the workspace that has it:
the starting device if it qualifies, otherwise an always-on server, otherwise
any other, decided in one function (`work::takes_turn`); a device that stops
writing for a minute loses the work to the next. Running elsewhere is that action dispatched to the other device's provider, a tilolang app or an engine alike, never a package's command line started there as a job. So an app whose work needs an engine runs on the web too: its engine calls go to a workspace device that runs that engine, and when none is reachable the person sees why at the moment they act; no app is hidden from a platform for it. It runs under the caller's grants: the device that runs it signs what it
writes with its own device key and carries the caller's signed call with it, so
every receiver checks that the call was the caller's and that the change stays
within it and the caller's grants. No caller key leaves the caller's device, and
there is no second signing scheme. An action's code that calls another action,
another app's or a native provider's, makes a child call bounded by its own
caller's grants, and a refusal comes back to that code as a result, never
only to a log. A child call is pending until it answers, and the calling run is suspended
in its journal meanwhile, holding no connection or thread. A call that needs a
person's yes is pending the same way: its `tilo.invocations` record stays
asked, readable by the caller and the person asked, until the person answers, and the answer resumes the run with the result, or
with a refusal on no, checked against the grants as they stand then. There is
no second way of asking. The person answers every question in one asking view the
shell draws beside what asked: a press's call, an agent's question and a
device's confirmation alike, in the desktop and on the web. No window draws
its own. That view lists the asked invocation records; there is no
store of asks beside them. It streams its progress and output
back to the caller as it goes, and ends in a result or a legible error the
caller sees, and the caller can cancel it. What it writes goes into resources,
so every view shows it, and work that outlives its device is carried on by
another eligible device from what is written. When no eligible device is
online, the call waits and says what it waits for ("waiting for a device that
can use the workspace's Anthropic key"), until one connects or the caller
cancels.

A secret such as an API key is a resource in the workspace's vault. Reading it
and using it are separate grants: a member may make model calls with the
workspace's models without being able to read the key, and such a call runs on
a device of someone who may read it. A member who loses access never held the
key, so nothing is taken back from their devices. Models, keys, spend and
limits are the workspace's; a limit for a member or an agent is a grant.
An app whose protocol needs the raw secret in its own process (a gateway
login, voice, an SSH or VNC handshake, a database login) holds a read grant on
that one item, which its person grants once as an asked record. The vault's
release action, addressed at the item (`vault.item:<id>#release`, so a grant on it
is a grant on that item alone), hands the item only to a process of that app's
own, sandboxed and placed by `work::takes_turn` on a device that may read it: its
held connection, or engine work that needs the secret run as such a process. A
process several apps share never receives a secret. The process's egress, fixed
at its launch, is the network its package was granted, which may name every host
under a domain when the person is shown that domain as such, and a new grant
restarts it. A grant may include UDP to its hosts, which the egress proxy carries
to their resolved addresses alone. Calling services whose media servers aren't
their hostnames (Discord's voice server is an address handed out inside the
call's own connection) need a grant the person sees as such: calls may send
audio and video to any internet address. It is UDP alone, through the proxy,
recorded and rate-limited, and open only while a call of that app is live;
its TCP stays per host. The process never holds raw network. A live
call, its keys and an app's call lane are the daemon's, so a device with no
session carries a call, and the desktop is a client of it. Passwords an app keeps are vault items, never
fields of its own records. An item says which app and purpose it serves, and the
app finds it by those. Secrets the old keeper holds move into the vault once, on
the person's own device, through the ordinary add as that person, and leave the
old keeper as they move; nobody types a secret again.
The secret lives in that process's memory alone, never in a record, an argument,
a log or history.
Every other use stays a call the vault makes itself, such as a web call with the
secret added, a signature made with a signing key, or the current one-time code
from a TOTP seed (`vault.item:<id>#code`), where the vault is; a grant
to use the item lets people, agents and CI call it without ever holding it.
The daemon's binary is its own crate outside core. It is the one place that links
core, the language and the device's services, and registers each into core through
core's traits; no core library names a desktop, os or language crate.
Engines are device capability, so each device runs one engine host, owned by
its daemon. It runs every app's engine jobs under each call's grants, with no
authority of its own, and lives while a job runs. Engine work that needs a
released secret runs in that app's own sandboxed process instead, and network
an engine reaches for an app goes under that app's grant alone. Streams a window
shows stay in the app's process.
Such a use that changes nothing is a read: it records no change, and what it
returns is shown, never stored, not even as window state.
A view that shows a secret or a code declares `protect capture`. Wherever it is
shown (its window, another view, a shared screen, the web page, a capture an
agent takes) it is left out of captures and streams.

### Views

A view is published at an address and takes a resource. Any view can show
any other by naming it and giving it a resource: a task list in a chat
message, a calendar week in a note. The shown view runs with its own app's
code and the viewer's permissions, and edits inside it are calls to its
app's actions. The shell is a view. The desktop styles and their presets are a
resource of the workspace that every view of the desktop reads, its page included;
picking or changing one is that resource's action. A person, or an agent in a chat, writes a
new view in tilolang from existing views and actions, without a build. One
view output is drawn as native widgets and as semantic HTML on the web page.

Live activities (something playing, a running timer, a call, a recording)
are views too. A resource type declares a `live` view, built from ordinary
components and its own actions, and states from its own fields when an
instance is live. The resource index lists the live instances across the
workspace's devices, with the device and the time it started. A reader sees
only the rows for resources they can read. The shell shows each row's `live`
view in the bar, quick settings and the lock screen, on every device. Media
keys and the lock screen's controls call the type's actions. No app calls a
shell interface to start or end one.

A window's own state (a calculator's mode, a book's open panels, a list's
filter) is declared in its view and kept in that window's entry in the
workspace layout. That way it follows the window to another device. Each
declared field gets a typed action on the window, so an agent, the palette
or a script can set it like any other action. Apps declare the state and
never store it themselves. Only the person whose window it is can read its
state, unless they share the window. Text a person is writing is never
window state: a draft is a resource, with history.

A view keeps time with `every` and `after`. Its timers run on the window's
device, as the person at the window, and only while the window is shown; a
hidden window holds them and catches up with one tick when it's seen again.
A view follows a job's progress from the job's record under
`devices/<id>/jobs`, never by polling an engine.

## The rule for every piece

- One type, one store, one protocol for each concept. A second copy is a
  defect to delete, not a variant to keep.
- The surviving copy is the one closest to the model below, not the largest
  or the newest. The others are deleted in the same piece of work that moves
  their callers. Nothing is kept beside its replacement "for now".
- No work adds a new copy of a core concept while an old one is still on
  `main`. Work that would do so either deletes the old copy first or stops.
- No legacy readers. Tilo is pre-alpha, and a format that changes moves its
  data once, in one migration action, and then the old reader goes.
- UX only gets better. Every push leaves what a person can do and how well
  it works at least as good as before, on every platform it ran on, rarely
  used features included. Before a deletion, list every capability of what
  goes (each action, shortcut, gesture, setting, platform, error state) and
  point each one at the place that now does it. Anything with no such place
  blocks the deletion until it exists there.
- Built but unused still counts. Capabilities nobody uses yet but that Tilo
  needs stay and keep working: sending files to nearby devices, casting
  (Chromecast, Miracast, AirPlay), the same open apps on every device (web
  and every native install), agent chat, Shortcuts, shells written in
  tilolang, audio across several speakers and devices, displays arranged
  across devices, the full installer. "Never used" or "never shipped" is no
  reason to cut one.
- Proven end to end. A capability counts as kept when a fast, realistic
  test drives it through its published actions and views, the way a person
  or agent would, and the test runs in `t test`.
- A removal lands whole. Deleting the old copy, moving every caller onto the
  survivor and the survivor's own changes are one branch and one push (or a
  few commits pushed together), never "move now, delete later". Older
  branches and worktrees on the same code are taken into that push or
  deleted, not left parked.
- Test once, at the end. The change is finished first (old code gone,
  callers moved, `t cargo check` clean); then clippy and the focused tests
  run once, and it is pushed. No test or cost runs on a half-done change.
- The words-that-go check fails only when a new name carrying a specific
  word appears. More uses of a name that already exists are printed, not
  failed. Generic words (record, item, kind, document, surface, instance,
  held) are only counted, and nothing that falls ever fails.

---

## 1. Devices

### Today

A device's key and certificate live in four places, and none of them reads
another:

| Store | Code | Location | Size |
|---|---|---|---|
| Native device key and certificate | `tilo-device-identity` `IdentityPaths` (`persist.rs`, `keychain.rs`) | `$TILO_IDENTITY_ROOT`, else `/var/lib/tilo`, else `$XDG_DATA_HOME/tilo`: `device-private/keypair.bin`, `device/cert.bin`, `device/certs/<user>.bin` | 6,922 |
| CLI and agent login | `tilo-device-identity` `login` `Place`, `file_certificate` | The certificate above, filed under `device/certs/<user>.bin`. Where the device enters is the daemon's to know, in memory and from its copy of the person's own workspace, and every program asks it (`account#who`). | 365 |
| Browser device keys | `tilo-browser-runtime` `web/device-key.mjs` | IndexedDB, a pool of non-extractable keys per tab | 423 |
| Boot unlock key | `tilo-auth` `userkey.rs`, `record.rs`; `tilo-unlock-protocol` `BootKeyCert` | `/var/lib/tilo/auth/`, the root keyring | 445 + 1,818 |

There are three certificate formats: `tilo_user_identity::DeviceCert`
(postcard, `cert.rs`), `tilo_unlock_protocol::BootKeyCert`, and the
`tilo-auth` sibling `Token` (`sibling/`, 2,085) that wraps the second. MLS
credentials in `key-group` are a separate layer and stay.

There are two device lists: `CertifiedFleet` (`tilo-user-identity`
`fleet.rs`, a verifier) and the devices the person's own workspace holds
(`so.tilo.devices/devices`), which every host reads through the daemon that
keeps it.
Revocation has a defect: `tilo-server` `admin.rs` `persist_fleet_revocation`
defaults to `$XDG_CONFIG_HOME/tilo/identities`, while every other reader
uses `tilo/identity`. It finds no fleet there and returns success, so by
default a revocation never reaches the fleet.

There are seven ways to add a device:

1. The imager: the person's root on the machine writing an image certifies
   the image's new device key, and the key and certificate go where a
   ticket's login keeps them (`imager` `handoff.rs`).
2. `tilo-identity` `certification_ceremony` with `pairing_code` and
   `enrollment/` (about 2,500).
3. The `tilo-enroll` and `tilo-approve` binaries in `tilo-server` (1,741).
4. `tilo login` with a ticket, the certify action on the device that issued
   it (`tilo_device_identity::login`, `tilo_peer_state::new_device`, `tilo-cli`
   `login.rs` and `device.rs`), and the tickets a page's host serves it
   (`tilo-server` `web/tickets.rs`).
5. The browser's `web/pairing.mjs` (348).
6. The boot-key enrolment in `tilo-auth` `sibling/enrol.rs` (1,121).

### What survives

- **One key per device, in one store.** `IdentityPaths` is the only place a
  native device keeps its key and certificate. The browser keeps its key in
  IndexedDB because it has no file system, but it holds the same `DeviceCert`
  bytes. A device logged in with a ticket keeps nothing beside its key and
  certificate: no site, no server certificate, no `login.json`.
- **One certificate format.** `DeviceCert`, checked by `verify_device_cert`
  for every caller. Before a disk is unlocked the device key is inside it,
  so the unlock request is signed by a boot key on the unencrypted
  partition. That key holds an ordinary `DeviceCert` with no capabilities.
  The signed request names the volume and slot, and the approving device
  checks them against what it recorded when unlock was set up. There is no
  `BootKeyCert` and no second verifier.
- **One device list**: the person's devices, a resource of the person's own
  workspace, signed by their root. Nothing keeps a second copy. Revocation is
  an entry in it and reaches every device of the identity. Every key the
  root certifies is an entry there: a device, an agent's grant key or a
  script's key, each revoked the same way. The list says who a key is; each
  workspace's grants say what it may do.
- **One way to admit a member**: `workspace/members#admit`, on the one
  dispatch. An invitation's redemption, a crowd join and a member adding
  someone all call it. An invitation's bearer calls with the issuer's grant,
  checked when it is used. Issuing an invitation needs what redeeming it
  needs, `member.add` for the role it gives, so no one holds an invitation
  that can only fail. One call is one change: the governance entry, the
  spent invitation and the member's record together, or nothing.
- **One way to create a person**: one action on the device's daemon makes
  the root and founds the person's own workspace. First-run onboarding and
  the CLI both call it, and it needs a running daemon. The daemon works out
  the device's class: native on Tilo OS, foreign elsewhere. It keeps the root
  and the device key in its own store, and an OS account it serves is
  admitted as its person, named by the account's display name until they
  rename themselves. A device that holds the root acts with the root's
  authority whatever its class. A foreign device without the root hands a
  capped call on to a root holder through the one dispatch, as an
  invocation record; the root holder asks once and makes the change under
  its own key, and the record waits while none is reachable. The person's
  devices record marks which devices hold the root, root-signed; an action
  that needs the root says so on its surface, as one that asks does. An
  action whose answer is a secret (recovery words, a backup) says so too,
  and its record keeps neither its arguments nor its answer. A Tilo OS device a
  root holder adds is certified native. Founding a workspace is signed on
  the founder's own device, never by a server or a page; joining one needs
  no device of one's own. A computer several people sign in to is each
  one's own device: its one daemon keeps a root per account, sealed to that
  account and signing only its calls, so a new person can found there. A workspace's VM is a device of the person it runs for, listed in their devices like any other, whose certificate reaches that one workspace alone, the way an agent's key reaches only its agent. A device's own settings and controls are its person's: a workspace's ownership or a role never reaches them, and a server added to a workspace gives that workspace's administrators an explicit grant on it, which the person sees and can revoke. An
  installed image carries no identity, at most a one-use ticket its person issued.
- **One way to add a device**: a ticket. `tilo device add` issues one and
  `tilo login <ticket>` uses it; the root certifies the new device. There are
  no tokens, codes or a join to wait on.
- **One endpoint per device, in the daemon.** The daemon holds the
  device's transport and state: the link between a person's devices,
  presence, the media relay (`tilo-live`), nearby send's transfers, the speaker
  group's clock and the resources they write. What needs the seat (capture,
  encoding, input injection, a window's sound, drawing) stays in the session.
  A speaker group plays through the device's audio service, so a headless
  device can play in one. The
  session publishes it as a provider at `devices/<id>/session/…` over
  `daemon.sock` and opens no endpoint of its own. With no session running,
  those actions answer that the device can't do it now, and wake still
  works.
- **One way the daemon starts:** the platform's service manager starts it,
  and everything else asks the running daemon. On Tilo OS and on a server
  it is a system service from boot, so the device answers before anyone
  signs in. Elsewhere (another Linux distribution, macOS, Windows) it is the
  signed-in person's service: a user unit, a launchd agent, a per-user
  service. Installing it never asks for admin rights. Only where the unit
  lives differs; the binary and its start are the same everywhere.

### What goes

| Delete | Size |
|---|---|
| `tilo-api` `Device` key file and `TILO_DEVICE_DIR` | ~120 |
| `BootKeyCert` and the duplicate verification in `tilo-auth` `sibling/token.rs` | ~550 |
| `certification_ceremony` beyond `pairing_code` | ~600 |
| Enrolment logic in the `tilo-enroll` and `tilo-approve` binaries | ~800 |
| `tilo-auth` `PeerRegistrar` enrolment (moves onto the enrolment transport) | ~400 |

Owner: **One identity** session.

## 2. Identity

### Today

A person's identity root is kept in more than one place:

| Store | Code | Location | Size |
|---|---|---|---|
| Native person records | `tilo-identity` `store/` (`unix.rs`, `backup.rs`, `recovery_words.rs`, `secure_store.rs`) | `$XDG_CONFIG_HOME/tilo/identity/{<id>.json,active,secrets/}` | 4,986 |
| Browser root | `tilo-browser-runtime` `root_identity.rs`, `web/identity.mjs`, the root as the identity store's record | IndexedDB | 738 |
| Old CLI location | `tilo-cli` `identity.rs` `earlier()` | `$XDG_STATE_HOME/tilo/identity/user-identity/pubkey.bin` | ~20 |

`tilo-identity` as a whole is 22,760 lines, with `store/`, `identity_store`,
`certificates` and `local_person` layered
over each other. `DeviceIdentity` means two different things: public
metadata in `tilo-user-identity` `lib.rs`, and a certificate plus signer in
`tilo-identity` `identity_store.rs`.

There are two recovery formats (the 24 words and `tilo-auth`'s modhex on
disk). There are five sign-in paths: the greeter
through `pam-tilo` to the `tilo-auth` daemon, `boot-signin`, `tilo-api`
`sign_in.rs` with `oauth.rs` and `SignInProof`, the browser `sign-in.mjs`,
and `tilo login`. A person's root is one Ed25519 seed on the device that
holds it; its 24 words are `tilo-user-identity`'s `recovery_words`.

### What survives

- **One identity root type**, `tilo-user-identity` (`DeviceCert`,
  `SignInProof`), shared by the OS, the CLI and the browser.
- **One person store**, `tilo_identity::store`: the `IdentityRecord` files,
  the `active` selection, and the profiles derived from them. Nothing else
  indexes them. The browser keeps its wrapped root in IndexedDB, in the same
  record format.
- **One recovery format**: the 24 words. No server keeps a copy of the root,
  wrapped by a passkey or otherwise; a new browser is added with a ticket from
  any of the person's devices, or restored from the words.
- **One login.** The identity is the person's login everywhere, tilo.sh
  included, with no separate account or password there. Other accounts a
  person has (mail, cloud storage, calendars) are connections of a
  workspace, used to bring data over or to keep using them, never a second
  identity.
- **One sign-in proof**, `SignInProof`. The greeter, the web page, the API
  and `tilo login` all prove the same thing (a current device of this
  identity) and differ only in how the person approves it.

### What goes

| Delete | Size |
|---|---|
| The old `$XDG_STATE_HOME/tilo/identity` probe, after one migration | ~20 |
| The `tilo-auth` modhex recovery format | ~400 |
| Overlapping layers inside `tilo-identity` `store/` | to be measured as they fold |

Devices and identity together come to about 6–7k lines, and they leave one
store of each kind where there are three today. Owner: **One identity**
session.

---

## 3. Sync of resources and files

### Today

A resource is an Automerge document. Its changes are carried as
`neighbour_wire::NeighbourMsg` frames (`crates/core/neighbour-wire`,
5,046), sealed under MLS content keys from `key-group` (9,993) and
`workspace-keys` (8,337).

An app's write goes from the app to the collaboration broker client
(`collaboration`, 56,611), over the `runtime-collab` wire (14,012) to the
workspace service's broker (`tilo-workspace-service` `apps/collab_app_broker`,
65,255), and into `tilo-peer-state`'s owner journal. From there it travels
one of two ways:

- over `tilo-server/1` as `Envelope::Delta` (`tilo-server-protocol`,
  `tilo-delivery-protocol` 1,946, `tilo-server-session` 4,819,
  `tilo-server-client` 3,982), admitted by the host in `peer/` (16,274) and
  kept by `persistence.rs` and `store/` (9,264);
- or over the direct device link `tilo-peer-state-direct/3`
  (`tilo-peer-state` `direct.rs`), kept by `tilo_peer_state::store::Store`.

`workspaces/link_follow.rs` copies between the two.

The copies:

| Concept | Copies |
|---|---|
| Frame and document stores | `tilo-peer-state` `Store` (one frame file per write); the host's retained store (`tilo-workspace-service` `store/`, `persistence.rs`, `compaction.rs`) and `tilo-server-store` (1,298); `fs` `kept_documents` |
| Compaction | `tilo-workspace-service` `compaction.rs` and `store/compaction.rs`; `tilo-peer-state` `sealed_compaction`; `tilo-workspace-service` `sealed_documents` and `repaired_documents` |
| Document sync protocols | `tilo-server/1` deltas with co-edit and anti-entropy; `tilo-peer-state-direct/3`; `tilo-personal-store/1`; `tilo-server-sibling/2` |
| Resource index | `resource-store` (7,475, used only by the workspace service); `tilolang-tilo` `index/` on redb (10,614); `tilolang-tilo` `TiloStore` |
| Brokers | `collaboration` `broker*.rs` (13,813); `collab_app_broker` (65,255) |
| Settings | the host's `person_settings` (the person's and the device's sections are resources of the workspace, read through `settings-store`'s typed readers) |
| File tree | `fs` `tree_sync.rs` (the workspace service, the standalone runtime, and the fs daemon through the session's `files-tree` socket), with one tree writer since a60e81727f2 |
| File bytes and keys | `fs` `FileArea` with iroh-blobs and its own ALPNs (`tilo-fs/session-link/1`, `tilo-fs/file-keys/1`, `tilo/fs/certify/1`, `tilo-fs-workspace-authority/1`); `tilo-content` (4,004) with `content-device` (706); `runtime-fs` blobs |
| File presentation | FUSE in `fs/src/fs/fuse.rs`; native Btrfs and fanotify in `fs-native` (10,340); the mirror folder `sync_folder`; `runtime-fs` (14,011) |

Cloud mounts (`fs/src/cloud`, 6,815) talk to outside providers and are a
separate engine by design. Live media (`tilo-live`, MoQ) carries voice, video
and screen share only, never documents, and stays as it is.

### What survives

- **One replica engine**: `tilo-peer-state` `StatePeer` and its owner
  journal, on every device, the host included. A server is a device that
  keeps more frames.
- **One frame store**: `tilo_peer_state::store::Store`. A write is staged,
  signed, made durable and only then projected into the resource store, the
  same way for every carrier. The rows the resource store reads can always be
  rebuilt from the kept signed frames.
- **One wire and one link**: `NeighbourMsg` on one device link. Device to
  device and device to host are the same link, over iroh, and the page uses
  the same framing over WebTransport. `tilo-server/1` goes: joining is a ticket and
  there is no address lookup.
- **One compaction**: `tilo-peer-state` `sealed_compaction`.
- **One resource index**: `resource-store`, with a persistent backend taken
  from the tilolang redb pager. Every host of resources (the workspace
  service, tilolang, the toolkit) reads it.
- **One dispatch**: the daemon's, which admits a call, hands it to whatever
  runs it (an app's process, a system service, another device) and returns
  its progress and result. `broker-core` and `collab_app_broker` fold into
  it, and the word "broker" goes with them. A hosted page is a view the
  daemon opens on the app's process for that visitor: opening and acting are
  dispatched calls, the answer is the view's HTML, and the visitor's writes
  are the app's actions under their own identity. There is no page replica
  and no per-person check beside the dispatch.
- **One way to read a record**: the record API (`ResourceRead`, with
  follow). Every program reads and keeps current a record this way: an app,
  the shell, the session, a system program (calling as the device, under
  ordinary grants) and, before anyone signs in, the device reading the
  workspace founded on it. A record's address names its workspace. Writes are
  the record's typed actions on the one dispatch. A record is answered as a
  document or, for device state, as a reading, on this one path and followed
  the same way. A program that needs more extends this API; it gets no call
  of its own. The session's own engine (`EngineRecords`, `settings_sync`)
  goes, and settings are records read and written like any other.
  No action lists or returns records (`keys#list`, `devices#list`): actions
  change things, and a list is a read of the records the caller may see.
  One check decides every read, apps' included: only grants the record's own
  workspace issued count, and a call made for someone gets the smaller of
  the caller's grant and theirs. An app acting for a person never reads or
  does more than its manifest declares; its own part of the workspace is its
  own. An app's process holds no replica and no key: the device's one store
  answers it through resource commands and change notices, its changes
  decided per caller like a read, with no channel join beside it.
- **One way to ask**: whenever a person is asked for a yes, a choice or a
  secret, the question is an asked invocation record answered through
  `tilo.invocations#resolve`. An app, an agent, a web page in the Browser, an
  automation, and an outside process through the portal (the session raising
  it on its behalf) all ask this way. A secret answer goes only to the asker,
  never into the record, its history, sync or logs. An action that asks
  declares it on its surface, and the dispatch asks once, stating facts
  taken from the action's declared argument fields.
- **An invocation follows its origin's permissions**: an invocation record
  names the resource it runs for. Its caller, or anyone who may write that
  resource, may cancel or retry it; only the identity an ask names may answer
  that ask, bounded by the smaller permission. What a run cost (provider,
  model, tokens, cost) is a field of its record, written where the model call
  returns.
- **The operator chooses workspaces, membership chooses people**: a host's
  operator decides which workspaces it stores and serves and who may create
  one there. A hosted workspace's signed membership decides who reaches it,
  guests admitted by entry included; the operator's default trust refuses only
  a device that reaches no workspace the host serves. The operator's levers
  are to stop hosting a workspace and to block a specific device or key.
- **A checkpoint is a cache, never an authority**: a compaction names the
  hashes of the writes it covers, and a key-holding reader reads it only after
  checking that its content is the merge of those writes, each checked as an
  ordinary write. A reader that cannot check it fetches the writes instead.
  Holding the compact grant lets a member make a checkpoint, never make one
  that says something the writes don't.
- **Fixtures write like everyone else**: a test seed, a demo or a scenario
  writes through the seeded app's declared actions on the one dispatch, so the
  store's schema check applies to it unchanged. A seed gets no store command,
  verb or write path of its own, and a store that knows no schema refuses
  every write.
- **Files are resources.** The tree syncs as ordinary records through
  `tree_sync` only. Bytes go through one content store, moved by
  `RouteContent`, with keys from `workspace-keys`. A person sees it as one
  `~/<workspace>` folder on every computer, Tilo OS and any other Linux
  alike, with files on demand and a keep-offline pin. The device daemon
  serves it through the platform's own file-provider (FUSE on Linux, File
  Provider on macOS, Cloud Files on Windows) with one caller resolver for
  who wrote. There is no second copy synced onto disk beside it: the mirror
  folder goes.
- **Settings are resources.** A person's settings (appearance,
  accessibility, shortcuts, language) are records of their own workspace,
  so they follow the person to every device, a shared one included. A
  device's settings (displays, power, hardware, network) are records at
  `devices/<id>/settings/<section>` in its owner's own workspace, read and
  changed by others through ordinary grants. Each section is one resource
  with a typed action per key (`settings/appearance#set-theme`), so a grant
  can name one setting and search finds each field at its own address; an
  app's preferences are one resource per app. There is no generic
  `set(key, value)`. A write that must not overwrite a newer one (a display
  arrangement) names the revision it was based on as an argument of its
  action, and the action refuses when the record has moved past it.

### What goes

| Delete | Size |
|---|---|
| `Envelope::Delta`, co-edit and anti-entropy (`tilo-server-session`, `tilo-server-client` co-edit, delta coding in `tilo-delivery-protocol`) | ~3,500 |
| The host retained store: `tilo-workspace-service` `store/`, `persistence.rs`, `compaction.rs`, and `tilo-server-store` | ~17,500 |
| `link_follow.rs` and the two-carrier path | ~1,500 |
| `tilo-personal-store/1` and `tilo-server-sibling/2` | ~1,500 |
| The tilolang redb index and the `TiloStore` mirror, keeping the pager | ~15,000 |
| The overlap in `collab_app_broker` | ~30,000 |
| `tilo_fs::sync` and the fs ALPNs | ~2,000 |
| `fs` `kept_documents`, `runtime-fs` blobs, `content-device` | ~3,000 |

About 75k lines. Owners: **One engine** session (replica, frame store,
link, compaction, broker, files); **tilolang onto the resource store**
session (the index and `TiloStore`); **one workspace** session (settings).

---

## 4. The app framework

### Today

There are two ways to write an app over one model, `tilo-resource`
(`#[derive(Model)]`, `#[resource]` with `#[action]` methods; 57,051 plus
7,556 of macros):

- **Rust.** `tilo-app` (92,545, of which `framework/` is 61,817):
  `ResourceApp`, `framework::run`, `ViewCx`, `Dispatcher<R>`,
  `ResourceSurface`, `ui::main!`. Views are written imperatively against
  ui-core widgets. 64 apps under `apps/`, 1.28M lines.
- **tilolang.** One file declares types, actions and views, and a view names
  its actions directly (`apps/tl/counter/app.tl`). The compiler `tilolang`
  (54,152), the WebAssembly host `tilolang-runtime` (67,069), the bridge
  `tilolang-tilo` (96,643), and `tilo-app-window` (13,771), which turns view
  patches into ui-core widgets. 13 apps under `apps/tl`, 16,663 lines.

The copies:

| Concept | Copies |
|---|---|
| Action registries and dispatchers | `tilo_actions::ActionRegistry` (`actions` 47,473); `tilo-app` `Dispatcher<R>`; ui-core `bound_dispatch` and `LocalDispatch`; ui-core `headless::RootDispatch`; `tilolang_runtime::registry::Registry`; `tilolang-tilo` `DynAction` republishing into the first; Settings calling `os-actions` directly (about 33 files) beside the connectivity service's actions, which become the only caller (the `os-actions` `Dispatcher` went in 3864b5fd04d); `session-actions`' own way of carrying a call to another device, which moves onto `tilo-actions/1` (it is already a registry provider, and its `surface.toml` is generated) |
| Rust view models | `ViewCx` views; `tilo-view` `#[view]` with `view-macros` (1,682, used only by Chat) |
| Widget output | ui-core paints to a canvas, and 162 widget files also write HTML (`composition.rs` `HtmlCx`) |
| tilolang hosts | `apps/tilolang` (21,346, one process per installed app, toolkit linked in); `tilolang-process` (14,358, one process per workspace and author) |
| Rust model logic | each app's own code, and again as `apps/*-model` (34 crates) plus `apps/*-model-wasm` (31 crates) with `tilo-model-guest` |
| Launch paths | a Rust app binary through `app-launch` and the `launcher` sandbox; `apps/tilolang`; `tilolang-process`; in-process written views and the session's own layers; `app-keeper` for non-Tilo programs; Rust models as WebAssembly |
| Permission checks | `action-policy` (12,437); tilolang's own `tl:host` commit and `SimpleAuthority`; the SDK's `PermissionContext`; Chat's `ChatPermissions` |
| Shell | `apps/tl/shell` (6,990), over the desktop session-core holds |
| App models compiled into hosts | `tilo-server` links Chat and Tasks; `tilo-agent-session` links most models natively; the WebAssembly `tilo-model-engine` is the third |
| Apps in both languages | Chat (68k Rust, 963 tilolang), Tasks, Notes and Word Processor, Mail and Messages |

### What survives

- **Two layers: a UI library and an app SDK.**
  - **The UI library** is raw UI that knows nothing about Tilo: widgets,
    layout, text, input, animation and rendering on every platform, good
    enough to compete as a UI library on its own on speed and developer
    experience. It holds no resources, actions, workspaces, identity,
    history, presence, search or permissions. ui-core's `history_mode`,
    `history_source`, `presence`, `search`, `action_palette`, its action
    dispatch and its shell widgets leave it.
  - **The app SDK** gives an app the whole of Tilo (workspace, resources,
    actions, history, identity, permissions, search, presence) with a pleasant
    developer experience. An app changes state only through actions, and
    everything it holds is in the normal store with permissions, history and
    an author. It may use the UI library directly, but only by declaring how
    that UI maps onto its resources, actions and views. tilolang and Rust are
    two languages for this one SDK, each with one path into the system; the
    showing of history, presence and search is SDK views over the system's
    services.
- **One model ABI**: `tilo-resource` (`DynResource`, merge kinds,
  `ActionCx`), with models running as WebAssembly in every host.
- **One action registry**: `tilo-actions` (the published tree,
  `ActionEnvelope` replay, `ActionProvider`), with `action-policy` deciding
  every call and `automation` on top. The OS and session verbs are
  providers in it, and their descriptions are generated.
- **One view path**: views, from tilolang view patches through
  `tilo-app-window::reconcile` or from a Rust app, become widgets of the UI
  library. The web page's HTML comes from one renderer over the semantics
  tree every window already produces (role, name, value, state, actions,
  bounds), written as semantic HTML in the shared components' styles; a
  press on the page is that node's accessibility action. It serves Rust and
  tilolang windows alike, replaces every widget's `html` (`HtmlCx`) in one
  push once the page walks match, and there is no second renderer from view
  nodes beside it. The same tree reaches screen readers through the session: a window
  sends it over its own connection and the session is the one AT-SPI (and
  platform accessibility) exporter, so no app process holds a bus.
- **One sandbox for app code**: `tilolang-process`, started through
  `app-launch` and the `launcher` sandbox when an action is called or a view
  is shown, one per workspace and app, serving every window, page viewer and
  windowless call of that app, and stopped when idle. It is how the system
  runs app code, not an app being open. Idle means no call or window for a
  while, not the last window closing, and a spare sandbox started ahead
  becomes the next app's process, so an open starts no process. One app's
  windows never share a process with another app's: a spare started ahead
  becomes that app's window process too, and the compositor names a
  window's app from the instance the session gave it, never from the
  process. `app-keeper` stays only for programs that are not Tilo
  apps.
- **One undo boundary and one executor, in the app SDK**: `LocalDispatch`.
  The UI library's widgets take plain closures; an app's state changes only
  through its typed actions, which agents and automations call. A press on a
  control is not itself a published action: an agent shown a window presses
  through the same accessibility action a person's assistive tool uses.

### What goes

| Delete | Size |
|---|---|
| `tilo-view` and `view-macros` (Chat's views port) | ~1,700 |
| `apps/tilolang` (folded into `tilolang-process`) | ~21,000 |
| `apps/*-model`, `apps/*-model-wasm`, `tilo-model-guest`, as each app's types move into `.tl` | ~35,000 and 65 crates |
| Native model links in `tilo-server` and `tilo-agent-session` | to be measured |
| `tl:host` commit and `SimpleAuthority`, the SDK `PermissionContext`, `ChatPermissions` | to be measured |
| ui-core `bound_dispatch`, `RootDispatch` and the widget callbacks (`on_change`, `on_view`, `take_action`) | ~15,000 |
| `tilo-app` `framework/` (`ResourceApp`, `Dispatcher<R>`, `ViewCx`, `ResourceSurface`) after the Rust apps port | ~62,000 |
| The Rust apps, each replaced by its tilolang port in the same push | ~900k of 1.28M; the rest becomes engine crates |
| The Rust shell, replaced by `apps/tl/shell` | ~140,000 |

This is the largest piece by far. A port lands together with the deletion of
the Rust app once the port does everything the Rust app did, on every
platform, each capability proven by a test through its actions and views,
and gaps are fixed in the shared framework, not in the app. Owners: **tilolang onto
the resource store** session (the language, its store and the single host);
**cleanup** session (app host off the session, onto the server, one process
per workspace and app, the web page); **legacy and compatibility readers**
session (the toolkit's second action path); each app's area owner for its
port.

---

## 5. The workspace

### Today

`collab_types::WorkspaceId` (a UUID) names a workspace, but eight structs
called `Workspace` describe it (`tilo-server-protocol` `workspaces.rs`,
`tilo-api` `server.rs` and `v1/answers.rs`, `desktop-model` `tree.rs`,
`session/client` `presence.rs`, `fs` `sync.rs`, `actions`
`remote/mcp_server.rs`, `workspace-keys` testing), and `ush` and `tilo-net`
have their own id types.

Membership is the signed governance log in `tilo-peer-state`
(`WorkspaceGovernance`, `members/`, `workspace_membership.rs`), with roles
and grants decided by `action-policy` `WorkspacePolicy`. Beside it are six
more membership records: `tilo-server-protocol` `WorkspaceRoles`, a
second role vocabulary; the
`tilo-workspace-service` `workspace.rs` `Membership` device list
(`WorkspaceRuntime`, 2,004, used in 23 files); `workspace-join`
`MemberList` and `MembershipProof` (3,173); `tilo-api` `store.rs` `Grant`
with `collab_types::WorkspaceGrant`, the API server's own grant file;
`apps/workspace_memberships`, a path for agents; and `PersonalStore`, which
is not governed at all.

There are two governors: `tilo-peer-state` `owner/` `NativeWorkspaceOwner`
(19,843, "workspaces this device governs itself") and the workspace service
as governor (`workspaces/authority.rs`, `own.rs`, `own_device.rs`), in which
the server's own person administers every workspace it holds. "Governor"
still appears in 347 Rust files.

Per-person stores still on `main`:

| Store | Code | Size |
|---|---|---|
| Personal records `(app, personal)` | `tilo-peer-state` `state/personal/`, `tilo-workspace-service` `apps/personal.rs`, `session` `personal.rs` | ~6,000 with tests |
| Connections under the person's root | `tilo-workspace-service` `connections` | 5,661 |
| A person's servers | the home-peer registry file, and again as the Servers workspace (`workspaces/servers.rs`) | 4,060 |

`person_settings` and `person_vault` already keep their data in the
workspace vault, despite their names.

### What survives

- **One workspace type and one id**, `WorkspaceId`, in `collab-types`.
  Other crates use it and do not redefine it.
- **Membership is a resource.** The signed governance log in
  `tilo-peer-state` is the one membership record. Its operations are signed
  and checked against the membership as their author saw it, so every
  member's device reaches the same answer without a coordinator.
- **One authority**: `action-policy` `Role` and `Grant`, read from that log,
  for people and agents alike. Keys follow membership through `key-group`.
- **No governor.** No device administers a workspace by being the device
  that holds it, and the server's own person has no power over a workspace
  it does not belong to.
- **One set of resources per workspace.** Mail, calendar, vault, models,
  connections, agents and skills belong to the workspace, and so does what
  an agent has learned, read by the person it acts for. Every tool a model
  sees is a published action, never a hand-written list; a turn's answer is
  written by actions on its invocation record. Privacy between
  members is a reader rule on a record, never a private store. The keys
  enforce it, not a filter: a record with its own readers (a direct
  conversation, a private note) is its own resource, sealed under the key
  group of exactly those readers. Every device may keep and forward its
  sealed changes, and only the readers' devices can open them. A private
  record is never a row inside a document that has more readers.
  A resource type declares its readers, in Rust and tilolang alike: the
  record's author (a draft until it is sent), a field naming members, or
  the workspace.
- **What is a person's alone** (their keys, devices, appearance, the list of
  their workspaces, their servers) lives in the person's own workspace,
  where no other person is a member (their agents may be, acting for
  them). There is no second kind of store for it. A device enters the person's
  own workspace from its ticket and certificate, and reads the list of the
  person's workspaces from it, so that list never names the own workspace
  itself.

### What goes

| Delete | Size |
|---|---|
| The self-governing half of `NativeWorkspaceOwner` and the service's governor role, with `workspace.rs` `Membership`, `membership_watch` and the governed-access file: the relay admits the devices of the signed member list, and a hosting server acts in a workspace only by its administrator row there | ~13,500 |
| `WorkspaceRoles.custom_roles` | to be measured |
| `WorkspaceRuntime`'s live relay path (subscriber routing, presence, the live session, invocation replay), deleted in the push that carries live traffic over `tilo-peer-state` links | ~10,000–20,000 with `peer/` |
| `tilo-api` grant file, `WorkspaceGrant`, `apps/workspace_memberships` | ~3,000 |
| `PersonalStore` and `tilo-personal-store/1` | ~6,000 |
| The second workspace list | ~2,000 |
| The home-peer registry file (the Servers workspace holds a person's servers) | to be measured |
| The duplicate `Workspace` structs | to be measured |
| `fs` `workspace_authority` ACL and its ALPN | ~1,100 |

About 50–60k lines. Owners: **one workspace, no per-person stores** session
(per-person stores, accounts, settings, the workspace list); **One engine**
session (membership as a resource, no governor); **hosting** session (one
daemon, the person's servers).

### Hosting

A server is a device that stores more. It runs the device's one daemon
(`tilo-runtime`), which reads a server configuration and composes the host
library `tilo-server`: its own iroh relay, the web entry, certification and the
workspaces it governs. It has no binary of its own, and no operator verbs beside
it. `tilo-api` (21,745) is the MCP
and OAuth endpoint with its own grant and token store. A device without a
desktop runs one daemon, `tilo-runtime`, which serves its apps, `tilo
mcp` and the CLI's workspace commands over the device's one session. Both open
the same `tilo-workspace-service` through `own_device`, but each has its own
wrapper around it.

What survives: **one daemon per device**, the workspace service opened
through `own_device`. It is the device's, not a signed-in person's: a system
service holding the device key, up from boot, so a device at the greeter, before
its first sign-in or mid-update still answers `tilo-actions/1`. The daemon is the
key's one writer: it alone opens the key and keeps its one sequence counter, and
its workspace service and every other process sign through it. A second process
opening the key is refused, saying which one holds it. Two changes under one key
with the same sequence number mean a copied machine: the receiver refuses that
key, says which device, and takes it again only once the person re-adds one copy
with a ticket. A desktop session is
a client of it. It is one binary that the CLI, a server and tilo.sh all run, with one MCP socket
locally and `tilo-actions/1` frames as its remote face. A server differs only in
what it keeps and which work it takes, set per workspace. An always-on server
keeps every workspace in its person's workspace list unless that workspace's
entry turns keeping off. It learns of a new one when the list changes, never
from a timer, and joins it the ordinary way, as a device of a member.
The relay is a mode of that daemon. `tilo-api` reads grants from the
workspace, not from its own file. The agent runner is in the workspace
service (`agents`, under its `agents` feature) and reaches what only a server
has today (its Browser windows and its signing sessions) through traits, so
any device that takes agent work runs it. There are no bots: an installed
bot (`bot_members`, `workspace_bots`, with its own device identity and
program) is an automation with triggers, run like any other, and that code goes. A
device's first run needs no server: it creates the person's identity and
their first workspace, an ordinary workspace whose signed member list names
them as owner.

What goes: `tilo-api`'s grant file; the separate binaries once their verbs are
actions of the daemon. Owner:
**hosting** session.

---

## 6. Actions everywhere

### Today

The registry is already shared. Every entry point lists and invokes actions
through `tilo_actions::remote::ActionProvider` (`crates/core/actions`
`remote.rs`; `RemoteActionFrame`, `REMOTE_ACTIONS_VERSION = 2`), and about
30 types implement it: the session, the workspace service, the agent
session's `WorkspaceActions`, `SessionActions`, the standalone runtime,
settings panes. The static schema is `surface.toml` (77 files), written from
code by `tilolang surface`. Devices carry calls to each other over
`tilo-actions/1`.

The entry points on top of it:

| Entry point | Code | Transport |
|---|---|---|
| MCP server | `tilo-mcp` `McpServer` (30,360): about twelve generic tools over the catalogue (`tilo_ls`, `tilo_do`, `tilo_view` and others) | stdio |
| Session MCP socket | `session` `mcp_server.rs` and `mcp_server/` (4,700) around `tilo-mcp` | `agent.sock` |
| `tilo mcp` | `tilo-cli` `mcp.rs`, `mcp/` (3,258), with its held device | stdio, then iroh |
| tilo-control | server in `session` `test_control.rs` (about 700 lines for actions, re-encoded as "legacy rows"); client `control-cli` (input, screenshots and `raw`); `console-control` serves the greeter's and installer's actions as frames | unix socket, virtio-serial |
| `tilo do` without `-w` | `tilo-cli` `desktop.rs` | tilo-control, falling back to MCP |
| `tilo do -w`, `tilo run` | `tilo-cli` `hosted.rs` (2,746) | HTTPS `/v1`, or `tilo_api::direct` in process |
| REST `/v1` | `tilo-api` (21,745): `v1/operations.rs`, a generated OpenAPI, `answers.rs`. Each route is turned into an MCP `tools/call` (`session.rs`) and the answer is parsed back, and `src/session/{members,unattended,invocations,models,keys}.rs` intercept some calls by tool name before any provider sees them | HTTPS, SSE; `api.sock` |
| Rust REST client | `tilo-api` `client.rs` (1,515), hand-written | HTTPS |
| TypeScript SDK | `sdk/typescript`: types generated by `tilo sdk gen`, and a hand-written runtime that calls the agent tools (`endpoint.ts`, `client.ts`, `transport.ts`) | napi into `tilo_api::calls` over the line wire to the daemon's native endpoint; with a key and no daemon, MCP `tools/call` to the API server |
| Python SDK | `sdk/python`: the same, in Python | pyo3, the same |
| Legacy readers | the `principal` alias (old history is pinned to surfaces that say it; it goes with that history) and the `legacy-guest` image name (goes in the first stable release that publishes `linux-guest`) | nothing | none: the lane is done |
| Agents | the remaining Claude-specific paths in the agent engine; any agent-only notice, rank or refusal path beside the ordinary grants | agents started by triggers (the Shortcuts system: mention, DM, assigned task by default, anything watchable when set), answering where they were triggered, each turn bounded by the agent's and the asker's grants; the Tilo-agent milestone: a chat message in the company workspace becomes a small change, checked with t, committed and pushed by the agent. One agent engine since 6b9795c98ef (−15.7k) | Agent engines |
| tilolang CLI | `tilolang-cli` `session.rs` | tilo-control and the REST client |

So there are three action wire dialects (`RemoteActionFrame`, MCP JSON, and
tilo-control's rows with two server copies), REST stacked on MCP, two
parameter schemas (`ActionParam`/`ParamType` and the surface field types,
plus `param_row`), three hand-written REST clients, and 26 modules in
`actions/src/remote/` (9,869) that hand-type a domain's provider name,
action ids, arguments and answers (`person_accounts`, `person_vault`,
`workspace_activity`, `person_telling` and others), which is what
`surface.toml` exists to declare. Two crates are not entry points at all:
`os-actions` is a hardware backend with no `ActionSpec`, and
`messaging-protocol` holds messaging codecs.

### What survives

- **One registry and one dispatch**: the `tilo-actions` published tree and
  `ActionProvider`, with the daemon's one dispatch running every call
  and the caller certified. Every action has a typed input, a typed result,
  and the view that shows it.
- **One wire**: `RemoteActionFrame` v2, framed the same way on iroh
  (`tilo-actions/1`), on the local sockets and on virtio-serial. MCP and
  `/v1` are projections of it: `/v1` calls the provider directly, never
  through MCP, for callers that cannot speak iroh.
- **The CLI, MCP and the SDKs are projections, not implementations.**
  `tilo do`, the MCP tools, and the TypeScript and Python SDKs list the
  actions the registry publishes and call them. Their types are generated
  from the published schemas. None of them hand-writes a per-action handler,
  schema or wrapper.
- Every call shows: an action an agent invokes attaches its view, so the
  person sees what was done and by whom.
- **One invocation record.** A call that runs somewhere is one published
  `tilo.invocations` record: id, status, who called it, target, changes and
  output, with `get`, `cancel`, `undo` and `output` on it. Every projection
  and view reads that record, and the toolkit checks its revision.
  The dispatch's entry for a call is that record; nothing else keeps a list of
  calls in flight. The admission journal (guarded digests, prepared outside
  effects, idempotency and crash recovery) is the running device's own log
  in a device-only part of the one store, under the same id. It never syncs
  and is read only by admission and recovery. A call from another device
  carries its signed call; a call on the same device is admitted by the
  daemon from who is on the other end of its socket, with no signing step
  added. A plain peer there (the CLI, an SDK script the person runs) is that
  person; an agent is admitted as itself only with its own certificate or a
  session a device handed it, and an installed SDK program is an app on its
  held connection, under the app's grants. A caller whose connection ends cancels its call: that is the
  record's status, not a lease beside it. The one exception is held work. A call
  that starts a device job (a recording, a render) stays running until the job
  ends, held by the job's device, which keeps the job at `devices/<id>/jobs/<job>`:
  its writes land at once, closing the window or the app's process ends nothing,
  no idle timeout applies while the job runs, and when the job ends its `done:`
  action runs as a child call under the caller's grants, starting the app without
  a window if it isn't running. Cancelling the call cancels its jobs. A project
  check runs only where its project's host approved it: the device setting
  `devices/<id>/settings/checks` names each approved project's exact app, workspace,
  record and resource identity, with the check's fixed command, environment, bounds
  and this device's own source, build, scratch, image and runtime paths. The person,
  or whoever they granted that setting, approves an entry with `settings#set` and
  revokes it with `settings#remove`; the daemon reads the setting through the one
  record API and takes the check's turn with `work::takes_turn`, keeping what it
  observed in the job's record under `devices/<id>/jobs`. A check ID the setting does
  not approve for exactly that project is refused, the first run after a removal
  included, and an app names a check's ID and never its commands. A press in a
  window is a call like any other, admitted as the person who pressed, with its
  own invocation record. The compositor, which saw the input, attests it: it
  issues the window a one-use input serial and tells the daemon, and only a call
  naming a fresh serial issued to that window counts as pressed; the page host
  does the same in the browser. Work with no end, such as a language server or a
  gateway, is the app's held connection, never a job. `work::takes_turn` only places
  work that leaves the starting device. A call that leaves its device carries its caller's
  signature: a person's device signs for its person, an agent with its own
  key, a visitor with theirs, and an app's call travels as the signed call it
  serves. The receiver never runs a forwarded call as the device that brought it.
  The caller's signed invocation travels with the call as its proof and is never
  stored as a copy: the receiver checks the signature, the caller's current access
  and that the forwarding device holds the connection's lease, and checks lease and
  access again at each step that has an effect. A call an app makes while serving
  an invocation carries that invocation and its id, signed by the device running
  the app; it runs as the original caller, bounded by both the caller's access and
  the app's grants, and only while that invocation is live.
  Such a call is not asked again when the invocation was delivered to that same
  connection and the parent action declares the call's effect: the consent the
  parent was given covers it, and history records it under the parent. A call the
  parent doesn't declare asks as any other.
  An action declares its effects in the one registry: the app, kind and verb each
  step writes. A recorded step on another resource or verb than the call named is
  accepted only when the called action declares it, read from the registry and
  never from the call, and only from the device that keeps that record; history
  still names the caller, and every projection shows the declared effects.
  A device placed for one record alone, such as a hosted Browser's environment,
  carries that confinement in its certificate: the placing host, for the person,
  certifies it for that one record, short-lived and refused once stopped. Every
  replica admits its writes to that record and nothing else, with no member row
  and no keys, and history names the person. It never enters a workspace or
  redeems an invitation.
  Inside such a VM the daemon's socket opens only for the tenant's account: the
  daemon checks each connection's peer credentials and refuses any other account,
  the Browser's included. A tenant connection acts as the person's unnamed agent,
  so asked actions still ask.
  On a person's own device, a connection the daemon verifies by peer credentials as
  that person's account is the person: a workspace's ask before outside actions
  doesn't apply to it, since nothing stands between them and the action. Apps,
  agents, shell programs and forwarded links still ask, whatever identity they
  carry.
  A confined device never treats a connection as the person, and refuses to
  issue device tickets.
  Reads work the same way: a record, page, search or view opened for a person
  reads with that person's grants, never with those of the person whose
  device serves it.

### What goes

| Delete | Size |
|---|---|
| tilo-control's row dialect: the action part of `test_control.rs`, the row rendering in `control-cli` `front.rs` (tilo-control keeps only input and screenshots) | ~2,200 |
| The REST-to-MCP bridge in `tilo-api` `session.rs` and the intercepts in `src/session/` (REST calls the provider directly; members and models become providers, as a workspace's servers, apps and mail accounts already are) | ~2,800 |
| The hand-typed vocabularies in `actions/src/remote/` (replaced by `surface.toml` and generated bindings) | ~7,000 net |
| The hand-written REST clients: Rust `client.rs`, the TypeScript and Python runtimes (generated from the OpenAPI, around a kept core of about 600 lines per language for calls, events and errors) | ~6,200 |

About 18k lines, with `tilo-cli` `sdk.rs` growing by about 1k to generate
the clients.

Owner: **actions everywhere** session.

---

## Words that go

Each of these names a concept the design does not have. The word goes with
the code that holds the concept, never by a rename alone, and no new use of
any of them is added.

| Word in the code | What it is today | What the design has instead |
|---|---|---|
| governor, governance document, `GovernedWorkspace`, `NativeWorkspaceOwner`, succession | one device that admits a workspace's changes and owns its record | the signed member list every member's device checks |
| `tilo.host`, `tilo.member` namespaces, host as owner | a server's own records and authority beside the workspace | the workspace's resources; a server is a device |
| keeper | the person an agent belongs to, named apart from members | the person the agent acts for |
| roster, `tilo.workspace.agent` | a second list of agents beside members | members; an agent is a member acting for a person |
| stand-in (host, approver) | a device that signs or answers in a person's place | the person's own devices; an agent acting for them |
| `PersonalStore`, `(app, personal)`, `tilo-personal-store/1` | records outside every workspace, admitted by the identity alone | records in the person's own workspace |
| `person_settings`, `person_vault`, `person_model(s)`, `person_telling`, `person_asking`, `person_accounts`, `workspaces/people/<person>/` | per-person stores and their names on a host | the workspace's resources, or the person's own workspace |
| `AccountStore`, settings `Personal`/`DeviceLocal`/`Shared` scopes, `LiveSettings`, `settings.sock` | accounts and settings outside resources | resources: the workspace's accounts record, the person's and the device's settings as records |
| per-person cluster | one key cluster per person derived from the root | key groups per set of readers |
| own workspace, `own_workspaces`, Servers workspace (as a name) | the person's private workspace under several names | one: the person's own workspace |
| `ProfileWorkspace`, `ProfileGrants`, `Standalone` | a runtime that governs a private profile | the one daemon in an ordinary workspace |
| `own_service`, `OwnService`, `own_device` wrappers | each host wrapping the engine its own way | the one daemon |
| held, holder, `HeldWorkspace`, member copy, replica | names for a device having a workspace | a device that holds the workspace (no type of its own) |
| listing workspace, governed/member/held/own workspace kinds | kinds of workspace by who governs or keeps them | one kind of workspace |
| home peer, home-peer registry, home server | a separate registry and name for a person's server | a device of the person |
| `tilo-server-sibling/2`, sibling | a link between one person's devices outside workspaces | the device link of the person's own workspace |
| `WorkspaceRuntime`, `Membership { devices }` | a device allowlist for a relay | the member list |
| retained store, lineage, `CURRENT` generations, `tilo-server-store` | a host's second store of changes | the one change store |
| `Envelope::Delta`, `EnvelopeV2`, co-edit, anti-entropy | a second way to move changes through a host | the one device link |
| carrier (Direct, Relay, WebTransport) | a choice of path named in callers | the link; how it reaches a device is its own |
| hosted workspace, `-w`, page-host | a workspace on a server treated as another thing | a workspace; some device holds it |
| relay runtime | a server mode that forwards without being a device | a device that keeps sealed changes |
| fleet, `DeviceFleetStore`, `CertifiedFleet` | the person's device list under a third name | the person's devices |
| ceremony | a crate for a record format | the identity store |
| threshold identity | key generation across devices with no product use | nothing until a product needs it |
| enrolment, pairing, certification ceremony, handoff | seven ways to add a device | adding a device: a code one of your devices approves |
| document (as the unit), `workspace_documents`, document host | the storage format named as the thing | a resource |
| record, item, `ItemId`, item node | second and third words for a resource and its parts | a resource, inside its parent |
| kind, `KindSet` | the type of a resource under another name | resource type |
| channel ops, broker, publication, `CommittedView`, `committed_surface` | the plumbing between an app and the engine, each with its own names | a call, and a change |
| surface, `surface.toml` | the published actions under a second name | the workspace's published tree |
| instance, windowless, showing action, packaged model, native model, `model.wasm` | an app as a running thing, with ways to start it | an app is a package; the system runs its code when called |
| `DynResource`, `DynAction` | a bridge between two app models | one app model |
| `ResourceApp`, `ViewCx`, `#[view]` | Rust apps and their second view model | tilolang |
| outbox, effect receipt | app-side queues of outside effects | effects an action declares and the system carries out |
| `Answering`, `answers` (MCP) | an MCP answer re-typed for REST | the typed result of a call |
| legacy, compat readers | readers of formats main no longer writes | nothing: one migration, then the reader goes |
| `tilo-workspaces`, `(app, workspace, document)` | the old spelling of a workspace on disk and in addresses | the workspace id and an address |
| invite shelf, invitation receipts | a site-side store of invite links and a receipt log | an invitation, a resource of the workspace |
| unattended (`commands.unattended`) | a flag on the governance document | a grant |
| wait-list as a governance verb | a list kept beside members | a resource of the workspace |
| appetite, key holder, store (as a kind of device) | words from this page for what a device keeps | what a device keeps, and whether it can read |
| tilo-control row dialect, `legacy_rows`, `param_row` | a second wire for actions | the one call |
| REST clients in our code (`tilo-api` `client.rs`, SDK HTTP runtimes) | our code calling our REST edge | calls over iroh |

## Before and after

The restructure removes duplicate concepts. Each of these exists once, the
same in the browser, the CLI, Tilo OS, a phone and a server:

- one workspace: members, resources, files and permissions, with no
  per-person store, per-app accounts or second membership model;
- one identity: the same person and the same device certificates whether
  they sign in on the web, in the CLI or on Tilo OS, and the same login at
  tilo.sh;
- one resource, action and view model, used by Rust apps, tilolang apps, the
  shell, agents and automations, with one way to call an action;
- one store and one sync engine on every device;
- one daemon per device, with one endpoint, one dispatch and one admission;
  a server is that daemon keeping more;
- one UI path: one component library, one app framework, one settings, one
  history and one command palette, with the shell a tilolang app.

Lines removed measure this; they are not the goal. A cut that narrows a
platform or a feature, or makes the UX worse, is not progress.

Production lines per layer at `d81cc1750ff`, and the size each layer has
once its duplicates are gone:

| Layer | Today | After | Where the difference goes |
|---|---|---|---|
| Foundation | ~230k | ~150k | settings, accounts and app state become resources (−25k); one identity store (−6k); the per-person store (−5k) |
| Device daemon | ~520k | ~250k | `tilo-server` becomes the daemon (−16k); one dispatch and admission (−30k); app features out of the host (−20k); generic actions instead of typed vocabularies (−15k); the agent session's own Rust app host (−20k) |
| Programs | ~300k | ~200k | one copy of each program feature |
| Views and UI toolkit | ~580k | ~350k | shell widgets out (−50k); the `tilo-app` framework (−42k); the second action path and `HtmlCx` (−22k); headless as a backend, not a copy (−10k) |
| Desktop | ~385k | ~200k | the Rust shell (−95k); the session as a daemon client (−50k); one settings, history and palette (−10k) |
| OS and product subsystems | ~400k | ~300k | filed under their products, not the core |
| Importers | ~180k | ~20k | behind `tilo migrate`, reading into the one store |
| Dev tools | ~100k | ~60k | tools reuse production paths |
| Apps | ~680k | ~300k | one framework, so each app keeps only what is its own |

That is about 2.97M production lines today and about 1.9M after, with the
core near 400k. Tests go from about 1.97M to about 0.7M: tests of deleted
copies go with them, and the rest test through production interfaces.

Where the difference comes from:

- two of everything (stores, hosts, action paths, app frameworks, shells,
  settings): about 400k;
- product subsystems filed as core: about 300k;
- importers: about 180k;
- app features inside the host, and dead code: about 100k.

The Plan below tracks each piece against this table.

## Code layout

The crates are grouped by what they are, so the core stays small and
everything else is visibly optional:

- `crates/core/`: the foundation (wire types, crypto, identity, keys,
  documents, the one store) and the device daemon. Nothing per app, no
  importers, no product features. The daemon links no app crate: it reaches
  apps, the agent runner included, only through the action registry and
  the catalogue, and the crate-graph policy refuses an edge to one.
- `crates/platform/`: one API per operating-system capability (files,
  paths, clock, network, processes, secrets, notifications, clipboard,
  dialogs, appearance, locale) on Linux, macOS and the web. It knows nothing
  about Tilo either; the UI library and the core both build on it.
- `crates/ui/`: the UI library, which knows nothing about Tilo and depends on
  no Tilo crate outside `platform/`.
- `crates/sdk/`: the app SDK over the core and the UI library, for tilolang
  and Rust.
- `crates/lang/`: tilolang's compiler, runtime and process.
- `crates/services/`: device services that run on any device, headless
  ones included: speaker groups, nearby send, casting, clipboard and
  notification sync. Each is a provider that the daemon binary mounts. The
  core library never links them.
- `crates/desktop/`: the compositor, a session that is a client of the
  daemon, the portal, and what needs a seat (capture, encoding, input,
  drawing). The shell is a tilolang app.
- `crates/os/`, `crates/vm/`, `crates/compat/`, `crates/integrations/`:
  product subsystems (install and boot, hardware, VMs and sandboxes, Windows
  programs, messaging bridges, cloud mounts), each optional and none linked
  by the core.
- `crates/engines/`: PDF, media, document codecs and search, shared by apps.
- `crates/import/`: everything that reads another product's data, behind
  `tilo migrate`. A migration is a resource with actions (sources, review,
  choose, start, status, undo) on a provider, and its window is a tilolang
  view of that resource. People, agents and the CLI use the same actions;
  there is no second import window. The migration's review, choices and
  record of what has arrived are fields of that resource, so they sync and
  survive a restart. The provider runs in the daemon of the device that holds
  the data. A run is an invocation of `start`: its progress and its asks (a
  secret, a choice) are that invocation's records, and stopping it cancels
  the invocation. No migration state lives in files on one computer.
- `tools/`: the dev VM, app CLIs, examples and test fakes.

A new crate starts in the group that owns its concern, and depends only on
the groups that group may use; `tools/policy/check_crate_layout.py` refuses
any other edge and any group this list doesn't name. `crates/session/` holds
only crates being deleted or moved to services, and goes when it's empty.

## Plan

Each piece's "What goes" table is the work. This table says where it
stands; the architecture owner keeps it current. A session works down its
rows without being told; nobody waits for a nudge.

When to land. main is an unstable integration branch, not a release. Land
a change as soon as it compiles and its lint passes. Never hold it for queued
test runs, a comparison against its parent, or another lane's push: the later
push rebases, and a red on main is fixed by the next commit. A walk proves a
port afterwards, on main. Don't let a branch drift far from main
or sit finished. Don't split a removal into a move now and a delete later,
and don't hold a finished removal back to batch it with unrelated work. No
landing dates: report what is done, what is on a branch, and what a push
waits on, if anything.

Every push removes more lines than it adds. The one exception is a fix for
a regression a person sees, with the test that shows it. A fix that adds a
shared helper deletes the copies the helper replaces in the same push. A
session whose push comes out net positive stops and asks before pushing.

Measured on main at 0503c21f750; lines are Rust left in what goes. Since then Weather, Snippets, Backups, Firewall, Maps and Settings (35514b7db79, −110k) have gone; main is at 3,828,622.

| Piece | Left on main | Lines | Next push | Session |
|---|---|---|---|---|
| Main builds | `tilo-session`'s lib (81 errors from the desktop crate moves), `tilo-runtime`'s tests, `tilo-peer-state`'s 20 fixtures without an invoker | | every crate compiles, then the waiting tests run | Make tilo-session's lib compile |
| Desktop session | `crates/desktop/session` (199k, tests 98k) and `session-core` (59k). The seat stays: windows, input, displays, sound, capture, served as providers on the device's dispatch (9ba6d815abb). Everything else goes: the Rust shell's drawing (`shell/`, the profile card, app tabs, the layout chrome) for `apps/tl/shell`; the session's MCP server (Actions); its own people, roster, search, notifications, workspaces and Nearby copies, onto the daemon's services and the tl shell. New shell behaviour goes into `apps/tl/shell` and the seat, never into the code that goes | ~150k of 357k, to be measured per module | main builds, then the shell's drawing goes once the tl shell gaps close | Make tilo-session's lib compile, with the two tl shell gap sessions |
| Rust apps | Terminal 20k, Slides 22k, OSK 13k, Remote 10k, Messages 8k, TOTP 7k, Discord 5k, Poker, Phone (Settings' accounts pane is back, 5cf3902e307; `protect capture` is in the language, 40c3a16728b): Terminal, Slides, OSK, Remote, Messages, TOTP, Poker, Phone, Discord (held until it is at parity with tl Chat's Discord), and their `*-model` crates; Settings, Logs, Console, Windows, Shortcuts, Publisher, Thumbnails, Typst Editor, Camera, Chip Design, Diffmerge and the model providers are ported and deleted | ~91k | TOTP (`#code`, a view timer), Slides (a handler binds an action's answer), then action needs with the GPU device fact, then Messages | Rust apps to tilolang |
| Imager | nothing: the installer stays a Rust program on `tilo-ui` (383f9558a7d), not a Tilo app (its manifest says so; it keeps no history of its own and serves its steps through the one actions protocol), and its own account capture and first-boot import are gone (6a594ad2dac); accounts come over with `tilo migrate` | 0 | | Imager to tilolang (done) |
| App framework | `tilo-app` `framework/` (62k), `tilo-view` and `view-macros` (1.6k) | 63k | goes with the last Rust app | Rust apps to tilolang |
| One engine | `collab_app_broker`, `collaboration`; the join is deleted, with the shared-checklist two-device test passing | ~103k (broker 64k, collaboration 39k) | in parallel, each push deleting what it replaces (history and undo live in the store, a6758222e2e; calls and presence run through the daemon, dfaa6505f17): Rust apps read and write through the device's store (no record copy in the app); undo and history from the one history service; presence and carets over tilo-live frames; the broker's per-app services become the device's actions and resources under `devices/<id>/…` through the one dispatch, and agents' calls reach a Rust process through `provide_app`. Then the rest of both crates goes. The declared-kinds limit is in the one reader (d35581e9297) | One engine |
| One endpoint | the VM's own broker and identity broker, the host's stand-in key | | the VM runs the daemon with a certificate reaching its one workspace | One endpoint, One identity |
| Hosting | `tilo-server` as a second host (47k) | 47k | `tilo-server` onto the one daemon, one push | tilo-server onto the one daemon (Core: hosting) |
| Actions | `tilo-mcp` (27k), `tilo-api` (21k), `action-policy` (14k); prompts not yet asked records; the session's confirmation copy (97c9e4d5765) | 62k | asks become asked records, the session's confirmation onto them | Core: actions everywhere, UX regressions |
| tilolang store | `workspace.members` is the Workspace package's root, served by each host from its member list; open, switch and scroll above their bars (289, 60 and 25 ms). The index is resource-store's in memory, rebuilt from snapshots; no disk pager unless a measurement asks for one | | | tilolang onto the resource store |
| Agents | the other agent engines, the governor | | one runtime | Agent engines |
| Port parity | every deleted Rust app and the Rust shell, audited feature by feature against its tilolang app and restored there: Chat, Settings, Files, Browser, Writer, Sheets, PDF, Books, Mail, Calendar, Contacts, Tasks, the video editor, Music, YouTube, Photos, Notes, Canvas, Scratchpad, the developer apps, the utilities, the shell, the web desktop, calls, agents and the installer | | one restore per area, each with its test | one session per area |
| UX regressions | untested: app joins and onboarding in the session, Terminal approvals, the web desktop's style menu; to do: the theme page's Import button; the Discord bridge, which runs in neither form since its host side went in e8f0b4880d4 (Chat's origin runs the worker, access control, the token in the vault, the old record carried over, then linking and voice) | | each tested once main builds | UX regressions |

Tests prove seams between systems at the depth of an end-to-end test and the
speed of a unit test, through production interfaces. A test that restates
its implementation, tests a deleted copy, or checks what another test already
covers is deleted, not kept or ported. Tests go from about 1.97M lines to
about 0.7M.

Progress is measured as net lines removed on `origin/main` in each piece's
crates since `d81cc1750ff`, and as copies of each concept left.
