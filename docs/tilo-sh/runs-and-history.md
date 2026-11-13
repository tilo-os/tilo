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

- **One step**: `undo` with `steps: 1` on the record, which takes back the last change.
- **A run**: `undo` with `run: <run id>`, which takes back every step that run made in that app.
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

# Undo the run: the app's own step, named by the run it belongs to.
run_id = next(version["run_id"] for version in history["versions"] if version.get("run_id"))
workspace.do("word-processor#undo", run=run_id)

after = workspace.do("word-processor#history", last=10)
print("undone:", sum(1 for version in after["versions"] if version.get("undone")), "steps")
```

```typescript example=run-and-undo
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Runs and history")).id);

// One run: every call inside it is one step of this app's history.
const history = await workspace.run("Write the cover letter", async () => {
  await workspace.do("word-processor#append-paragraph", { text: "Dear Visa Officer," });
  await workspace.do("word-processor#append-paragraph", { text: "Yours faithfully, Jane Doe" });
  return (await workspace.do("word-processor#history", { last: 10 })) as {
    versions: Record<string, unknown>[];
  };
});

for (const version of history.versions) {
  console.log(" ", version.sentence, "| undone:", version.undone ?? false);
}

// Undo the run: the app's own step, named by the run it belongs to.
const runId = history.versions.find((version) => version.run_id !== undefined)?.run_id as string;
await workspace.do("word-processor#undo", { run: runId });

const after = (await workspace.do("word-processor#history", { last: 10 })) as {
  versions: Record<string, unknown>[];
};
console.log("undone:", after.versions.filter((version) => version.undone === true).length, "steps");
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
