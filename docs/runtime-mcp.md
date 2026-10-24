# Native shared-runtime MCP host

`tilo-runtime-mcp` exposes the action directories of native Tilo apps on one
newline-delimited MCP stdio connection. A normal framework GUI launch joins the
same per-user runtime automatically. The first app starts the background daemon;
later apps and MCP clients reuse it. Individual apps need no manifest, service
descriptor setup, MCP registration hook, or app-specific routing.

Build the host on macOS or Linux:

```sh
cargo build -p shell --bin tilo-runtime-mcp --no-default-features
```

In an installed layout, app startup attaches to a runtime host beside the app,
then on `PATH`. Inside the Tilo desktop (`XDG_CURRENT_DESKTOP` names `tilo`
among its colon-separated entries; the Tilo login exports `tilo:GNOME`) that is
only `tilo-runtime-mcp`, so an app never prefers the standalone host there. Elsewhere — stock macOS or a
foreign Linux desktop — the standalone `tilo-runtime` comes first, then
`tilo-runtime-mcp`. Packages built by `scripts/browser/package.py` carry
`tilo-runtime`. In a source checkout on a foreign desktop, build the host once:

```sh
cargo build --locked -p tilo-runtime --bin tilo-runtime
cargo run --locked --profile thin-app -p ebook
```

Source apps also find hosts in `target/debug`, `target/thin-app` and
`target/release` in the same checkout, so the host and app need not use the
same profile. A missing source host reports the exact build command. Installed
apps keep the beside-app and `PATH` lookup; they never search Cargo profiles.
Set `TILO_RUNTIME_BINARY` only when packaging or testing must select a
specific absolute host binary. Startup errors are printed instead of silently
running an app disconnected.

Without configuration, the persistent profile is
`$HOME/Library/Application Support/Tilo/runtime` on macOS and
`$HOME/.local/state/tilo/runtime` on other Unix systems. An absolute
`TILO_RUNTIME_PROFILE` selects another profile, which is useful for isolated
development runs. For a fresh, separate development workspace, choose a private
directory once and reuse it while editing:

```sh
export TILO_RUNTIME_PROFILE="$(mktemp -d /tmp/tilo-app-dev.XXXXXX)"
cargo run --locked --profile thin-app -p ebook
```

This profile has a separate runtime workspace and identity state. It does not
unlock or repair a personal profile whose identity keys are unavailable; preserve that
profile and recover its identity through the normal identity workflow.

The daemon owns the profile independently of every app and
MCP client: closing one app or one MCP connection does not stop the daemon or
the other apps. Only one daemon may own a profile, and private-directory and
socket checks reject unsafe or competing owners.

The standalone runtime keeps its workspace's files in the profile and shows
them in `$HOME/Tilo/<workspace>` (`~/Tilo/Home` for a new profile), an ordinary
folder any program can use. Changes cross both ways within about two seconds:
a file saved in the folder becomes a version in the workspace's history, and a
document in a legacy format (`.md`, `.docx`, `.odt`, …) is imported through its
codec. A file changed on both sides keeps the folder's version; the
workspace's version stays in its history. Symbolic links in the folder are not
imported. On Linux, where `/dev/fuse` exists and `fusermount3` is on `PATH`,
the workspace is mounted at that path instead (visible to this user only), and
the folder takes over again whenever it cannot be. A profile the Tilo desktop
serves has neither: the desktop projects its files itself.

## Assistant connection

Automatic app joining does not install or silently configure an assistant.
Point the assistant at the existing host with `--connect` and the same profile:

```json
{
  "mcpServers": {
    "tilo-runtime": {
      "command": "/absolute/path/to/tilo-runtime-mcp",
      "args": ["--profile", "/absolute/path/to/persistent-profile", "--connect"]
    }
  }
}
```

The first ordinary app may start the daemon before the assistant connects. A
`--connect` client can also start it when no app is running. Stdio EOF ends only
that proxy connection.

A profile named with `--profile` or `TILO_RUNTIME_PROFILE`, or `--standalone`,
always selects this standalone runtime.

