# Scripting a workspace: SDK, CLI and tilolang

Everything in a Tilo workspace is a resource with fields and actions: a note, a PDF, a
terminal, and the account that owns the workspaces. A person reaches them by clicking, an
agent through MCP, a program in tilolang. The SDKs (Python, TypeScript) and the CLI are
one more way in, over the same resources and the same actions, with the same permissions,
history and undo. There is no second API.

Both SDKs are published as `tilo-sdk` (`uv add tilo-sdk`, `bun add tilo-sdk`). The Python module
is `tilo`: plain `tilo` on PyPI is another project's name, so only the distribution is
`tilo-sdk`. A release tag publishes both, at that tag's version ([Publishing the
SDKs](../publishing.md#publishing-the-sdks) has the tag, the credentials and what the native
packages will publish as).

## One model, four ways in

| | Who uses it | What it sends | What comes back |
|---|---|---|---|
| MCP tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status`, `tilo`) | agents | paths and arguments | compact text, priced per token |
| SDK (Python, TypeScript) | your code | the same calls | typed values (JSON on the wire) |
| tilolang (`tilo` tool, `tilo run`) | agents and scripts | programs | values, pasteable as input |
| CLI (`tilo do`, `tilo files`, `tilo account …`) | shells | the API's `/v1` routes | text, or `--json` |

The CLI calls [`/v1`](api.md): `POST
/v1/workspaces/<workspace>/resources/<path>/actions/<action>` for an action,
`POST /v1/workspaces` for one more workspace. It reaches the API through an
API server with `TILO_API_KEY`, or with no server at all: as this computer's
own key, in process (`tilo keys export` writes that key as a file, which
`tilo --key-file` acts with and which holds nothing but the key, its
certificate and the host it reaches).

Every resource has a **path**, `app/resource#action`, as MCP's `tilo_do` takes it:
`pdf-viewer/application.pdf#read-form-fields`. The account is a resource too:
`account/workspaces#create`, `account/keys#mint`. Creating workspaces and minting keys are
ordinary actions, so they appear in the action list, go through the same permission checks,
and land in history like any other action.

## Types come from the workspace's catalogue

Each app publishes its surface: its resource kinds, their fields with types and which are
read-only, and its actions with their parameters (`surface.toml`, written by
`tilo-app surface`, signed with the app). tilolang already compiles programs against this
catalogue, using the loader `tilolang check` uses. The SDK generates its types from the same
catalogue:

```sh
tilo sdk gen --lang python -w jane-doe    # writes tilo_types/<app>.py
tilo sdk gen --lang ts -w jane-doe        # writes tilo-types/<app>.ts
tilo sdk gen --lang python --check        # says whether the checked-in types are current
```

- One module per app, and one class per resource kind in it. Fields are typed properties,
  and a read-only field has no setter (a property in Python, `readonly` in TypeScript).
  Actions are methods. Action ids map to `snake_case` in Python and `camelCase` in
  TypeScript (`read-form-fields` → `read_form_fields` / `readFormFields`); the arguments
  keep the names the wire takes them under, so a call reads like the surface does.
- Surface types map directly: `String` → `str`, `Option<T>` → `T | None`, `Collection<T>`
  → a list, `BTreeMap<String, T>` → a mapping, enums → literal unions, and an enum whose
  cases carry fields → an object tagged by the case, as the app's schema encodes it.
- An assignment is a field edit: `form.form_values["surname"] = "Doe"`. A method call is the
  action, the same one a click or an agent runs.
- A class is the SDK's own resource, so a name it already answers to (a field called `path`,
  an action called `refresh`) is reached through `fields[…]` and `action("…")` instead; the
  generated file says so where it is.
- The package ships the built-in apps' types, and `tilo sdk gen --check` is what the
  `sdk-types` check runs, so a surface change writes them again. `gen -w` writes the apps
  installed in that workspace, including ones added later: each app's `surface.toml` is what
  the workspace's own read of the app answers, so an app that never ran there is not among
  them. Without generated types, everything still works through `ws.do(path, **args)`.
