# Webhooks and events

A backend that waits for something to happen should not poll for it. Tilo answers a **watch**: a stream of what
happened in a workspace, from a cursor. An API server also posts the same events to your server, signed, as
**webhooks** ([below](#webhooks)).

## The stream

`watch` answers three kinds of event, all in one stream, each once:

| Kind | What it says |
| :--- | :--- |
| `change` | A step in a record's history: its path, what it did (`sentence`), who (`by`), the run, and whether it has been undone |
| `run` | An invocation starting or finishing: `started`, `succeeded`, `failed` or `cancelled`, with its invocation id, path and run |
| `presence` | Who is in the workspace, and their agents, whenever that changes |

The stream is the workspace's `events` route of the [API](../developers/api.md): server-sent events, its
answers named `change`, `run` and `presence`. Each event's `id` is a cursor: reconnecting with
`Last-Event-ID` resumes after it, so a caller that keeps its cursor sees each event once. A comment line
every 15 seconds keeps an idle connection open.

`path` narrows change events to one record and what is under it, which is what a page or a job usually
wants.

## Watching from your code

`ws.watch()` is an async iterator: Python's `async for`, TypeScript's `for await`. Here a watch is opened,
a page is made, and the first event is printed:

```python example=watch-stream
import asyncio

from tilo import AsyncTilo


async def main() -> None:
    tilo = AsyncTilo()
    workspace = tilo.workspace((await tilo.workspaces.create("Watching")).id)

    async def first_change():
        async for event in workspace.watch():
            if event.kind == "change":
                return event
        raise RuntimeError("the watch ended without a change")

    watcher = asyncio.create_task(first_change())
    await asyncio.sleep(1)                      # let the watch attach
    await workspace.do("notes#create-page", title="Arrivals", markdown="- 09:40 Jane Doe\n")

    event = await asyncio.wait_for(watcher, timeout=30)
    print("kind:", event.kind, "path:", event.path, "sentence:", event.sentence, "run:", event.run)
    await tilo.aclose()


asyncio.run(main())
```

```typescript example=watch-stream
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Watching")).id);

async function firstChange() {
  for await (const event of workspace.watch()) {
    if (event.kind === "change") {
      return event;
    }
  }
  throw new Error("the watch ended without a change");
}

const watcher = firstChange();
await new Promise((resolve) => setTimeout(resolve, 1000)); // let the watch attach
await workspace.do("notes#create-page", { title: "Arrivals", markdown: "- 09:40 Jane Doe\n" });

const event = await Promise.race([
  watcher,
  new Promise<never>((_, reject) => setTimeout(() => reject(new Error("no change in 30s")), 30000)),
]);
console.log("kind:", event.kind, "path:", event.path, "sentence:", event.sentence, "run:", event.run);
process.exit(0);
```

```curl example=watch-stream
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Watching"}' | jq -r .id)

# The stream, read in the background until its first change.
curl -sSN "$api/workspaces/$workspace/events" -H "$auth" > events.txt &
watching=$!
sleep 1
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/notes/actions/create-page" -H "$auth" \
  -H "Content-Type: application/json" -d '{"title": "Arrivals", "markdown": "- 09:40 Jane Doe\n"}' > /dev/null
for _ in $(seq 30); do grep -q '^event: change' events.txt && break; sleep 1; done
kill "$watching"
grep -A1 '^event: change' events.txt | head -2
```

The call's own `run` events arrive too; the example reads past them to the first `change`, which names the
app, the record, the sentence a person would read, and the run that made it.

## Watching one run

A `Run` has the same stream narrowed to itself: `run.output()` answers its events, from `started` to
`succeeded`, `failed` or `cancelled`, and stops there. That is how a caller follows a long action without
polling its status.

## Webhooks

On an API server, `POST /v1/workspaces/{workspace}/webhooks` with `{"url", "events"?, "path"?}` posts each
event to `url` as it happens: the same events, as JSON, with a `Tilo-Signature` header (an HMAC-SHA256 of
the timestamp and the body under the secret the call answers once) and a `Tilo-Delivery` id to deduplicate
by. A delivery not answered with a `2xx` is tried again, in order, for close to three hours. The
[API reference](../developers/api.md#webhooks) has the details.

## Tools

A watch is the workspace's event stream (`GET /v1/workspaces/{workspace}/events`, resumed with
`Last-Event-ID`), so anything that speaks the API can watch, and both SDKs wrap it. The `tilo` command has
no watch verb today.

## Where next

- [Runs and history](runs-and-history.md): what a `change` event is a step of.
- [Approvals](approvals.md): the events a person's answer releases.