### Inside a Tilo session

Inside a Tilo login, `--connect` with no profile relays to the desktop itself:
the session's **agent door** at `$XDG_RUNTIME_DIR/tilo-session/agent.sock`.
It never starts a standalone daemon there — a second, empty authority beside
the desktop would answer every question about it wrongly. When the door is
missing (first login not complete, or a compositor too old to serve it) the
client fails with `the desktop's agent door isn't available: <reason>` and how
to go on; `--standalone` runs a separate runtime anyway.

Whether a process is inside a Tilo login does not rest on
`XDG_CURRENT_DESKTOP`, which a terminal multiplexer, `ssh` or a service
manager can each drop. The environment names the desktop (`tilo` among its
colon-separated entries) **or** the account's runtime directory holds the
compositor's own sockets (`agent.sock` or `admin-control.sock` in
`$XDG_RUNTIME_DIR/tilo-session/`; `/run/user/<uid>` when the variable is gone
too). The environment can only add to that evidence, never subtract. The same
test decides which host an app launched from such a terminal attaches to.

```sh
tilo agent connect claude
```

That registers the door, `tilo-runtime-mcp --connect`, with Claude Code for
every directory (`claude mcp add --scope user tilo -- /usr/libexec/tilo/tilo-runtime-mcp --connect`);
`tilo agent connect codex` does the same for Codex.

The door serves this login's own processes only: same account, same user and
mount namespaces. A sandboxed app is refused and uses its own collaboration
connection. A process inside an app the session launched — the terminal an
agent runs in — is admitted to ask, and that app is named in the question.

Nothing a client sends after its `initialize` is read until the person
answers, in trusted chrome: "Claude Code (node, pid N) in
org.alacritty.Alacritty — Work in “Home” as the agent claude-code". The same
decision is the `permission-prompts` provider's `allow-once@…`,
`allow-session@…`, `allow-always@…` and `deny@…` actions, so the owner can
also answer remotely; those take optional `agent` (an existing agent member,
or a new one by that name) and `preset` parameters. A refusal reaches the
client as a JSON-RPC error on its `initialize`, and the connection closes.

Admitting a client makes it an agent member of the workspace in front of the
person, through the same membership Settings → Agents shows and removes. Its
connection is then served the MCP tools as `Caller::local_agent(agent)` in that
workspace's room, in an execution of its own: every query and invocation
passes the broker's ordinary gate and is attributed to `agent:<id>`. An agent
holds no role, so it reaches only what the person grants it there (Settings →
Agents → Review access); removing it ends both its catalogue and its authority
at once. There are no pixel (`computer`) tools on the door.

"This session" and "Always" remember that this client — the executable the
kernel names, under the client name it gives — is that agent, and let it in
without asking wherever that agent is still a member. "Always" is kept in
`$XDG_STATE_HOME/tilo/agent-door-bindings.json` (0600), never in the
replicated workspace document, and only a workspace owner may give it. Any
process of the same account can claim the same name, so a remembered client is
a convenience against repeated prompts, not a credential: what it can do is
still exactly the agent's grants.



### The agent tools