- Python imports an app's module directly (`from tilo.types.pdf_viewer import Document`);
  TypeScript exports the modules as namespaces (`pdfViewer.Document`), because two apps may
  both call a type `Person`. Without `-w`, `gen` writes the apps of the checkout it runs in.

A surface publishes every action of its app that a caller of another language reads: its
record's verbs, and its read effects and conversions (`read-form-fields`, Writer's
`export-content`) as actions of the record too, each with its parameters and the value it
answers with. A read action's return type is part of the surface, so `read_form_fields`
types the form a script reads its fields from; an action that answers nothing or a job has
no return type. What a window shows and does to itself (its zoom, the caret) is no action
of the catalogue.

## Async first

Almost everything in Tilo happens somewhere else: an action runs at its app's host, edits
sync between devices, a Linux command runs in a VM. The SDK is async first.

- **An action is an invocation.** `await form.read_form_fields()` resolves when the action
  answers. An action that takes longer resolves to a `Run`, with `run.output()` (an async
  iterator), `await run.result()`, `await run.cancel()` and `await run.undo()`.
- **Reads are snapshots, and watches are live.** `async for change in doc.watch()` yields
  each change as it syncs, with who made it. The same goes for a collection, a workspace's
  runs, and presence.
- **Concurrency is plain asyncio and Promises.** `asyncio.gather` over many workspaces is
  fine. Nothing is atomic across apps; each action commits at its own app, as in tilolang.
- **There's a sync client for scripts.** Python has `AsyncTilo` and `Tilo`; `Tilo` runs its
  own event loop and has the same surface without `await`. TypeScript is Promise-only.

## Runs, attribution and undo

A client connection is an agent run, named for its key ("visa-backend #3"). People watching
the workspace see it live, with its own name and colour, and it appears in each resource's
history. `async with ws.run("Fill Jane's application") as run:` groups the steps inside into
one run, which a person can undo as a whole, as one tilolang call is one step.

## Links for people

Work ends with a person looking at it, so every call that changed a record answers that
record's link, the one a person copies in the app: `run.links` lists each record a run's calls
changed, once each, and `run.link` is the one link when they changed one record (else
`None`/`null`). A pending call's `Run` has `link` once it finished. `ws.link(path)` answers the
link to anything a path names. A link opens the record in its app for the workspace's members
and carries no secret: invite the person first (`ws.members.invite`).

## Errors

A refusal is an exception carrying the API's reason and its next steps (`Refused`,
`.next_steps`). An action that asks the person first raises `AskPending` with a link to
the ask. A wrong path or argument names what's valid, as tilolang's errors do. A call the API
will not run because the workspace's app publishes another surface than the generated types were
written for raises `SurfaceChanged`, which says to run `tilo sdk gen`.

## Keys

A key is a grant: to the account (list and create workspaces, mint keys) or to one
workspace. It expires and can be revoked; actions taken with it are attributed to it.
`ws.keys.mint(...)` makes a grant narrower than the caller's own, with an expiry no later than
the caller's. Revoking a key ends every key minted from it.

## Wire

The SDKs speak Tilo's `/v1` JSON API, [`api.md`](api.md): an action is
`POST …/resources/{path}/actions/{action}`, a run is followed at `…/runs/{run}`, and a
watch is the workspace's event stream (`…/events`, resumed with `Last-Event-ID`). Each call
carries an `Idempotency-Key`, so a retry runs nothing twice, and a run's calls carry
`Tilo-Run` and `Tilo-Run-Title`. They can also reach a Tilo host in process, through the same
Rust client the API server runs: no second implementation, no socket. File bytes go through
`…/uploads`, then the Files app's actions.

## tilolang

A tilolang program runs on the host as a task, against the same catalogue, as the key's
agent. `tilo run visa.tl -w jane-doe` runs a file. The `tilo` MCP tool runs a program sent
by an agent. An assignment compiles to a field edit, a method to the type's action, and one
program is one step for undo.

Open: how one program moves from the account to a workspace it just created. Today a task
lives in one workspace.
