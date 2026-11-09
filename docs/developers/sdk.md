# Scripting a workspace: SDK, CLI and tilolang

Everything in a Tilo workspace is a resource with fields and actions: a note, a PDF, a
terminal, and the account that owns the workspaces. A person reaches them by clicking, an
agent through MCP, a program in tilolang. The SDKs (Python, TypeScript) and the CLI are
one more way in, over the same resources and the same actions, with the same permissions,
history and undo. There is no second API.

## One model, four ways in

| | Who uses it | What it sends | What comes back |
|---|---|---|---|
| MCP tools (`tilo_ls`, `tilo_read`, `tilo_do`, `tilo_find`, `tilo_status`, `tilo`) | agents | paths and arguments | compact text, priced per token |
| SDK (Python, TypeScript) | your code | the same calls | typed values (JSON on the wire) |
| tilolang (`tilo` tool, `tilo run`) | agents and scripts | programs | values, pasteable as input |
| CLI (`tilo do`, `tilo files`, `tilo account …`) | shells | the same calls | text, or `--json` |

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
tilo sdk gen --lang python -w jane-doe    # writes tilo_types/ (.pyi)
tilo sdk gen --lang ts -w jane-doe        # writes tilo-types.d.ts
```

- One class per resource kind. Fields are typed properties, and read-only fields have no
  setter. Actions are methods with keyword arguments. Action ids map to `snake_case` in
  Python and `camelCase` in TypeScript (`read-form-fields` → `read_form_fields` /
  `readFormFields`).
- Surface types map directly: `String` → `str`, `Option<T>` → `T | None`, `Collection<T>`
  → an iterable collection, `BTreeMap<String, T>` → a mapping, enums → literal unions,
  unions → tagged unions.
- An assignment is a field edit: `form.form_values["surname"] = "Doe"`. A method call is the
  action, the same one a click or an agent runs.
- The package ships types for the built-in apps. `gen` adds the apps installed in that
  workspace, including ones added later. Without generated types, everything still works
  through `ws.do(path, **args)`.

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

## Errors

A refusal is an exception carrying the host's reason and its next steps (`Refused`,
`.next_steps`). An action that asks the person first raises `AskPending` with a link to
the ask. A wrong path or argument names what's valid, as tilolang's errors do.

## Keys

A key is a grant: to the account (list and create workspaces, mint keys) or to one
workspace. It expires and can be revoked; actions taken with it are attributed to it.
`ws.keys.mint(...)` makes a grant narrower than the caller's own, with an expiry no later than
the caller's. Revoking a key ends every key minted from it.

## Wire

The SDK talks to the same endpoints as MCP: `https://mcp.tilo.sh/mcp` for the account and
`https://mcp.tilo.sh/w/<workspace>/mcp` for one workspace. It makes the same calls, gets
structured answers instead of agent text, and has a watch stream on top. Each tool's request
and answer shapes, the `_meta` keys, run grouping and watch are in
[`sdk-wire.md`](sdk-wire.md). File bytes go through content upload, then the Files app's
actions.

## tilolang

A tilolang program runs on the host as a task, against the same catalogue, as the key's
agent. `tilo run visa.tl -w jane-doe` runs a file. The `tilo` MCP tool runs a program sent
by an agent. An assignment compiles to a field edit, a method to the type's action, and one
program is one step for undo.

Open: how one program moves from the account to a workspace it just created. Today a task
lives in one workspace.
