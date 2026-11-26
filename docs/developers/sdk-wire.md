# MCP's typed answers

An MCP client that asks for typed answers gets the same calls a model makes, answered as
JSON instead of agent text: `https://mcp.tilo.sh/mcp` for the account and
`https://mcp.tilo.sh/w/<workspace>/mcp` for one workspace, with the tools `tilo_ls`,
`tilo_read`, `tilo_do`, `tilo_find`, `tilo_link`, `tilo_status`, `tilo` and `tilo_watch`. The
`tilo` command reads them this way for the account and a workspace; the SDKs are clients of
[the API](api.md) instead, which answers the same values over `/v1`.

There is no second API. Every structured answer is made by the call that answers text, from
the same read of the same app, and names what the text names: the same paths, the same
invocation ids, the same errors.

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
| `invocation` | `{"id": "…", "title": "…"}`: the invocation this call is a step of (see [Invocations](#invocations)). `title` is shown where people read history; `id` groups, so it is stable for the whole group. |
| `step` | What `invocation.id` is shorthand for, when a caller wants grouping without naming one: every call carrying the same `step` is one undo step. Agents that run tilolang programs use it. |

`structured` and `invocation` are read per call; `invocation.id` and `step` never grant
anything. They only say whose changes are taken back together.

## Answers

Every answer whose call failed carries the same failure shape:

```json
{"status": "failed",
 "error": {"code": "invalid_arguments", "message": "…",
           "next_steps": ["tilo_ls notes/Todo#add_text lists its parameters"]}}
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
           "actions": ["toggle-checkbox"]}],
 "kinds": {"notes.Block": [{"id": "toggle-checkbox", "label": "Done",
                            "params": [{"name": "id", "ty": "Text", "required": true}],
                            "kind": "undoable"}]},
 "actions": [ …action rows, for the resource listed… ],
 "next_cursor": "50"}
```

A listing of the apps (`path` omitted) answers `path: "apps"` and rows of
`{"path": "notes", "name": "notes", "label": "Notes", "kind": "app", "state":
"running"|"service"|"installed", "has_children": true, "actions": []}`.

A row names the actions it offers by id; `kinds` describes each kind's actions once, for
the kinds of the rows on the page. `next_cursor` is passed back as `arguments.cursor` for the
rest, and is present exactly while more rows follow. `limit` and `kind` page and
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
 "path": "notes/Todo#add_text",
 "resources": ["notes/Todo"],
 "link": "https://tilo.sh/app/#/@eu1.tilo.sh/w/6f1c…/so.tilo.notes/uuid:3fa2…",
 "edited_by_person": {"text": "Buy milk, buttermilk"}}
```

`path` is the action at the resource it resolved to, as a caller writes it; `result` is the
action's output value as the app typed it (absent when it gave none); `resources` are the
resources the answer says it touched; `edited_by_person` is there when the person changed the
arguments before allowing the action, and holds what actually ran. `link` is there when the
action changed a resource the workspace serves: the resource link a person copies in the app
(`share-link` on the resource's root), which opens the resource in its app for the workspace's
members and carries no secret, so someone else must be invited first. A read answers none. A
failure also carries the `action` row, so a caller can see what it takes.

Several actions (`calls`) answer one step each, in order:

```json
{"status": "failed", "stopped_at": 1,
 "steps": [{"path": "notes/Todo#add_text", "status": "ok", "invocation_id": "…"},
           {"path": "notes/Todo#pin", "status": "failed", "invocation_id": "…",
            "error": {"code": "unknown_action", "message": "…", "next_steps": ["…"]}}]}
```

Steps before the failed one stay applied, as the text says. A batch also answers `links`: the
`link` of every step that has one, each resource once, in the order first changed.

`tilo_status` answers the same shape for one invocation, `link` included once a change it
follows finished, and with `cancel: true` stops it.

### tilo_link

```json
{"status": "ok", "path": "pdf-viewer/application.pdf",
 "link": "https://tilo.sh/app/#/@eu1.tilo.sh/w/6f1c…/so.tilo.pdf-viewer/uuid:a65e…"}
```

The link to whatever `path` names: the resource's own `share-link` when it offers one (a
message, a range of a text), else its resource's. `mode` (`app`, the default, or `desktop`)
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

A query narrows itself with `in:here` (the conversation an agent's turn is in) or `in:` a
conversation's path, and with `after:` and `before:` a day (`2026-11-10`) or a moment
(`2026-11-10T14:00:00Z`). Narrowed, it needs no words and lists the newest first. A hit that
says when it was written carries `when`, in seconds since the Unix epoch. A workspace's
server narrows; a search that cannot (a desktop's) refuses a narrowed query, and never
answers it with everything. A caller is answered only with what it may read.

### tilo

```json
{"status": "ok",
 "output": ["n1 \"Q3 plan\"", "step 1: 2 changes in notes; `undo` takes it back"],
 "changed": 2,
 "steps": [{"number": 1, "apps": ["notes"], "changed": 2,
            "program": "notes.notebook.notes.filter(.pinned)"}]}
```

`output` is the program's answer, one line per element as the text prints it (references and
all); `steps` is this task's part of `history task` — every step of this session's that changed
something, oldest first, with the apps it changed. The `history task` answer's text beside it
lists the agent's runs in the workspace, its `tilo_do` calls and earlier sessions included.
`status` is `ok`, `failed`, or `preview` for a program that would change much or destroy
something and waits for `confirm: true`.

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
   "resources": [{"path": "notes/Todo", "app": "so.tilo.notes"}],
   "sentence": "added \"Buy milk\"", "by": "Jane", "title": "Fill Jane's application",
   "group": "mcp:…", "time_ms": 1730000000000, "undone": false},
  {"kind": "invocation", "phase": "started", "invocation_id": "3f0c…",
   "path": "notes/Todo#add_text", "title": "Fill Jane's application",
   "group": "mcp:…", "time_ms": 1730000000000},
  {"kind": "invocation", "phase": "succeeded", "invocation_id": "3f0c…", "path": "notes/Todo#add_text",
   "title": "Fill Jane's application", "group": "mcp:…", "time_ms": 1730000000001},
  {"kind": "presence", "here": [{"who": "…", "name": "Jane", "kind": "person"},
                                {"who": "agent:…", "name": "Claude Code", "kind": "agent"}]}
 ],
 "cursor": "…"}
```

- **change** — a step in a resource's history. `resources` names what changed: its `app`, the
  `item` inside it when the change names one, and the id the app gives it when the watch is of
  the whole workspace (which names the app's short name in `path`, `notes` for
  `so.tilo.notes`; a watch of one path names that path). `by` names who made it, or `actor`
  when the change keeps one; `sentence` says what it did, and `undone` says it was taken back.
- **invocation** — one of this session's invocations starting or finishing (`started`,
  `succeeded`, `failed`, `cancelled`). `title` is the group's title and `group` its id: the
  same group a set of calls is attributed to.
