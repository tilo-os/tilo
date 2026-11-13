# Search

Anything in a workspace is reachable by walking it: the apps, then an app's records, then one record's
contents. That is what works today from your code and from an agent, and it is enough to find by name.
Searching by words is the desktop's search, and a hosted workspace does not run it yet.

## Walk it

`ls` answers rows. A row that has children can be listed again, and every row carries the label a person
sees, so picking is a matter of reading labels rather than guessing ids:

```text
ls                 → the apps, each with the path its own listing takes
ls notes           → the notes
ls notes/Todo      → the blocks of one note, and the actions it offers
```

Paging is `limit`, and a listing answers a cursor for the rest.

```python example=find-by-listing
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Looking for things").id)

workspace.do("notes#create-page", title="Visa checklist", markdown="- passport\n")
workspace.do("notes#create-page", title="Packing list", markdown="- socks\n")

wanted = "visa"
for row in workspace.ls("notes").rows:
    if wanted in (row.label or row.name).lower():
        print(row.path, "|", row.label, "|", row.kind)
```

```typescript example=find-by-listing
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Looking for things")).id);

await workspace.do("notes#create-page", { title: "Visa checklist", markdown: "- passport\n" });
await workspace.do("notes#create-page", { title: "Packing list", markdown: "- socks\n" });

const wanted = "visa";
const notes = await workspace.ls("notes");
for (const row of notes.rows) {
  if ((row.label || row.name).toLowerCase().includes(wanted)) {
    console.log(row.path, "|", row.label, "|", row.kind);
  }
}
```

```curl example=find-by-listing
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Looking for things"}' | jq -r .id)
resources=$api/workspaces/$workspace/resources

for page in '{"title": "Visa checklist", "markdown": "- passport\n"}' \
            '{"title": "Packing list", "markdown": "- socks\n"}'; do
  curl -sS --fail-with-body "$resources/notes/actions/create-page" -H "$auth" \
    -H "Content-Type: application/json" -d "$page" > /dev/null
done

curl -sS --fail-with-body "$resources?path=notes" -H "$auth" \
  | jq -r '.rows[] | select((.label // .name) | ascii_downcase | contains("visa"))
           | "\(.path) | \(.label) | \(.kind)"'
```

## Search by words

`find` is Tilo's one search: the same ranking the desktop's search box uses, over what each app publishes
for it. It answers hits, best first, one per resource: a `path` the other calls take, its `kind`, its
`label`, and the `action` the row offers (`open` when the row opens the resource itself).
`ws.find(query, app=None, limit=None)` narrows it to one app and a count.

> [!NOTE]
> A hosted workspace does not run the desktop's search today. `find` there answers a refusal that says so
> and points at `ls`, so walking the tree — the example above — is what works from your code and from an
> agent right now.

## What is worth finding

An app decides what search can see and what it says about a record: a note's title and text, a task's
title and notes, a file's name. Those same labels are what a person reads in the palette and what an agent
gets when it lists.

## Where next

- [Events](events.md): be told when something arrives, instead of looking for it.
- [Resources and actions](resources-and-actions.md): what a hit's path names.
