# MCP's typed answers

An MCP client that asks for typed answers gets the same calls a model makes, answered as
JSON instead of agent text: `https://mcp.tilo.sh/mcp` for the account and
`https://mcp.tilo.sh/w/<workspace>/mcp` for one workspace, with the tools `tilo_ls`,
`tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status`, `tilo` and `tilo_watch`. The
`tilo` command reads them this way for the account and a workspace; the SDKs are clients of
[the API](api.md) instead, which answers the same values over `/v1`.

There is no second API. Every structured answer is made by the call that answers text, from
the same read of the same app, and names what the text names: the same paths, the same run
ids, the same errors.

## Asking for typed answers

A `tools/call` says so in its `_meta`:

```json
{"jsonrpc": "2.0", "id": 7, "method": "tools/call",
 "params": {"name": "tilo_read", "arguments": {"path": "notes/Todo"},
            "_meta": {"so.tilo.mcp": {"structured": true}}}}
```

With `structured: true` every tool answers `structuredContent` (the typed value below) and
`content: [{"type": "text", "text": "<the same JSON>"}]`. Without it, answers are the agents'
text, byte for byte as before. An answer that carries typed content also carries `isError`
when the call failed, as MCP asks.

The other keys under `so.tilo.mcp`:

| key | meaning |
|---|---|
| `structured` | `true`: typed answers, as this document. |
| `run` | `{"id": "…", "title": "…"}`: the run this call is a step of (see [Runs](#runs)). `title` is shown where people read history; `id` groups, so it is stable for the whole run. |
| `step` | What `run.id` is shorthand for, when a caller wants grouping without naming a run: every call carrying the same `step` is one undo step. Agents that run tilolang programs use it. |

`structured` and `run` are read per call; `run.id` and `step` never grant anything. They only
say whose changes are taken back together.

## Answers

Every answer whose call failed carries the same failure shape:

```json
{"status": "failed",
 "error": {"code": "invalid_arguments", "message": "…",
           "next_steps": ["tilo_ls notes/Todo#append-paragraph lists its parameters"]}}
```

- `code` is the app's own code (`invalid_arguments`, `action_confirmation_required`,
  `unknown_action`, …) or `failed` when the app named none.
- `message` is the app's message, in the words it gives people.
- `next_steps` are the calls that would work, each a sentence naming a path (an empty list is
  left out). The SDK raises them as `Refused.next_steps`.

`status` is one of:

| status | meaning |
|---|---|
| `ok` | The call did what it was asked. |
| `failed` | It did not, and `error` says why. |
| `pending` | It has not finished, or it waits for the person. `invocation_id` follows it (`tilo_status`), `ask` says what the person is being asked. |

### A call that waits for the person

An action that asks its person first — a send, a delete, anything reaching outside the
document they did not pre-allow — is put to the person on their device. The call answers
while they decide, once its `wait_ms` is up:

```json
{"status": "pending", "invocation_id": "…",
 "ask": {"invocation_id": "…"}}
```

The SDK raises `AskPending` carrying that `ask`. Nothing ran yet: the call answered before
the person decided. Call `tilo_do` again with the same `invocation_id` and arguments to put
the ask to them again — the same id never runs the action twice, and once they allow it the
call runs it and answers `ok`; a person who refuses turns it into `status: "failed"`.

`ask.url` is not sent today: it is there for the web page that shows a person the asks put to
them, and no such page exists yet.

### tilo_ls

```json
{"path": "notes/Todo",
 "kind": "notes.Page",
 "rows": [{"path": "notes/Todo/Buy milk", "name": "Buy milk", "label": "Buy milk",
           "kind": "notes.Block", "has_children": false,
           "actions": [{"id": "toggle-checkbox", "label": "Done",
                        "params": [{"name": "id", "ty": "Text", "required": true}],
                        "kind": "undoable"}]}],
 "actions": [ …the same action rows, for the resource listed… ],
 "next_cursor": "50"}
```

A listing of the apps (`path` omitted) answers `path: "apps"` and rows of
`{"path": "notes", "name": "notes", "label": "Notes", "kind": "app", "state":
"running"|"service"|"installed", "has_children": true, "actions": []}`.

`next_cursor` is passed back as `arguments.cursor` for the rest. `limit` and `kind` page and
narrow as they do for agents. `kind` on a row is the app's type, `kind` at the top is the
type of the resource listed (or the app's id).

An action row's `params` are the action's parameters as the app publishes them: `name`, `ty`
(`text`, `integer`, `number`, `boolean`, `object`, `array`, `resource`, `datetime`, `secret`),
`required`, `description`, `choices`, and the JSON `schema` where the app gives one. `kind` is
`read`, `undoable` or `outside`: what running it may reach. A row with children is one whose
path can be listed again (`has_children`); its own actions are listed at its own path, as
`tilo_ls` names them.

### tilo_read

```json
{"path": "notes/Todo", "kind": "notes.Page",
 "fields": {"title": "Todo", "pinned": false},
 "children": [{"list": "blocks", "name": "Buy milk", "item": "c01e",
               "path": "notes/Todo/c01e"}],
 "cursor": "…"}
```

`fields` is the app's own answer with its own types (numbers as numbers, lists as lists);
`children` are the lists of what the resource holds, each with the key a read takes as
`item` and the path that reads it. `cursor` is the app's cursor for the next page.

### tilo_do and tilo_status

One action:

```json
{"status": "ok",
 "invocation_id": "3f0c…",
 "path": "notes/Todo#append-paragraph",
 "result": {"appended": "c01e"},
 "resources": ["notes/Todo"],
 "link": "https://tilo.sh/app/#/@eu1.tilo.sh/w/6f1c…/so.tilo.notes/uuid:3fa2…",
 "edited_by_person": {"text": "Buy milk, buttermilk"}}
```

`path` is the action at the resource it resolved to, as a caller writes it; `result` is the
action's output value as the app typed it (absent when it gave none); `resources` are the
resources the answer says it touched; `edited_by_person` is there when the person changed the
arguments before allowing the action, and holds what actually ran. `link` is there when the
action changed a record the workspace serves: the record link a person copies in the app
(`share-link` on the record's root), which opens the record in its app for the workspace's
members and carries no secret, so someone else must be invited first. A read answers none. A
failure also carries the `action` row, so a caller can see what it takes.

Several actions (`calls`) answer one step each, in order:

```json
{"status": "failed", "stopped_at": 1,
 "steps": [{"path": "notes/Todo#append-paragraph", "status": "ok", "invocation_id": "…",
            "result": {"appended": "c01e"}},
           {"path": "notes/Todo#pin", "status": "failed", "invocation_id": "…",
            "error": {"code": "unknown_action", "message": "…", "next_steps": ["…"]}}]}
```

Steps before the failed one stay applied, as the text says. A batch also answers `links`: the
`link` of every step that has one, each record once, in the order first changed.

`tilo_status` answers the same shape for one invocation, `link` included once a change it
follows finished, and with `cancel: true` stops it.

### tilo_link

```json
{"status": "ok", "path": "pdf-viewer/application.pdf",
 "link": "https://tilo.sh/app/#/@eu1.tilo.sh/w/6f1c…/so.tilo.pdf-viewer/uuid:a65e…"}
```

The link to whatever `path` names: the resource's own `share-link` when it offers one (a
message, a range of a text), else its record's. `mode` (`app`, the default, or `desktop`)
chooses how it opens.

### tilo_find

```json
{"hits": [{"path": "notes/Todo", "kind": "notes.Page", "label": "Todo",
           "action": null}],
 "query": "todo"}
```

`path` is a path the other tools take; `action` names the action the hit's row offers
(`open` when the row opens the resource itself). One hit per resource path, best first, as the
desktop's search ranks them.

### tilo

```json
{"status": "ok",
 "output": ["n1 \"Q3 plan\"", "step 1: 2 changes in notes; `undo` takes it back"],
 "changed": 2,
 "steps": [{"number": 1, "apps": ["notes"], "changed": 2,
            "program": "notes.notebook.notes.filter(.pinned)"}]}
```

`output` is the program's answer, one line per element as the text prints it (references and
all); `steps` is `history task` — every step of this session's that changed something, oldest
first, with the apps it changed. `status` is `ok`, `failed`, or `preview` for a program that
would change much or destroy something and waits for `confirm: true`.

### tilo_watch

```json
{"cursor": "c1a2…"}
```

asks for what happened since `cursor`; without one, since now. `wait_ms` (default 25000, at
most 60000) is how long the call holds until something happened. `path` narrows change events
to one resource and what is under it.

```json
{"events": [
  {"kind": "change", "path": "notes/Todo", "change": "c1a2…",
   "sentence": "added \"Buy milk\"", "by": "Jane", "run": "Fill Jane's application",
   "time_ms": 1730000000000, "undone": false},
  {"kind": "run", "phase": "started", "invocation_id": "3f0c…",
   "path": "notes/Todo#append-paragraph", "run": "Fill Jane's application",
   "run_id": "mcp:…", "time_ms": 1730000000000},
  {"kind": "run", "phase": "succeeded", "invocation_id": "3f0c…", "path": "notes/Todo#append-paragraph",
   "run": "Fill Jane's application", "run_id": "mcp:…", "time_ms": 1730000000001},
  {"kind": "presence", "here": [{"who": "…", "name": "Jane", "kind": "person"},
                                {"who": "agent:…", "name": "Claude Code", "kind": "agent"}]}
 ],
 "cursor": "…"}
```

- **change** — a step in the watched resource's history: who made it (`by`, or the run's
  title), what it did (`sentence`), and whether it has been taken back. A change under a
  watched path is named by its own path.
