# Invocations and history

Nothing in a workspace changes anonymously. Each action call is an **invocation**; calls may be grouped
under one id, every change is a step in its record's **history**, an invocation can be taken back as one
piece, and the workspace lists its **invocations** to anyone who may read the records they changed.

## An invocation

An agent's connection invokes under a name taken from the key it holds, so a workspace with three customer
agents shows three invocations, each with its own name and colour. Your code's calls are invocations too,
named after their key.

An invocation can also be named by you. Everything you do inside `ws.invocation(title)` is one invocation,
and each app records the invocation's changes to it as **one step**. So an invocation of thirty calls that
filled a form and wrote a letter is two steps: one in the PDF, one in Writer. The example under
[Undo](#undo) opens an invocation, reads the history it left, and takes it back.

## History

Each record keeps its own history: what changed, in words, who did it, when, and which group it was part
of. The workspace's Activity window shows it, and its changes are the steps an invocation can take back:
its listing says how many stand.

```text
tilo.framework/1 genesis
Added a paragraph            Write the cover letter
Undid earlier steps
```

Changes come newest-first, and one an agent made names the actor: its kind (`agent`), the name of its
key, the colour it shows in, and the person it acted for.

## Undo

An undo is the app's own action, so it is recorded like anything else and is itself undoable:

- **An invocation**: `invocation.undo()` (`POST /v1/workspaces/{workspace}/invocations/{invocation}/undo`)
  takes back exactly what the invocation's calls changed, in every resource they changed. Any key that may
  undo there can take it back, from any connection. A call's id names it while the server remembers the
  call; after the server restarts, the invocation's `step` from the listing names it. When someone else's
  change stands on top of the invocation's, it is refused and nothing is taken back, naming whose change
  it is.
- **One step**: `undo` with `steps: 1` on the record, which takes back its last change, whoever made it.
- **A person**: Undo in the record's history, which is those same actions.

```python example=invocation-and-undo
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo,
            {"path": "account/workspaces", "action": "create"},
            {"name": "Invocations and history"},
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])

        # One invocation: every call inside it is one step of this app's history.
        async with workspace.invocation("Write the cover letter") as group:
            await run(
                workspace,
                {"path": "word-processor", "action": "append_paragraph"},
                {"text": "Dear Visa Officer,"},
            )
            await run(
                workspace,
                {"path": "word-processor", "action": "append_paragraph"},
                {"text": "Yours faithfully, Jane Doe"},
            )

        print("one invocation:", group.title, "|", len(group.invocations), "calls")

        # Take the invocation back: exactly what its calls changed.
        await group.undo()
        print("undone:", group.title)


asyncio.run(main())
```

```typescript example=invocation-and-undo
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Invocations and history" },
);
const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);

// One invocation: every call inside it is one step of this app's history.
const group = await workspace.invocation("Write the cover letter", async (group) => {
  await run(
    workspace,
    { path: "word-processor", action: "append_paragraph" },
    { text: "Dear Visa Officer," },
  );
  await run(
    workspace,
    { path: "word-processor", action: "append_paragraph" },
    { text: "Yours faithfully, Jane Doe" },
  );
  return group;
});

console.log("one invocation:", group.title, "|", group.invocations.length, "calls");

// Take the invocation back: exactly what its calls changed.
await group.undo();
console.log("undone:", group.title);
```

```curl example=invocation-and-undo
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
json="Content-Type: application/json"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "$json" \
  -d '{"name": "Invocations and history"}' | jq -r .result.workspace.id)
word=$api/workspaces/$workspace/resources/word-processor/actions

# One invocation: every call naming it in Tilo-Invocation is one step of this app's history.
group=(-H "Tilo-Invocation: cover-letter" -H "Tilo-Invocation-Title: Write the cover letter")
for text in "Dear Visa Officer," "Yours faithfully, Jane Doe"; do
  invocation=$(curl -sS --fail-with-body "$word/append_paragraph" -H "$auth" -H "$json" "${group[@]}" \
    -d "{\"text\": \"$text\"}" | jq -r .invocation)
done

# Take the invocation back, by any of its calls: exactly what it changed.
curl -sS --fail-with-body -X POST "$api/workspaces/$workspace/invocations/$invocation/undo" -H "$auth" \
  | jq -r '"status: \(.status)"'
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/record%3Atilo.invocations%3A" -H "$auth" \
  | jq -r '.fields.items[] | select(.title == "Write the cover letter")
      | "undone: \(.undone) of \(.changes) changes"'
```

## Every invocation you may read

A workspace lists its invocations, newest first, whoever made them (every key of yours, every agent's key
made for it, and you), as far as the key asking may read the records they changed. Several agents each
working in a workspace of their own leave one list per workspace of what was invoked, what waits for you,
and what can still be taken back.

The invocations are a record of the workspace, `record:tilo.invocations:`, read whole through the
resource API (`ws.readResource({ path: "record:tilo.invocations:" })`; `GET
/v1/workspaces/{workspace}/resources/record:tilo.invocations:`; over MCP `tilo_read
record:tilo.invocations:`; `tilo invocations -w <workspace>` pages it). A reader
pages it itself, 50 unless it says (at most 200), and the record also answers one invocation by its
id, `record:tilo.invocations:<id>`. Each says:

- **who made it**: the name of its key or agent, and its kind (`agent` for every key; `person`,
  `bot` or `automation` otherwise), and the title its client gave it with `ws.invocation(title)`;
- **what it was made from**: `origin`, the resource its call named with `$from` (else the
  conversation its turn belongs to), and `retries`, the invocation a retry runs again;
- **what a model call it made cost**: `spend`, the provider and model that answered, the tokens it
  counted and the millionths of the currency it spent;
- **how it stands**: `waiting` for you to allow it, `running`, `ended`, `failed` (with a typed
  `failure`, `confirmation_required` when you did not allow it), `cancelled`, or `undone` once
  everything it changed was taken back;
- **when**: `started_ms`, and `ended_ms` once it ended;
- **what it touched**: how many `changes` it made and to which `records`, how many were `undone`,
  and the `summary` of its newest change, without the values it carried (the action by its
  published label);
- **whether it can still be taken back**: `undoable`, or `undo_blocked` naming whose change stands
  on top of its own.

An invocation whose change a resource keeps never reads `running`.

The page also says the `total`, and how many are `waiting` for you.

An invocation's `id` is the id the other invocation calls take (`ws.status(id)`, its cancel and undo):
the newest call of it. Which invocations are listed, and how, holds across restarts of the host as far as
the workspace's records keep them. One that changed a record is kept in that record's history, so it is
listed the same after the host restarts (`recorded`), only without an `id`: the host remembers an
invocation's calls (`remembered`) until it restarts, or lets go of the oldest among many. Its `step`, the
undo step its changes carry in every record's History, names it either way. One that changed nothing
(still waiting for you, or failed before it wrote) is known from the host's memory alone, and is not
listed after a restart. `ws.status(id)` stays `pending` until the change is saved, a moment after the
listing may already read it ended.

A key reads the invocations in records it may read now, as it reads those records' History: of one that
also changed a record it may not read, only the part in the records it may; one with nothing in them is
not listed, and `total` and `waiting` count only what is listed. A call that changed nothing is listed to
a key that may read the record it asked about. A Read taken away shows in the next listing. What an
invocation was given, what it answered and the words of its failure stay with whoever made it.

```python example=list-invocations
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo, {"path": "account/workspaces", "action": "create"}, {"name": "Every invocation"}
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])

        async with workspace.invocation("Write the cover letter"):
            await run(
                workspace,
                {"path": "word-processor", "action": "append_paragraph"},
                {"text": "Dear Visa Officer,"},
            )

        # The workspace keeps its invocations as a record; its items are newest first.
        record = await workspace.read_resource("record:tilo.invocations:")
        fields = record.get("fields") or {}
        items = fields.get("items") or []
        print(fields.get("total", 0), "invocations;", fields.get("waiting", 0), "waiting for you")
        for item in items:
            print(
                " ",
                item.get("status"),
                "by",
                (item.get("by") or {}).get("name"),
                "|",
                item.get("title"),
                "|",
                item.get("changes"),
                "changes",
            )

        letter = next(item for item in items if item.get("title") == "Write the cover letter")
        assert letter.get("status") == "ended" and letter.get("undoable"), letter
        print("found:", letter.get("id"), (letter.get("records") or [{}])[0].get("app"))


asyncio.run(main())
```

```typescript example=list-invocations
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Every invocation" },
);
const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);

await workspace.invocation("Write the cover letter", async () => {
  await run(
    workspace,
    { path: "word-processor", action: "append_paragraph" },
    { text: "Dear Visa Officer," },
  );
});

// The workspace keeps its invocations as a record; its items are newest first.
type Listed = {
  status: string;
  title?: string;
  by?: { name?: string };
  changes?: number;
  id?: string;
  undoable?: boolean;
  records?: { app?: string }[];
};
const record = await workspace.readResource({ path: "record:tilo.invocations:" });
const fields = (record.fields ?? {}) as { total?: number; waiting?: number; items?: Listed[] };
const items = fields.items ?? [];
console.log(fields.total ?? 0, "invocations;", fields.waiting ?? 0, "waiting for you");
for (const item of items) {
  console.log(
    " ",
    item.status,
    "by",
    item.by?.name,
    "|",
    item.title,
    "|",
    item.changes ?? 0,
    "changes",
  );
}

const letter = items.find((item) => item.title === "Write the cover letter");
if (letter === undefined || letter.status !== "ended" || !letter.undoable) {
  throw new Error(`the letter's invocation is listed as ended: ${JSON.stringify(letter)}`);
}
console.log("found:", letter.id, letter.records?.[0]?.app);
```

```curl example=list-invocations
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
json="Content-Type: application/json"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "$json" \
  -d '{"name": "Every invocation"}' | jq -r .result.workspace.id)
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/word-processor/actions/append_paragraph" \
  -H "$auth" -H "$json" -H "Tilo-Invocation: cover-letter" -H "Tilo-Invocation-Title: Write the cover letter" \
  -d '{"text": "Dear Visa Officer,"}' > /dev/null

# Newest first: who made each, how it stands and what it changed.
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/record%3Atilo.invocations%3A" \
  -H "$auth" \
  | jq -e -r '.fields.items[] | select(.title == "Write the cover letter")
      | "\(.status) by \(.by.name): \(.changes) changes, undoable: \(.undoable)"'
```

## Links for people

Work ends with a person looking at it, so a call that changed a record answers that record's **link**: the
address a person copies in the app, which opens the record in it. A link carries no secret; it opens for
the workspace's members, so invite the person first.

```text
https://tilo.sh/app/#/@eu1.tilo.sh/w/6f1c…/so.tilo.pdf-viewer/uuid:a65e…
```

A resource's `share-link` action answers the link to anything its path names
(`run(ws, {"path": …, "action": "share-link"})`). An invocation group collects the links to every record
its calls changed, once each, in `group.links`, and `group.link` is the one link when they changed one
record.

## Watching it happen

`watch` answers changes, invocation events and who is there as they arrive ([events](events.md)).

## Where next

- [Approvals](approvals.md): the actions an invocation cannot take without a person.
- [Errors](errors.md): what a refused step raises.
