# Scripting a workspace: SDK, CLI and tilolang

Everything in a Tilo workspace is a resource with fields and actions: a note, a PDF, a
terminal, and the account that owns the workspaces. A person reaches them by clicking, an
agent through MCP, a program in tilolang. The SDKs (Python, TypeScript) and the CLI are
one more way in, over the same resources and the same actions, with the same permissions,
history and undo. There is no second API.

Both SDKs are published as `tilo-sdk` (`uv add tilo-sdk`, `bun add tilo-sdk`). The Python module
is `tilo`: plain `tilo` on PyPI is another project's name, so only the distribution is
`tilo-sdk`. A release tag publishes both, at that tag's version.

## One model, four ways in

| | Who uses it | What it sends | What comes back |
|---|---|---|---|
| MCP tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status`, `tilo`) | agents | paths and arguments | compact text, priced per token |
| SDK (Python, TypeScript) | your code and apps | the API's `/v1` requests, through a client generated from its OpenAPI description | typed values |
| tilolang (`tilo` tool, `tilo run`) | agents and scripts | programs | values, pasteable as input |
| CLI (`tilo do`, `tilo ls`, `tilo files`) | shells | the API's `/v1` routes | text, or `--json` |

The CLI calls [`/v1`](api.md): `POST
/v1/workspaces/<workspace>/resources/<path>/actions/<action>` for an action,
`POST /v1/resources/account/workspaces/actions/create` for one more workspace,
`…/actions/branch` for a branch of one. It reaches the API as the person this computer
logged in with (`tilo login`), or for a script, with a key file of its own
(`tilo do account/keys#create name="<what will use it>" --out <file>` asks for a new key,
approved as any key is, and writes it as a file `tilo --key-file` acts with; it never takes
the computer's own key, which stays in one process).

Every resource has a **path**, `app/resource#action`, as MCP's `tilo_do` takes it:
`pdf-viewer/application.pdf#read_fields`. The account is a resource too:
`account/workspaces#create`, `account/workspaces#branch`, `account/keys#mint`. Creating and
branching workspaces and minting keys are
ordinary actions, so they appear in the action list, go through the same permission checks,
and land in history like any other action.

## Types come from the workspace's catalogue

Each app publishes its surface: its resource kinds, their fields with types and which are
read-only, and its actions with their parameters (`surface.toml`, written by
`tilolang surface`, and the canonical `surface.json` compiled from it, which the package ships
and signs). tilolang already compiles programs against this
catalogue, using the loader `tilolang check` uses. The SDK generates its types from the same
catalogue:

```sh
tilo sdk gen --lang python -w jane-doe    # writes tilo_types/<app>.py
tilo sdk gen --lang ts -w jane-doe        # writes tilo-types/<app>.ts
tilo sdk gen --lang python --check        # says whether the types in tilo_types/ are current
```

- One module per app, and one class per resource kind in it. Fields are typed properties,
  and a read-only field has no setter (a property in Python, `readonly` in TypeScript).
  Actions are methods. Action ids map to `snake_case` in Python and `camelCase` in
  TypeScript (`find_in` → `find_in` / `findIn`); the arguments
  keep the names the wire takes them under, so a call reads like the surface does.
- Surface types map directly: `String` → `str`, `Option<T>` → `T | None`, `Collection<T>`
  → a list, `BTreeMap<String, T>` → a mapping, enums → literal unions, and an enum whose
  cases carry fields → an object tagged by the case, as the app's schema encodes it.
- An assignment is a field edit: `form.form_values["surname"] = "Doe"`. A method call is the
  action, the same one a click or an agent runs.
- An undoable action that answers nothing also has a bulk form, `create_many` /
  `createMany`, taking a list of a generated arguments type (`CreateArgs`): see
  [Bulk calls](#bulk-calls).
- A class is the SDK's own resource, so a name it already answers to (a field called `path`,
  an action called `refresh`) is reached through `fields[…]` and `action("…")` instead; the
  generated file says so where it is.
- The package ships the built-in apps' types, written by `tilo sdk gen` when it is built, so
  a release carries its apps' surfaces as they are; the types are never committed. `gen -w`
  writes the apps installed in that workspace, including ones added later: each app's compiled surface is what
  the workspace's own read of the app answers, so an app that never ran there is not among
  them. Without an app's generated types, everything still works through `run(ws, {"path": …, "action": …}, args)`.
- Python imports an app's module directly (`from tilo.types.pdf_viewer import Document`);
  TypeScript exports the modules as namespaces (`pdfViewer.Document`), because two apps may
  both call a type `Person`. Without `-w`, `gen` writes the apps of the checkout it runs in.

A surface publishes every action of its app that a caller of another language reads: its
resource's verbs, and its reads and conversions (PDF Viewer's `find_in`, Writer's `read_content`
and `export_document`) as actions of the resource too, each with its parameters and the value it
answers with. A read action's return type is part of the surface, so `find_in` types what a
search finds; an action that answers nothing or a job has no return type. What a window shows and
does to itself (its zoom, the caret) is no action of the catalogue.

## Async first

Almost everything in Tilo happens somewhere else: an action runs at its app's host, edits
sync between devices, a Linux command runs in a VM. The SDK is async first.

- **An action is an invocation.** `await ws.run_action({"path", "action"}, args)` is one request;
  `run(ws, {"path", "action"}, args)` follows a pending one until it answers. An invocation is
  read with `ws.get_invocation(invocation, wait=…)`, its events with
  `ws.invocation_output(invocation)`, and stopped or taken back with
  `ws.cancel_invocation(invocation)` and `ws.undo_invocation(invocation)`.