- **run** — one of this session's invocations starting or finishing (`started`, `succeeded`,
  `failed`, `cancelled`). `run` is the agent run's title and `run_id` its id: the same run a
  group of calls is attributed to.
- **presence** — who is in the workspace, whenever that changes: its people and the agents its
  roster names, as the desktop's people panel lists them. A host that keeps no roster reports
  no presence rather than an empty workspace.

An answer with no events still carries the cursor to ask again from. A caller that got its
cursor from an earlier answer only ever sees each event once. A held watch reads the watched
resource's history about twice a second and the host's runs and roster on every look, so a
change reaches a watching caller within a second of it landing. The stream is a long poll, not a
push: one `tools/call` at a time, straight through the same POST path every other call takes.

## Runs

`async with ws.run("Fill Jane's application")` names one run:

```json
"_meta": {"so.tilo.mcp": {"structured": true,
                          "run": {"id": "fill-jane", "title": "Fill Jane's application"}}}
```

Every call inside it carries the same `run.id`. Then:

- **One undo step.** Every change those calls make to one app is recorded as one step of that
  app's history, as one tilolang program is, so the app's `undo` takes the whole run back in
  that app with `{"path": "<resource>#undo", "args": {"steps": 1}}`.
- **One run in history.** History, activity and presence name it `Fill Jane's application`,
  not the connection's own label. `run.id` groups within the connection that names it: two
  clients naming the same id never share a step, because the step is derived from the
  connection's own execution and agent run as well as the id.
