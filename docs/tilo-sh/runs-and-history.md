# Runs and history

Nothing in a workspace changes anonymously. Each connection is a **run**, every change is a step in its
record's **history**, and a run can be taken back as one piece.

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
