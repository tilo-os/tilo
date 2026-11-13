# Search

Two ways find something in a workspace: search it by words (`find`), or walk it: the apps, then an app's
records, then one record's contents. Every host answers both, the same way.

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

`find` (`GET /v1/workspaces/{workspace}/search?q=`) is the host's search over what the workspace's
records hold: their titles and text. It answers hits, best first, one per record, each as your own
listing shows it: a `path` the other calls take, its `kind`, its `label`, and the `action` the row offers
(`open` when the row opens the resource itself). A record sealed end to end that the host cannot open is
not found by words; walk to it instead.

```python example=find-by-words
from tilo import Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("Searching").id)
workspace.do("notes#create-page", title="Visa checklist", markdown="- passport\n- photos\n")

for hit in workspace.find("passport"):
    print(hit.path, "|", hit.label, "|", hit.kind)
```

```typescript example=find-by-words
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Searching")).id);
await workspace.do("notes#create-page", { title: "Visa checklist", markdown: "- passport\n- photos\n" });

for (const hit of await workspace.find("passport")) {
  console.log(hit.path, "|", hit.label, "|", hit.kind);
}
```

```curl example=find-by-words
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Searching"}' | jq -r .id)
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/notes/actions/create-page" -H "$auth" \
  -H "Content-Type: application/json" \
  -d '{"title": "Visa checklist", "markdown": "- passport\n- photos\n"}' > /dev/null

curl -sS --fail-with-body "$api/workspaces/$workspace/search?q=passport" -H "$auth" \
  | jq -r '.hits[] | "\(.path) | \(.label) | \(.kind)"'
```
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
