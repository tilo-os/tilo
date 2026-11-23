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
of. An app publishes it as the `history` action on the record, so every way in can read it.

```text
tilo.framework/1 genesis
Added a paragraph            Write the cover letter
Undid earlier steps
```

Versions come newest-last, `last` says how many. `undone` marks the ones taken back. For an agent's
change, the version names the actor: its kind (`agent`), the name of its key, the colour it shows in, and
the person it acted for.

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
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Invocations and history").id)

# One invocation: every call inside it is one step of this app's history.
with workspace.invocation("Write the cover letter") as invocation:
    workspace.do("word-processor#append-paragraph", text="Dear Visa Officer,")
    workspace.do("word-processor#append-paragraph", text="Yours faithfully, Jane Doe")

history = workspace.do("word-processor#history", last=10)
print("one invocation:", invocation.title)
for version in history["versions"]:
    print(" ", version.get("sentence"), "| undone:", version.get("undone", False))

# Take the invocation back: exactly what its calls changed.
invocation.undo()

after = workspace.do("word-processor#history", last=10)
print("undone:", sum(1 for version in after["versions"] if version.get("undone")), "steps")
```

```typescript example=invocation-and-undo
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Invocations and history")).id);
type History = { versions: Record<string, unknown>[] };

// One invocation: every call inside it is one step of this app's history.
const invocation = await workspace.invocation("Write the cover letter", async (group) => {
  await workspace.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
  await workspace.do("word-processor#append-paragraph", { text: "Yours faithfully, Jane Doe" });
  return group;
});

const history = (await workspace.do("word-processor#history", { last: 10 })) as History;
console.log("one invocation:", invocation.title);
for (const version of history.versions) {
  console.log(" ", version.sentence, "| undone:", version.undone ?? false);
}

// Take the invocation back: exactly what its calls changed.
await invocation.undo();

const after = (await workspace.do("word-processor#history", { last: 10 })) as History;
console.log("undone:", after.versions.filter((version) => version.undone === true).length, "steps");
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
  invocation=$(curl -sS --fail-with-body "$word/append-paragraph" -H "$auth" -H "$json" "${group[@]}" \
    -d "{\"text\": \"$text\"}" | jq -r .invocation)
done

# Take the invocation back, by any of its calls: exactly what it changed.
curl -sS --fail-with-body -X POST "$api/workspaces/$workspace/invocations/$invocation/undo" -H "$auth" \
  | jq -r '"status: \(.status)"'
curl -sS --fail-with-body "$word/history" -H "$auth" -H "$json" -d '{"last": 10}' \
  | jq -r '"undone: \([.result.versions[] | select(.undone)] | length) steps"'
```

## Every invocation you may read

A workspace lists its invocations, newest first, whoever made them (every key of yours, every agent's key
made for it, and you), as far as the key asking may read the records they changed. Several agents each
working in a workspace of their own leave one list per workspace of what was invoked, what waits for you,
and what can still be taken back.

The invocations are a record of the workspace, `record:tilo.invocations:`, read whole through the
record API (`ws.invocations()`; `GET /v1/workspaces/{workspace}/resources/record:tilo.invocations:`;
over MCP `tilo_read record:tilo.invocations:`; `tilo invocations -w <workspace>` pages it). A reader
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
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Every invocation").id)

with workspace.invocation("Write the cover letter"):
    workspace.do("word-processor#append-paragraph", text="Dear Visa Officer,")

listed = workspace.invocations()
print(listed.total, "invocations, newest first;", listed.waiting, "waiting for you")
for invocation in listed:
    print(
        " ",
        invocation.status,
        "by",
        invocation.by,
        "|",
        invocation.title,
        "|",
        invocation.changes,
        "changes",
    )

letter = next(item for item in listed.invocations if item.title == "Write the cover letter")
assert letter.status == "ended" and letter.undoable, letter
print("found:", letter.id, letter.records[0]["app"])
```

```typescript example=list-invocations
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Every invocation")).id);

await workspace.invocation("Write the cover letter", async () => {
  await workspace.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
});

const listed = await workspace.invocations();
console.log(listed.total, "invocations, newest first;", listed.waiting, "waiting for you");
for await (const invocation of listed) {
  console.log(
    " ",
    invocation.status,
    "by",
    invocation.by,
    "|",
    invocation.title,
    "|",
    invocation.changes,
    "changes",
  );
}

const letter = listed.invocations.find((item) => item.title === "Write the cover letter");
if (letter === undefined || letter.status !== "ended" || !letter.undoable) {
  throw new Error(`the letter's invocation is listed as ended: ${JSON.stringify(letter)}`);
}
console.log("found:", letter.id, letter.records[0]?.app);
```

```curl example=list-invocations
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
json="Content-Type: application/json"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "$json" \
  -d '{"name": "Every invocation"}' | jq -r .result.workspace.id)
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/word-processor/actions/append-paragraph" \
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

`ws.link(path)` answers the link to anything a path names. An invocation collects the links to every
record it changed, once each, in `invocation.links`, and `invocation.link` is the one link when it changed
one record.

## Watching it happen

`watch` answers changes, invocation events and who is there as they arrive ([events](events.md)).

## Where next

- [Approvals](approvals.md): the actions an invocation cannot take without a person.
- [Errors](errors.md): what a refused step raises.