- **Reads are snapshots, and watches are live.** `async for change in doc.watch()` yields
  each change as it syncs, with who made it. The same goes for a collection, a workspace's
  runs, and presence.
- **Concurrency is plain asyncio and Promises.** `asyncio.gather` over many workspaces is
  fine. Nothing is atomic across apps; each action commits at its own app, as in tilolang.
- **Both clients are async.** Python's methods are coroutines and TypeScript's are Promises;
  neither runs an event loop of its own.

## Runs, attribution and undo

A client connection is an agent session, named for its key ("visa-backend #3"). People watching
the workspace see it live, with its own name and colour, and it appears in each resource's
history. `async with ws.invocation("Fill Jane's application") as invocation:` groups the steps
inside into one invocation, which a person can undo as a whole, as one tilolang call is one
step.

## Bulk calls

A resource publishes a `group` action: several of its other actions, applied in order as one
all-or-nothing step. The SDK sends it whenever a script fills a resource from a list. The
generated bulk method takes a list of the action's arguments, each typed (`CreateArgs`):

```python
tracker = await Tracker(ws, "tasks/Tracker").load()
answers = await tracker.create_many([
    {"title": "Ship the backlog", "status": "todo"},
    {"title": "Write the migration", "status": "todo"},
])
```

```ts
const tracker = tasks.tracker(ws, "tasks/Tracker");
const answers = await tracker.createMany([
  { title: "Ship the backlog", status: "todo" },
  { title: "Write the migration", status: "todo" },
]);
```

`res.many("create", items)` is the same call without generated types. A list of 1,200 items
is three calls: at most 500 items and about 4 MiB of JSON each, and a member is never
split. Each call is one step, so if one of its members refuses, nothing of that call
applies; the calls before it stand, because they were steps of their own. All the calls of
one bulk call carry one `Tilo-Invocation`, so History lists the work once and one undo takes
it back; inside `ws.invocation(...)` they join that group instead. A bulk call answers each call's
answer, in order, and an empty list sends nothing.

A read, or an action that answers a value (the resource it made, a file it exported), has no
bulk form: `group` reports no member's answer, so the value would be lost.

## Links for people

Work ends with a person looking at it, so every call that changed a resource answers that
resource's link, the one a person copies in the app: a group's `links` lists each resource its
calls changed, once each, and `link` is the one link when they changed one resource (else
`None`/`null`). An invocation's answer carries `link` once it finished. A resource's `share-link`
action answers the link to anything its path names. A link opens the resource in its app for the
workspace's members and carries no secret: invite the person first
(`run(ws, {"path": "workspace/invitations", "action": "invite"}, {"role": "editor"})`).

## Errors

A refusal is `Refused`, carrying the API's reason: `code` is what a program branches on
(`not_permitted`, `key_revoked`, `surface_changed`, …), `message` its words for people,
`next_steps` the calls that would work, and `status` the HTTP status. An action that asks the
person first answers `pending` with the `ask` it waits on; `run` follows it until they decide. A
call the SDK will not run because the workspace's app publishes another surface than the
generated types were written for is refused `surface_changed`, which says to run `tilo sdk gen`.

## Keys

A key is a grant: to the account (list and create workspaces, mint keys) or to one
workspace. It expires and can be revoked; actions taken with it are attributed to it.
`run(ws, {"path": "account/keys", "action": "mint"}, {"workspace": …})` makes a grant narrower
than the caller's own, with an expiry no later than the caller's. Revoking a key ends every key
minted from it.

## Wire

A program on a computer that runs Tilo connects to that computer's daemon, the way `tilo`'s
workspace commands do, on the daemon's native endpoint. The SDK's native part (`tilo._native`, the
npm package `tilo-sdk-native`) is the line wire's one Rust client. Both SDKs send the API's own
requests on it (`/v1`, as [api.md](api.md) describes them), each one frame with its body after it,
and the daemon answers each with its in-process API, its body in pieces, as a stream of events goes
on. The daemon admits the connection as whoever makes it: the person whose program it is, or an app
the daemon started, which the launcher hands an open connection (`TILO_SVC_NATIVE_FD`) and its
workspace (`TILO_WORKSPACE_ID`). An app's requests are decided by its own grants, in that one
workspace, exactly as its action frames are; none reaches its person's account.

A program with no daemon beside it uses a key instead (`TILO_API_KEY`) and sends the same `/v1`
requests over HTTP to the API server (`TILO_API_URL`, `https://api.tilo.sh` by default). Every
action is invoked under an id of its own, so a call asked again runs nothing twice, and a group's
calls name the group they are steps of.

A secret never leaves the vault: a program signs in somewhere with a vault item through the
vault's `web-call` (`ws.vault.web_call` / `ws.vault.webCall`), which adds the item on the way out
to the service it belongs to and answers the response with it struck out, and comes by an item
through `ask` (`ws.vault.ask`), which the person answers on a device of theirs.

## tilolang

A tilolang program runs on the host as a task, against the same catalogue, as the key's
agent: the apps the workspace lists, compiled against the surfaces the server ships. An app
whose surface the server does not know is one a program cannot name. `tilo run visa.tl -w jane-doe` runs a file. The `tilo` MCP tool runs a program sent
by an agent. An assignment compiles to a field edit, a method to the type's action, and one
program is one step for undo.

Open: how one program moves from the account to a workspace it just created. Today a task
lives in one workspace.