- **presence** — who is in the workspace, whenever that changes: its people and the agents its
  roster names, as the desktop's people panel lists them. A host that keeps no roster reports
  no presence rather than an empty workspace.
- **unwatched** — the host refused to tell when a `source` (`records` or `presence`) changes,
  and `reason` says why. Said once; the watch then reads that source only when something else
  wakes it, and its wait runs to `wait_ms` rather than to a change of it.

An answer with no events still carries the cursor to ask again from. A caller that got its
cursor from an earlier answer only ever sees each event once. A held watch listens for every
source it reports on, so a change to a resource, an invocation of the session or a person joining
answers it as that happens, without a re-read on a timer. The stream is a long poll, not a push:
one `tools/call` at a time, straight through the same POST path every other call takes.

## Invocations

`async with ws.invocation("Fill Jane's application")` names one invocation:

```json
"_meta": {"so.tilo.mcp": {"structured": true,
                          "invocation": {"id": "fill-jane", "title": "Fill Jane's application"}}}
```

Every call inside it carries the same `invocation.id`. Then:

- **One undo step.** Every change those calls make to one app is recorded as one step of that
  app's history, as one tilolang program is, so the app's `undo` takes the whole group back in
  that app with `{"path": "<resource>#undo", "args": {"steps": 1}}`.
- **One group in history.** History, activity and presence name it `Fill Jane's application`,
  not the connection's own label. `invocation.id` groups within the connection that names it:
  two clients naming the same id never share a step, because the step is derived from the
  connection's own execution and agent session as well as the id.
- **One step for `undo task`.** The `tilo` tool answers `steps` (`history task`) with the
  group's steps and `undo task` takes them back, each app's one step at a time.

A call naming no `invocation` is a step of its own, as it is for an agent.

## Errors the SDK maps

| wire | SDK |
|---|---|
| `status: "failed"`, `error.code`, `error.message` | `Refused(code, message)` |
| `error.next_steps` | `Refused.next_steps` |
| `status: "pending"` with `ask` | `AskPending(ask.url, invocation_id)` |
| `status: "pending"` without `ask` | `Invocation` (follow with `tilo_status` / `tilo_watch`) |
| `isError: true` with no `structuredContent` | `ProtocolError` (the call itself was malformed) |
| HTTP 401 with `{"error": {"code": "key_revoked", "revoked_at_ms"?}}` | `KeyRevoked` (`revoked_at_ms`), a `KeyRefused` |
| HTTP 401 with `{"error": {"code": "key_expired", "expired_at_ms"?}}` | `KeyExpired` (`expired_at_ms`), a `KeyRefused` |
| HTTP 401 with `{"error": {"code": "invalid_token"}}` | `KeyRefused` (a key the API never issued) |

A key the API turns away is answered 401 with `WWW-Authenticate: Bearer …,
error="invalid_token", error_description="<code>: <words>"` and the typed body
above, the same `{code, message, next_steps}` every refusal carries. A
request with no key at all is answered 401 with the challenge alone.
`KeyRefused` is a `ProtocolError` whose `code` is the API's code and whose
`status` is 401.
