# Resources and actions

Every app publishes what it holds and what can be done to it, under one shape: **resources** with
**fields**, and **actions** on them. A note is a resource. So is a PDF, a window, a whole notebook, and
your account. Clicking, an agent and your code all take the same path to the same action.

## Paths

A path names a resource and, after `#`, one of its actions:

```text
notes#new_note                          an action on the app's own record
notes/Todo                              a resource: the note called Todo
notes/Todo#retitle                      its rename action
pdf-viewer/application.pdf#read_fields
```

The first segment is the app. What follows is the resource inside it, as the app names it — a title, an
id, a path the app's listing answered. An action hangs off the resource it changes.

## Reading

Two calls read, and they answer the two things a caller needs: what is there, and what is in one thing.

- **`ls`** answers rows: the apps in the workspace, an app's resources, or one resource's children and the
  actions it offers. Each row has a `path` the other calls take.
- **`read`** answers one resource whole: its `kind`, its `fields`, its `children`, and a cursor if it has
  more.

Fields come typed by the app: numbers as numbers, lists as lists, maps as maps. A field the app marks
read-only is published but cannot be set.

## Acting

An action is a typed change. There are three kinds, and the app does not choose them — they follow from
what the action can reach:

| Kind | What it does | Undo |
| :--- | :--- | :--- |
| **Read** | shows or reads: `pdf-viewer/application.pdf#find_in`, Writer's `export_document` | nothing to undo |
| **Undoable** | changes the workspace's own records, nothing outside | by construction |
| **Outside** | reaches outside the records: sends a mail, submits a page, runs a command | asks the person first where it must |

Running one answers what it did, or a refusal with the code, the app's words, and the calls that would
work instead ([errors](errors.md)).

## Field edits

Setting a field is an action too, so a field edit replays, syncs, lands in history and undoes like any
other. It names a path into the resource — a field, then map keys or item ids — an operation and a value:

```text
{ "path": [{"field": "tags"}], "op": "insert", "value": "shopping" }
{ "path": [{"field": "title"}], "op": "set", "value": "Errands" }
```

The operations are the ones the field's type supports: `set`, `replace` and `splice` for text, `insert`
and `remove` for sets and collections, `remove-key` for a map entry, `set-cell` for a grid. Both SDKs wrap
this: assigning a field is an edit, and so is assigning inside a map field.

```python example=read-and-edit
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo,
            {"path": "account/workspaces", "action": "create"},
            {"name": "Snippets and actions"},
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])

        # An action: make a snippet.
        await run(workspace, {"path": "snippets", "action": "new_snippet"}, {"title": "Visa checklist"})

        # A read: the snippet as a resource, with the fields the app publishes.
        snippet = await workspace.resource("snippets/Visa checklist").load()
        print(
            "kind:",
            snippet.kind,
            "title:",
            snippet.fields["title"],
            "favorite:",
            snippet.fields["favorite"],
        )

        # A field edit, twice: assigning a field, and naming the operation.
        snippet.favorite = True
        await snippet.edit("tags", op="insert", value="visa")
        after = await workspace.resource("snippets/Visa checklist").load()
        print("favorite:", after.fields["favorite"], "tags:", after.fields["tags"])


asyncio.run(main())
```

```typescript example=read-and-edit
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Snippets and actions" },
);
const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);

// An action: make a snippet.
await run(workspace, { path: "snippets", action: "new_snippet" }, { title: "Visa checklist" });

// A read: the snippet as a resource, with the fields the app publishes.
const snippet = await workspace.resource("snippets/Visa checklist").load();
console.log("kind:", snippet.kind, "title:", snippet.fields.title, "favorite:", snippet.fields.favorite);

// A field edit, twice: assigning a field, and naming the operation.
snippet.favorite = true;
await snippet.edit(["tags"], "visa", "insert");
const after = await workspace.resource("snippets/Visa checklist").load();
console.log("favorite:", after.fields.favorite, "tags:", after.fields.tags);
```

```curl example=read-and-edit
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Snippets and actions"}' | jq -r .result.workspace.id)
resources=$api/workspaces/$workspace/resources

# An action: make a snippet.
curl -sS --fail-with-body "$resources/snippets/actions/new_snippet" -H "$auth" \
  -H "Content-Type: application/json" -d '{"title": "Visa checklist"}' > /dev/null

# A read: the snippet as a resource, with the fields the app publishes. Each path segment is
# percent-encoded on its own.
snippet=$(curl -sS --fail-with-body "$resources/snippets/Visa%20checklist" -H "$auth")
jq -r '"kind: \(.kind) title: \(.fields.title) favorite: \(.fields.favorite)"' <<<"$snippet"

# Field edits, through the field-edit action every resource has: setting a field, and naming
# the operation.
for edit in '{"path": [{"field": "favorite"}], "op": "set", "value": true}' \
            '{"path": [{"field": "tags"}], "op": "insert", "value": "visa"}'; do
  curl -sS --fail-with-body "$resources/snippets/Visa%20checklist/actions/field-edit" -H "$auth" \
    -H "Content-Type: application/json" -d "{\"edit\": $edit}" > /dev/null
done
curl -sS --fail-with-body "$resources/snippets/Visa%20checklist" -H "$auth" \
  | jq -r '"favorite: \(.fields.favorite) tags: \(.fields.tags)"'
```

## What a write answers

An action that makes something answers what it made: `result.made` holds one entry per item, its `id`
and the `address` it is published under, and the answer repeats the list as `made` at its top. An action
of several answers each item the same way, in the order it made them.

```json
{"status": "ok", "result": {"made": [{"id": "9f2c…", "address": "tasks.tracker/issues/9f2c…"}]},
 "made": [{"id": "9f2c…", "address": "tasks.tracker/issues/9f2c…"}], "resources": ["tasks.tracker/issues/9f2c…"]}
```

The address names the item by its id, so renaming the item neither changes it nor stops it resolving.

## The catalogue

An app publishes its surface once: its resource kinds, every field with its type and whether it can be
set, and every action with its parameters. Reading the app answers it, and it is what tells an agent what
it may call, what tells the app's forms what to draw, and what the SDKs turn into types.

Ask the SDK to write those types for a workspace and you get one module per app, one class per kind:

```sh
tilo sdk gen --lang python -w jane-doe     # writes tilo_types/<app>.py
tilo sdk gen --lang ts -w jane-doe         # writes tilo-types/<app>.ts
```

Then a PDF's document is a `Document`, and every action is a method: `read_fields()` in Python,
`readFields()` in TypeScript — the app's `read_fields`, in each language's spelling. An assignment is a field edit, and a read-only field has
no setter, so the compiler says what the app would have refused.

Without generated types nothing is missing: a path is a string, an action is
`run(ws, {"path": …, "action": …}, args)`, and a resource is `ws.resource(path)`.

## Where next

- [Invocations and history](invocations-and-history.md): one call is one step, and a group of calls can be
  undone whole.
- [Approvals](approvals.md): what happens when an action wants a person first.
