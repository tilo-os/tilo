# Search

Two ways find something in a workspace: search it by words (`find`), or walk it: the apps, then an app's
records, then one record's contents. Every host answers both, the same way.

## Walk it

`ls` answers rows. A row that has children can be listed again, and every row carries the label a person
sees, so picking is a matter of reading labels rather than guessing ids:

```text
ls                 → the apps, each with the path its own listing takes
ls snippets        → the snippets
ls snippets/Todo   → the fields of one snippet, and the actions it offers
```

Paging is `limit`, and a listing answers a cursor for the rest.

```python example=find-by-listing
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo, {"path": "account/workspaces", "action": "create"}, {"name": "Looking for things"}
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])

        await run(workspace, {"path": "snippets", "action": "new-snippet"}, {"title": "Visa checklist"})
        await run(workspace, {"path": "snippets", "action": "new-snippet"}, {"title": "Packing list"})

        wanted = "visa"
        listing = await workspace.list_resources(path="snippets")
        for row in listing["rows"]:
            if wanted in (row.get("label") or row.get("name") or "").lower():
                print(row["path"], "|", row.get("label"), "|", row.get("kind"))


asyncio.run(main())
```

```typescript example=find-by-listing
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Looking for things" },
);
const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);

await run(workspace, { path: "snippets", action: "new-snippet" }, { title: "Visa checklist" });
await run(workspace, { path: "snippets", action: "new-snippet" }, { title: "Packing list" });

const wanted = "visa";
const snippets = await workspace.listResources({ path: "snippets" });
for (const row of snippets.rows) {
  if ((row.label || row.name).toLowerCase().includes(wanted)) {
    console.log(row.path, "|", row.label, "|", row.kind);
  }
}
```

```curl example=find-by-listing
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Looking for things"}' | jq -r .result.workspace.id)
resources=$api/workspaces/$workspace/resources

for snippet in '{"title": "Visa checklist"}' \
               '{"title": "Packing list"}'; do
  curl -sS --fail-with-body "$resources/snippets/actions/new-snippet" -H "$auth" \
    -H "Content-Type: application/json" -d "$snippet" > /dev/null
done

curl -sS --fail-with-body "$resources?path=snippets" -H "$auth" \
  | jq -r '.rows[] | select((.label // .name) | ascii_downcase | contains("visa"))
           | "\(.path) | \(.label) | \(.kind)"'
```

## Search by words

`find` (the `search#find` action, `POST /v1/workspaces/{workspace}/resources/search/actions/find`) is the host's search over what the workspace's
records keep: their titles and text. It answers hits, best first, one per record, each as your own
listing shows it: a `path` the other calls take, its `kind`, its `label`, and the `action` the row offers
(`open` when the row opens the resource itself). A record sealed end to end that the host cannot open is
not found by words; walk to it instead. What a change says joins the index a moment after it lands, so a
search made right after a write may answer none of it yet.

```python example=find-by-words
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo, {"path": "account/workspaces", "action": "create"}, {"name": "Searching"}
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])
        await run(
            workspace, {"path": "snippets", "action": "new-snippet"}, {"title": "Visa passport checklist"}
        )

        found = (
            await run(workspace, {"path": "search", "action": "find"}, {"query": "passport"})
        ).get("result") or {}
        for hit in found.get("hits") or []:
            print(hit["path"], "|", hit.get("label"), "|", hit.get("kind"))


asyncio.run(main())
```

```typescript example=find-by-words
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(
  tilo,
  { path: "account/workspaces", action: "create" },
  { name: "Searching" },
);
const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);
await run(workspace, { path: "snippets", action: "new-snippet" }, { title: "Visa passport checklist" });

const found = await run(workspace, { path: "search", action: "find" }, { query: "passport" });
for (const hit of ((found.result ?? {}) as { hits?: { path: string; label?: string; kind?: string }[] }).hits ?? []) {
  console.log(hit.path, "|", hit.label, "|", hit.kind);
}
```

```curl example=find-by-words
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Searching"}' | jq -r .result.workspace.id)
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/snippets/actions/new-snippet" -H "$auth" \
  -H "Content-Type: application/json" \
  -d '{"title": "Visa passport checklist"}' > /dev/null

curl -sS --fail-with-body "$api/workspaces/$workspace/resources/search/actions/find" -H "$auth" \
  -H "Content-Type: application/json" -d '{"query": "passport"}' \
  | jq -r '.result.hits[] | "\(.path) | \(.label) | \(.kind)"'
```
`run(ws, {"path": "search", "action": "find"}, {"query": …, "app": …, "limit": …})` narrows it to one app
and a count.

> [!NOTE]
> A hosted workspace does not run the desktop's search today. `search#find` there answers a refusal that says so
> and points at `ls`, so walking the tree — the example above — is what works from your code and from an
> agent right now.

## What is worth finding

An app decides what search can see and what it says about a record: a snippet's title and body, a task's
title and notes, a file's name. Those same labels are what a person reads in the palette and what an agent
gets when it lists.

## Where next

- [Events](events.md): be told when something arrives, instead of looking for it.
- [Resources and actions](resources-and-actions.md): what a hit's path names.