- **One step for `undo task`.** The `tilo` tool answers `steps` (`history task`) with the run's
  steps and `undo task` takes them back, each app's one step at a time.

A call naming no `run` is a step of its own, as it is for an agent.

## Errors the SDK maps

| wire | SDK |
|---|---|
| `status: "failed"`, `error.code`, `error.message` | `Refused(code, message)` |
| `error.next_steps` | `Refused.next_steps` |
| `status: "pending"` with `ask` | `AskPending(ask.url, invocation_id)` |
| `status: "pending"` without `ask` | `Run` (follow with `tilo_status` / `tilo_watch`) |
| `isError: true` with no `structuredContent` | `ProtocolError` (the call itself was malformed) |
| HTTP 401 with `{"error": {"code": "key_revoked", "revoked_at_ms"?}}` | `KeyRevoked` (`revoked_at_ms`), a `KeyRefused` |
| HTTP 401 with `{"error": {"code": "key_expired", "expired_at_ms"?}}` | `KeyExpired` (`expired_at_ms`), a `KeyRefused` |
| HTTP 401 with `{"error": {"code": "invalid_token"}}` | `KeyRefused` (a key the door never issued) |

A key the door turns away is answered 401 with `WWW-Authenticate: Bearer …,
error="invalid_token", error_description="<code>: <words>"` and the typed body
above, the same `{code, message, next_steps}` every door refusal carries. A
request with no key at all is answered 401 with the challenge alone.
`KeyRefused` is a `ProtocolError` whose `code` is the door's code and whose
`status` is 401.
