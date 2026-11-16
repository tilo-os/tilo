# Runs and history

Nothing in a workspace changes anonymously. Each connection is a **run**, every change is a step in its
record's **history**, a run can be taken back as one piece, and the workspace lists **every run** in
it.

## A run

An agent's connection is a run named after the key it holds, so a workspace with three customer agents
shows three runs, each with its own name and colour. Your code's calls are runs too, named after their
key.

A run can also be named by you. Everything you do inside `ws.run(title)` is one run, and each app records
the run's changes to it as **one step**. So a run of thirty calls that filled a form and wrote a letter is
two steps: one in the PDF, one in Writer. The example under [Undo](#undo) opens a run, reads the history
it left, and takes it back.

## History

Each record keeps its own history: what changed, in words, who did it, when, and which run it was part
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

- **A run**: `run.undo()` (`POST /v1/workspaces/{workspace}/runs/{run}/undo`) takes back exactly what
  the run's calls changed, in every record they changed. When someone else's change stands on top of the
  run's in a record, it is refused and nothing is taken back, naming whose change it is.
- **One step**: `undo` with `steps: 1` on the record, which takes back its last change, whoever made it.
- **A person**: Undo in the record's history, which is those same actions.

```python example=run-and-undo
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Runs and history").id)

# One run: every call inside it is one step of this app's history.
with workspace.run("Write the cover letter") as run:
    workspace.do("word-processor#append-paragraph", text="Dear Visa Officer,")
    workspace.do("word-processor#append-paragraph", text="Yours faithfully, Jane Doe")

history = workspace.do("word-processor#history", last=10)
print("one run:", run.title)
for version in history["versions"]:
    print(" ", version.get("sentence"), "| undone:", version.get("undone", False))

# Take the run back: exactly what its calls changed.
run.undo()

after = workspace.do("word-processor#history", last=10)
print("undone:", sum(1 for version in after["versions"] if version.get("undone")), "steps")
```

```typescript example=run-and-undo
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Runs and history")).id);
type History = { versions: Record<string, unknown>[] };

// One run: every call inside it is one step of this app's history.
const run = await workspace.run("Write the cover letter", async (run) => {
  await workspace.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
  await workspace.do("word-processor#append-paragraph", { text: "Yours faithfully, Jane Doe" });
  return run;
});

const history = (await workspace.do("word-processor#history", { last: 10 })) as History;
console.log("one run:", run.title);
for (const version of history.versions) {
  console.log(" ", version.sentence, "| undone:", version.undone ?? false);
}

// Take the run back: exactly what its calls changed.
await run.undo();

const after = (await workspace.do("word-processor#history", { last: 10 })) as History;
console.log("undone:", after.versions.filter((version) => version.undone === true).length, "steps");
```

```curl example=run-and-undo
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
json="Content-Type: application/json"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "$json" \
  -d '{"name": "Runs and history"}' | jq -r .id)
word=$api/workspaces/$workspace/resources/word-processor/actions

# One run: every call naming it in Tilo-Run is one step of this app's history.
run=(-H "Tilo-Run: cover-letter" -H "Tilo-Run-Title: Write the cover letter")
for text in "Dear Visa Officer," "Yours faithfully, Jane Doe"; do
  invocation=$(curl -sS --fail-with-body "$word/append-paragraph" -H "$auth" -H "$json" "${run[@]}" \
    -d "{\"text\": \"$text\"}" | jq -r .run)
done

# Take the run back, by any of its calls: exactly what the run changed.
curl -sS --fail-with-body -X POST "$api/workspaces/$workspace/runs/$invocation/undo" -H "$auth" \
  | jq -r '"status: \(.status)"'
curl -sS --fail-with-body "$word/history" -H "$auth" -H "$json" -d '{"last": 10}' \
  | jq -r '"undone: \([.result.versions[] | select(.undone)] | length) steps"'
```

## Every run

A workspace lists its runs, newest first, whoever ran them: every key of yours, every agent's key
made for it, and you. Several agents each working in a workspace of their own leave one list per
workspace of what ran, what waits for you, and what can still be taken back.

`ws.runs()` (`GET /v1/workspaces/{workspace}/runs`; over MCP `tilo_read workspace/runs`; `tilo runs
-w <workspace>`) answers a page of 50, `limit` up to 200, and reads on from the cursor a page ends
with. Each run says:

- **who ran it**: the name of its key or agent, and its kind (`agent` for every key; `person`,
  `bot` or `automation` otherwise), and the title its client gave it with `ws.run(title)`;
- **how it stands**: `waiting` for you to allow it, `running`, `ended`, `failed` (with a typed
  `failure`, `confirmation_required` when you did not allow it), `cancelled`, or `undone` once
  everything it changed was taken back;
- **when**: `started_ms`, and `ended_ms` once it ended;
- **what it touched**: how many `changes` it made and to which `records`, how many were `undone`,
  and the `summary` of its newest change as History words it;
- **whether it can still be taken back**: `undoable`, or `undo_blocked` naming whose change stands
  on top of its own.

The page also says the `total`, and how many runs are `waiting` for you.

A run's `id` is the run id the other run calls take (`ws.status(id)`, its cancel and undo): the
newest call of it. Which runs are listed, and how, holds across restarts of the host as far as the
workspace's records keep them. A run that changed a record is kept in that record's history, so it
is listed the same after the host restarts (`recorded`), only without an `id`: the host remembers
a run's calls (`remembered`) until it restarts, or lets go of the oldest among many. Its `step`,
the undo step its changes carry in every record's History, names it either way. A run that changed
nothing (one still waiting for you, one that failed before it wrote) is known from the host's
memory alone, and is not listed after a restart.

Any key that reaches a workspace reads every run in it, as History shows every change there.
What a run was given, what it answered and the words of its failure stay with whoever ran it.

```python example=list-runs
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Every run").id)

with workspace.run("Write the cover letter"):
    workspace.do("word-processor#append-paragraph", text="Dear Visa Officer,")

runs = workspace.runs()
print(runs.total, "runs, newest first;", runs.waiting, "waiting for you")
for run in runs:
    print(" ", run.status, "by", run.by, "|", run.title, "|", run.changes, "changes")

letter = next(run for run in runs.runs if run.title == "Write the cover letter")
assert letter.status == "ended" and letter.undoable, letter
print("found:", letter.id, letter.records[0]["app"])
```

```typescript example=list-runs
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Every run")).id);

await workspace.run("Write the cover letter", async () => {
  await workspace.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
});

const runs = await workspace.runs();
console.log(runs.total, "runs, newest first;", runs.waiting, "waiting for you");
for await (const run of runs) {
  console.log(" ", run.status, "by", run.by, "|", run.title, "|", run.changes, "changes");
}

const letter = runs.runs.find((run) => run.title === "Write the cover letter");
if (letter === undefined || letter.status !== "ended" || !letter.undoable) {
  throw new Error(`the letter's run is listed as ended: ${JSON.stringify(letter)}`);
}
console.log("found:", letter.id, letter.records[0]?.app);
```

```curl example=list-runs
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
json="Content-Type: application/json"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "$json" \
  -d '{"name": "Every run"}' | jq -r .id)
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/word-processor/actions/append-paragraph" \
  -H "$auth" -H "$json" -H "Tilo-Run: cover-letter" -H "Tilo-Run-Title: Write the cover letter" \
  -d '{"text": "Dear Visa Officer,"}' > /dev/null

# Newest first: who ran each, how it stands and what it changed.
curl -sS --fail-with-body "$api/workspaces/$workspace/runs?limit=10" -H "$auth" \
  | jq -e -r '.runs[] | select(.title == "Write the cover letter")
      | "\(.status) by \(.by.name): \(.changes) changes, undoable: \(.undoable)"'
```

## Links for people

Work ends with a person looking at it, so a call that changed a record answers that record's **link**: the
address a person copies in the app, which opens the record in it. A link carries no secret; it opens for
the workspace's members, so invite the person first.

```text
https://tilo.sh/app/#/@eu1.tilo.sh/w/6f1c…/so.tilo.pdf-viewer/uuid:a65e…
```

`ws.link(path)` answers the link to anything a path names. A run collects the links to every record it
changed, once each, in `run.links`, and `run.link` is the one link when it changed one record.

## Watching it happen

`watch` answers changes, run events and who is there as they arrive ([events](events.md)).

## Where next

- [Approvals](approvals.md): the actions a run cannot take without a person.
- [Errors](errors.md): what a refused step raises.