An agent on the door is offered five tools, addressed by **path** and answered
as text. A path is `app/resource/resource#action`: the app (`notes`, the last
part of its id), resources by their title, kind or the app's own key
(`Todo`, `body`, `notes.page:3fa2…`), any case, and the action after `#`. A
name containing `/`, `#` or `\` writes it with a leading backslash. Paths are
resolved against the live directory on every call, so they mean the same thing
after the compositor restarts; the opaque ids the broker mints per publication
(`tilo-ui-bound:v1:<uuid>:<uuid>:<hex>`) are never shown and need never be
sent. A resource published beside its owner rather than inside it (a note's
body) is found under the owner when its key names the owner's id; where a title
fits two resources the owner gets it.

| Tool | Answers | Example |
| --- | --- | --- |
| `tilo_ls` | apps; an app's resources and actions; one action's parameters | `{}` · `{path: "notes"}` · `{path: "notes/Todo#rename"}` |
| `tilo_read` | a resource as compact text: markdown with `{#block id}`, a list of what an app holds | `{path: "notes/Todo"}` |
| `tilo_do` | the action's result, waited for (default 5 s) | `{path: "notes/Todo/body#append-paragraph", args: {text: "Buy milk"}}` |
| `tilo_find` | ranked one-line hits, ready to pass to `tilo_do` | `{query: "checklist add", app: "notes"}` |
| `tilo_status` | how a pending invocation went; `cancel: true` stops it | `{invocation_id}` |

```text
> tilo_ls {path: "notes"}
notes  (so.tilo.notes)
Notebook/  notes.notebook
Todo/  notes.page
Groceries/  notes.page
#new-page(title?)  New note
#read(item?)

> tilo_read {path: "notes/Todo"}
# Todo
- [ ] milk {#b1}
- [x] bread {#b2}
## Later {#b3}

> tilo_do {path: "notes/Todo/body#append-paragraph", args: {text: "call mom"}}
ok
{"block":"b9"}
```

`tilo_do` answers `ok` with the action's result, `failed: <code>: <message>`
(with the action's parameters after an `invalid_arguments`), or `pending <id>`
when it ran past `wait_ms`; there is no receipt to poll for an action that
finishes. `calls: [{path, args}, …]` runs several in order and stops at the
first failure. `notes#open` starts an installed app and lists it. `tilo_find`
searches apps by name, then resources, then actions named as searched, then
actions described so; it does not search files. It asks every running app at
once, and an app that does not answer is named as not searched rather than
failing the search. Listings
carry names and one-line labels only; `tilo_ls path#action` gives an action's
parameters when it is wanted. Results are text, with no structured copy beside them. Measured (the
`mcp_agent` tests keep these in bounds): the agent tool list is 2,900 bytes of
JSON against 4,546 for the classic one, and a four-block Notes page reads as 71
bytes of markdown against 370 for the structured `read` answer of the same page.

A client that parses directory rows (peer ids, `tilo_query`'s grouped pages)
is served the **classic** tools, below, anywhere but the door.

## Agent host (MCP beside local relay)

A Cursor/Grok-style box on an existing Linux OS should not install the Tilo
desktop or OS image. Keep three roles distinct: the **relay** (store-and-forward),
the **home peer** (always-on replica — same OCI as the relay Compose), and the
**agent host** (MCP client → local `tilo-runtime` enrolled under the user).

Preferred layout: run `tilo-relay` as a home peer, run `tilo-runtime` beside it
with MCP on loopback (`TILO_RUNTIME_MCP_HTTP=127.0.0.1:7391` plus
`TILO_RUNTIME_MCP_HTTP_TOKEN_FILE`), enroll the runtime as a personal device
(`tilo-enroll pair-device --headless`), and point the assistant at that MCP.
Never copy `user-identity/privkey.bin` into the container.

Operator Compose and image: [`os/containers/tilo-agent-host/`](../os/containers/tilo-agent-host/).
Architecture: [`architecture/agent-host.md`](architecture/agent-host.md).

## HTTP endpoint

A hosted machine (for example a provider's per-user VM) exposes the same tools
over Streamable HTTP. Set `TILO_RUNTIME_MCP_HTTP=HOST:PORT` for the daemon,
with `TILO_RUNTIME_MCP_HTTP_TOKEN_FILE`, whose nonempty contents callers
present as `Authorization: Bearer …`. A token is required on loopback too:
every process and account on the machine can reach a loopback port. Requests
without the token cannot create, use or delete a session, drive `/watch`, or
manage viewer invitations. Local assistants can use the owner-only Unix socket
with `--connect` instead; inside a Tilo login that still selects the desktop's
agent door, never an accidental standalone daemon:

```sh
TILO_RUNTIME_MCP_HTTP=0.0.0.0:7391 \
TILO_RUNTIME_MCP_HTTP_TOKEN_FILE=/run/secrets/mcp-token \
  tilo-runtime --profile /data/tilo --daemon
```

The standalone daemon defaults to the profile's own room. A host may explicitly
set `TILO_RUNTIME_MCP_ROOM` to the canonical UUID of a room the runtime has
**already joined**. Startup verifies current certified device admission and
that its root principal is the selected profile actor. An invitation, saved
room name, client label or supplied session metadata cannot select an audience.
Missing, fenced or stale identity fails startup. The runtime checks current
room-selected custody on every shared-room query, invocation, retry, status and
cancel request; the action broker also rechecks current room policy on admission. Selecting a room
never enrolls it, requests keys or changes membership or grants.

A shared-room endpoint exposes the semantic action door only. It does not attach
the private profile's computer input/watch lease, partner viewer or media reader.
The native endpoint and local desktop keep their existing scope. Other runtime
hosts refuse a different MCP audience unless they implement current-admission
validation explicitly. The endpoint's room stays fixed for its lifetime; clients
cannot switch it through `initialize` or tool arguments.

This audience selection does not itself publish semantic presence. Authenticated
invocation transport, exact mounted app/record/resource geometry and presentation
coherence remain necessary before a panorama can show real semantic agent work.

`POST /mcp` carries one JSON-RPC message and answers with JSON; `initialize`
without an `Mcp-Session-Id` opens a session and returns its id, later requests
must carry it, and `DELETE /mcp` ends it. No server-initiated stream is offered.
`GET /health` needs no token.

Each admitted session receives a server-issued execution UUID, returned in
`capabilities.experimental["so.tilo.mcp"].execution_id` at initialization.
Repeated initialization on that session keeps the UUID. It scopes invocation
retry, status and cancellation ownership; a second session acting as the same
profile actor cannot borrow the first session's invocation IDs. Reusing an ID
within its owning session reports the existing operation without dispatching it
again. Reconnecting HTTP with the same valid `Mcp-Session-Id` retains ownership.
Opening a new logical session (or a new stdio connection) receives a new
execution UUID and cannot resume invocation ownership by supplying the old UUID.

Invocations also carry an `agent-run` history group named after the client's
declared `clientInfo.name`. Clients can continue a history group through the
experimental `run` field, but names and history-group IDs are presentation
metadata, not authenticated execution identity. An `execution_id` supplied by a
client is ignored. The returned UUID is a correlation identifier, not a bearer
credential, device identity or capability grant. It does not publish a cursor or
prove that an action changed visible state.

Sessions retain the profile actor's authority. The token authenticates the
fronting proxy only; that proxy owns end-user authentication and must route each
user to their machine. Ending an MCP session removes access to that session; it
does not assert that its already-dispatched operations were cancelled.

## Pixel fallback

Beside a compositor, `TILO_RUNTIME_COMPUTER_CONTROL=/abs/path/to/control.sock`
(the compositor's `TILO_TEST_CONTROL` socket) adds the canonical `computer` and
`computer_batch` tools that hosted computer-use providers and model vendors
share: `screenshot`, the click family, `mouse_move`, `left_click_drag`,
`left_mouse_down`/`up`, `scroll`, `type`, `key` and `hold_key` (xdotool names
such as `ctrl+s`), `wait`, `zoom` and `cursor_position`. They exist for
windows that publish no typed action; tool descriptions steer models to the
typed tools first. Such legacy applications (a browser, an office suite) are
started like any other: `tilo_query` with `workspace: true` lists each
installed desktop application as `app.host:<desktop-id>` (kind
`applications.host-app`), and its `open` action launches it unconfined into
the workspace.

Each action returns its post-action screenshot (JPEG by default, `format: png`
on request) and a text block with `frame_id`, `width` and `height`.
Coordinates are pixels in the last screenshot's frame; screens larger than
1280×800 are scaled down and coordinates scaled back. A batch returns frames
only for `screenshot` steps and its last step unless `no_screenshot` says
otherwise, and stops at the first failing step.

## Explicit manifest mode

Choose a short absolute profile path and create an app manifest at an absolute
path. Unix socket pathname limits also apply to the profile’s `collab.sock` and
`identity.sock`; long profile paths fail at startup. For example, use a private
`/tmp/tilo-mcp-...` directory for temporary runs. The executable paths must also
be absolute. The list may be empty, and repeated
app ids intentionally create separate live instances:

```json
{
  "apps": [
    {
      "id": "dev.tilo.photos",
      "executable": "/absolute/path/to/photos",
      "args": ["--open", "/absolute/path/to/source.png"]
    },
    {
      "id": "dev.tilo.word-processor",
      "executable": "/absolute/path/to/word-processor",
      "args": []
    }
  ]
}
```

The original explicit launcher remains available for controlled workflows:

```json
{
  "mcpServers": {
    "tilo-runtime": {
      "command": "/absolute/path/to/tilo-runtime-mcp",
      "args": [
        "--profile", "/absolute/path/to/persistent-profile",
        "--apps", "/absolute/path/to/apps.json"
      ]
    }
  }
}
```

Manifest mode owns the processes it launches. EOF, output failure, app launch
failure, or an unexpected child exit ends that manifest session and reaps only
its children. App stdin is null and app stdout/stderr go to instance-indexed
logs, so only JSON-RPC replies appear on MCP stdout. Request lines are limited
to 1 MiB and the pending input queue is bounded.

## Discovery and invocation

The endpoint implements `initialize`, `ping`, `tools/list`, `tools/call`,
`resources/list`, and `resources/read`. Its generic tools list live app
instances, browse a bounded lazy resource/action directory, resolve an exact
current action, start it, inspect its canonical status, and request
cancellation. Starting, status and cancellation go through the session's
action host — the same `tilo_actions::remote` door apps, agents and peers use —
so an invocation is identified by a UUID `invocation_id`. Pass your own
`invocation_id` to `tilo_invoke` to make a retry safe: repeating the same id and
request reports the original invocation instead of starting it again, and
reusing an id for a different request is refused. Call `tilo_list_apps`, then `tilo_query`; keep the returned peer
id (or name the app by `app_id`: its newest instance), resource and action for
`tilo_resolve` or `tilo_invoke`. Browse roots with an empty `parent_resource`,
then use a returned resource as `parent_resource` to discover its actions and
children. Follow `next_cursor` with the same revision and query. `needle`
finds actions and resources by name; several words must each begin a word of
the row's names, and each of up to three resources a needle finds is listed
with all its actions. When no row has every word, a compact page answers with
the rows that have the most of them and says `"partial_match": true`, so a
needle naming several actions ("open paragraph style heading") still finds
`set-paragraph-style`; version 1 rows keep the every-word match. `revision` is
optional on `tilo_resolve` and `tilo_invoke`: omitted, the target resolves
against the directory as it is now (an invoke passes no revision on, and the
app resolves it in the same turn the broker admits it, so nothing reads the
revision first); named, a target that changed since is refused. Either way the
admission is bound to a revision, the named one or the one the app resolved
at, and the journal records it: a host that runs the action later refuses it
once the app's directory moved on before it ran. An action whose contract asks
for confirmation needs the revision it is confirmed at, so for one of those
the bridge reads the current revision once and invokes at it; an exact-phrase
contract is answered with the phrase read at that revision, and the retry
names it.
`tilo_invoke` waits up to `wait_ms` (default 5 s) for the terminal update and
answers with it, woken by the broker when the invocation changes rather than
on a timer; an invocation still running then says so in `pending` and is
followed with `tilo_status`. A session's requests may overlap: its
`tilo_status` and `tilo_cancel` answer while one of its `tilo_invoke` calls
waits, and calls naming the same `invocation_id` run one after the other. One call is `resource`,
`action` and `arguments`. Several that share a peer go in `calls`,
`[{resource, action, arguments}, …]` (at most 32), and run in order: the
first keeps the named revision, each later one reads the directory as it
is then, and the first that does not succeed stops the rest. The answer is
`results`, plus `stopped` at that call's index. The same `invocation_id`
never runs the batch twice: the first step uses it, and each later step's
id is derived from it. One call is one invocation: the bridge never runs
it again under another id. A number outside the range its action declares
(`pageSize: 200` where the signature says `1..64`) fails validation before
the action runs, and the failure carries the bound as data
(`update.error.details`: `{"param": "pageSize", "violation": {"kind":
"maximum", "bound": 64}}`) beside its message; the answer's `hint` names it
(`"pageSize must be at most 64; call again with a value in range, such as
64"`) and the caller decides whether to call again. The bridge decides by the
host's typed refusal, never its wording: a target not offered, an action
asking for confirmation and a stale revision (`stale_revision`) are each a
variant of their own.
Pass content resource references returned by one successful action as typed
arguments to a freshly discovered action in another app. The runtime contains
no Photos/Writer workflow branch and never treats `Accepted` as `Succeeded`.
A failed invocation is not replayed merely because a text argument matches a
session handle: the answer's `hint` names the handle and the id it stands
for, for the caller to pass if it meant the resource.

### Opening an app

`tilo_invoke {workspace: true, resource: "app.<app_id>", action: "open"}`
launches an installed app and answers, once the new instance has published its
directory, with its `peer_id` and its `directory`: the compact page of its
top-level resources with all their actions, and the resources beneath them
while the page stays within 12 KiB (`"truncated": true` when something was left
for `tilo_query`). An action line there is its label and parameters
(`"save-as": "Save document as (path?)"`); the description sentence is left
to `tilo_query`, because this reply is read again on every later turn. An
agent acts on the reply without browsing first. An app
may publish again while it starts, and a directory read in between lists
nothing, so the reply waits up to 3 s more for one that lists a resource.

`arguments: {"path": "/abs/file"}` also opens that file. The workspace's own
launch opens content only by hash, read through an authenticated app's scope,
and a launch argument would bypass the instance's typed surface, so the bridge
launches the app empty and then invokes the action the instance declares as
opening paths (`ActionSpec::opens_path`, `#[effect(opens_path = "csv")]`;
tagged `open:path`, with `open:path-ext:<extension>` for the types it is
declared for), preferring one declared for the file's extension, as Sheets
declares its imports. An action is never chosen by its name: one that only
takes a `path`, such as Windows' ISO import, is not an opener. The app reads the file inside its sandbox, so only files the workspace
granted it open; nothing is granted on the way. The reply's `opened` carries
that invocation's update and the resource and action it used, or an `error`
naming why the file is not open (the app started empty either way). Workspace
pages list `path?` on each application's `open`. Host applications
(`app.host:…`) open without a path.

Once the file is open, the reply's `content` carries the document's first
page, so an agent edits without reading it first: the bridge runs, with no
arguments, the action the app tagged `read:document`
(`ActionSpec::reads_document`) and answers `{resource, action, value}`, the
value being that action's output. That is the Word Processor's `read-text`
(Markdown with block ids, up to 64 blocks, and its `nextCursor`), a sheet's
`read-cells` (its used range, as `rows`, `formulas` and `bold`, plus
`formula_errors` naming each formula that does not parse and why) and the
deck's `read` outline. The File Explorer's `open` itself answers the folder's
`entries` in `opened`. A value over 6 KiB is cut to fit (text at a line,
arrays by whole elements) and says `"truncated": true`; its cursor is
dropped, and the same action answers the page whole. An app without a
tagged action answers no `content`, nor does a file that did not open or an
instance that already held it.

### Reading back what an action did

An agent sees the result of an edit in the edit's reply and reads a
document as text, so it needs no screenshot to check its work. A document
action whose contract declares a JSON result gets one: after the edit
applies, ui-core diffs the document from the edit's base heads and asks the
app through `PublishesResources::document_action_result`.

- **Word Processor.** `read-text` answers Markdown, one block per line ending
  in its `{#block id}`: headings as `#`…`######`, list items as `- ` or `1. `
  indented two spaces per level, and a table as a Markdown table tagged
  `{#table id}` whose every cell, empty or not, ends in its own `{#block id}`:

  ```text
  ## Standup {#b1}
  - review {#b2}
    1. screen reader {#b3}
  | Person {#c1} | Blocked {#c2} | Next {#c3} | {#table t1}
  | --- | --- | --- |
  | Maya {#c4} | no {#c5} | {#c6} |
  ```

  A block id is what `select-block`, `select-range` and `move-caret` take,
  so filling a cell is `select-block {block: "c6"}` then `type-text`; the
  table's `word-processor.table:<table id>` resource adds and deletes rows
  and columns. It pages up to 64 blocks through `nextCursor`;
  `format: "segments"` returns the typed segments with their text offsets.
  `paste` (Markdown, including headings, lists and pipe tables),
  `set-paragraph-style`, `apply-named-style`, `insert-table` and a table's
  `append-row` and `append-column` answer with the blocks they changed,
  rendered the same way (`2 changed:` …, at most 32 listed, each cut at 160
  characters with `…`, then `N removed`).
- **Slides.** The deck's `read` answers the deck as text: each slide's
  number, layout and `{#id}`, its title as a `# ` line, its body and other
  elements each ending in its `{#id}`, then its speaker notes. `add-slide`,
  `set-slide-text`, `move-slide` and the element edits answer with the same
  outline. `set-slide-text {slide, title?, body?}` replaces a slide's title
  (first text box) and body (second, a paragraph per line); `slide` is its
  id or its number as `read` shows it.
- **File Explorer.** `move {entries, folder, create?}` moves several entries
  of the current folder into `folder` in one call, creating it unless
  `create` is false, and answers `moved N into <folder>: …`. Nothing moves
  when an entry is not there; the error lists the folder's entries.
  `move-to` into a missing folder names the `move` call that creates it.

Only these actions declare a result: each open document publishes its
catalogue to the broker, and the broker's catalogue reply is capped at
256 KiB (`MAX_PRIVATE_BYTES`), which five open Word Processor documents
already nearly fill.

### Page format

`tilo_query` pages are compact (surface version 3, reported at `initialize`
as `capabilities.experimental["so.tilo.mcp"].version`): each resource once,
its actions as one-line signatures such as `"save-as": "Save document as
(path?)"` (`p?` optional, `p:type` a non-text type, `a|b` the accepted values,
`1..64` a numeric range), and string cursors. In an app instance's pages, a
resource id longer than 24 characters is shown as a session handle such as
`r3`; workspace pages keep their ids (`app.<app_id>`, the permission roles),
which provisioning code and prompts spell out. `tilo_query`'s
`parent_resource`, `tilo_resolve` and `tilo_invoke` accept a handle wherever
they take a resource, and full ids and a resource's kind or label
(`canvas.artwork`) keep working. A `parent_resource` that lists nothing and
names no resource is refused with the resources there are, rather than
answered with an empty page. A compact session's `tilo_resolve` names the
resource by its handle and says each parameter schema once: an optional
property does not also list `null` (absent and `null` read alike), a `const`
carries no `type`, bounds that only restate an integer type's range are left
out, and a subschema repeated under several properties is one `$defs` entry
each place `$ref`s, keeping its own description. In `tilo_invoke`'s
`arguments`, a handle this session issued is replaced by its id where the
action declares a `resource` parameter. Where the parameter is text, the handle
goes as given, since it may be meant literally. If the invocation fails, the
original failure and invocation id are returned without rewriting that text or
running it again: the first call may already have applied a mutation. An admission
refusal remains an MCP tool error; an admitted execution failure remains its
structured `update`. A bound UI resource's id is
about 130 characters, repeated in every call. A model reads every page again
on each later turn, and version 1's rows repeated each resource id three times
and each contract twice. `detail: "full"` returns version 1 rows, with full
ids, for one call; a client that parses them sends `{"so.tilo.mcp": {"detail":
"full"}}` in `capabilities.experimental` at `initialize` to keep them for its
session, and its `open` replies carry no directory. `resources/read` always
returns full rows.

A normal `ui::App` automatically obtains collaboration and identity
connections from the shared runtime and publishes its current lazy directory.
Existing Tilo or legacy inherited connections remain authoritative and are not
replaced by another profile. Complete malformed inherited transfers fail
closed. Diagnostic, audit, shot, headless, and handled-command launches remain
detached, and an app may explicitly opt a private/local mode out before live
construction.

## Authority and scope

This is a local, same-user native host. The profile's workspace and actor are
fixed when the host starts; MCP requests cannot select an actor, workspace, or
executable. Discovery and invocation retain the production broker's workspace
authority and current-instance checks; the Tilo permission system decides what
is visible and invokable. An agent never confirms by its own word: an action
that asks for confirmation, or that the agent's role answers with Ask (an
action reaching outside, such as a browser page action), is put to the person
on this device, and runs once they allow it. On macOS the standalone runtime
asks in a native prompt, for that one invocation; where nobody can be asked,
the action is refused. Risk levels travel in each action's contract, and
`tilo_invoke` is annotated as potentially destructive so clients can prompt.

The concrete launcher uses Unix sockets, process execution, and descriptor
inheritance and is currently qualified for macOS and Linux. Windows host
transport and inheritance remain unsupported. The repository-wide source audit
found that all 47 inventoried GUI entry points reach the shared production
`ui::App` publication path, including custom live handlers. That source audit
is not native execution proof for all 47 apps. The headless Agent is a consumer
rather than a GUI publisher.

Building or running this binary does not configure an MCP client, change an
account, enable a cloud relay, or add remote federation. Assistant setup remains
explicit. `TILO_RUNTIME_STANDALONE=1` is a deliberate test/diagnostic opt-out
when no inherited Tilo connection exists; it is not the ordinary default.

## Native acceptance proof

Capture the host inputs, build the host from those inputs, then run the
ordinary-launch proof in that order. Do not rebuild or edit the captured source
between these commands:

```sh
python3 scripts/runtime-auto-attach-proof.py \
  --capture-host-source-snapshot /absolute/path/to/host-build-source-hashes.json
cargo build -p shell --bin tilo-runtime-mcp --no-default-features
python3 scripts/runtime-auto-attach-proof.py \
  --host-source-snapshot /absolute/path/to/host-build-source-hashes.json \
  --output /absolute/path/to/new-evidence
```

The runner opens Writer, Files, and Photos with literal ordinary `cargo run`
commands, waits for the first app to start the daemon, and only then opens an
explicit `--connect` proxy. Calculator starts late through its own ordinary
`cargo run`, demonstrating generic joining rather than a three-app allowlist.
The runner requires four distinct peer registrations, invokes Calculator's
declared `add-function` through MCP, and polls its canonical status to
`succeeded`. It then closes Writer and requires Files, Photos, Calculator, the
daemon, and MCP client to remain live.

Evidence includes exact commands, relevant source hashes before and after the
run, executable hashes, actual app/runtime/MCP PIDs, the complete MCP transcript,
terminal action output, app logs, and exact-PID cleanup. App data and runtime
authority use separate isolated directories. `proof.json` appears only after
the proxy exits cleanly, every known PID is absent, and the temporary runtime
profile has been removed. `cleanup.json` records profile-removal success or its
exact error; failed attempts retain `failure.txt` instead.

Verified on macOS on 2026-09-17 at
`target/runtime-proof/runtime-auto-attach/native-3/proof.json`: Writer, Files,
Photos, and late Calculator registered as peers 1–4 in daemon PID 28409;
Calculator invocation 1 reached `succeeded`; after Writer PID 28361 exited, the
other three registrations, daemon, and MCP PID 28548 remained live. Cleanup
confirmed all six known PIDs absent. The relevant source snapshot was unchanged
and exact binary hashes are retained with the run. This qualifies native shared
runtime behavior, not visible frames, performance, Photos History acceptance,
all 47 apps, or Windows.
