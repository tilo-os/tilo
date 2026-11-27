# Webhooks and events

A backend that waits for something to happen should not poll for it. Tilo answers a **watch**: a stream of what
happened in a workspace, from a cursor. An API server also posts the same events to your server, signed, as
**webhooks** ([below](#webhooks)).

## The stream

`watch` answers three kinds of event, all in one stream, each once:

| Kind | What it says |
| :--- | :--- |
| `change` | A step in a record's history: `resources` names the app and the id it gives the changed item, `path` what was watched, `sentence` what it did, `by` who, the group, and whether it has been undone |
| `invocation` | An invocation starting or finishing: `started`, `succeeded`, `failed` or `cancelled`, with its id, path, title and group |
| `presence` | Who is in the workspace, and their agents, whenever that changes |

The stream is the workspace's `events` route of the [API](../developers/api.md): server-sent events, its
answers named `change`, `invocation` and `presence`. Each event's `id` is a cursor: reconnecting with
`Last-Event-ID` resumes after it, so a caller that keeps its cursor sees each event once. A comment line
every 15 seconds keeps an idle connection open.

`path` narrows change events to one record and what is under it, which is what a page or a job usually
wants.

## Watching from your code

`ws.events()` is an async iterator: Python's `async for`, TypeScript's `for await`. Here a watch is opened,
a snippet is made, and the first event is printed:

```python example=watch-stream
import asyncio

from tilo import Tilo, run


async def main() -> None:
    async with Tilo() as tilo:
        made = await run(
            tilo, {"path": "account/workspaces", "action": "create"}, {"name": "Watching"}
        )
        workspace = tilo.workspace(made["result"]["workspace"]["id"])
        events = workspace.events()

        async def first_change():
            async for event in events:
                if event["type"] == "change":
                    return event
            raise RuntimeError("the watch ended without a change")

        watcher = asyncio.create_task(first_change())
        await asyncio.sleep(1)                  # let the watch attach
        await run(workspace, {"path": "snippets", "action": "new-snippet"}, {"title": "Arrivals"})

        event = await asyncio.wait_for(watcher, timeout=30)
        data = event["data"]
        print(
            "type:",
            event["type"],
            "path:",
            data.get("path"),
            "what:",
            data.get("what"),
            "change:",
            data.get("change"),
        )


asyncio.run(main())
```

```typescript example=watch-stream
import { Tilo, run } from "tilo-sdk";

const tilo = new Tilo();
const made = await run(tilo, { path: "account/workspaces", action: "create" }, { name: "Watching" });
const workspace = tilo.workspace((made.result as { workspace: { id: string } }).workspace.id);
const events = workspace.events();

async function firstChange() {
  for await (const event of events) {
    if (event.type === "change") {
      return event;
    }
  }
  throw new Error("the watch ended without a change");
}

const watcher = firstChange();
await new Promise((resolve) => setTimeout(resolve, 1000)); // let the watch attach
await run(workspace, { path: "snippets", action: "new-snippet" }, { title: "Arrivals" });

const event = await Promise.race([
  watcher,
  new Promise<never>((_, reject) => setTimeout(() => reject(new Error("no change in 30s")), 30000)),
]);
const data = event.data as { path?: string; what?: string; change?: string };
console.log(
  "type:",
  event.type,
  "path:",
  data.path,
  "what:",
  data.what,
  "change:",
  data.change,
);
await events.return?.();
process.exit(0);
```

```curl example=watch-stream
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "Watching"}' | jq -r .result.workspace.id)

# The stream, read in the background until its first change.
curl -sSN "$api/workspaces/$workspace/events" -H "$auth" > events.txt &
watching=$!
sleep 1
curl -sS --fail-with-body "$api/workspaces/$workspace/resources/snippets/actions/new-snippet" -H "$auth" \
  -H "Content-Type: application/json" -d '{"title": "Arrivals"}' > /dev/null
for _ in $(seq 30); do grep -q '^event: change' events.txt && break; sleep 1; done
kill "$watching"
grep -A1 '^event: change' events.txt | head -2
```

The call's own `invocation` events arrive too; the example reads past them to the first `change`, which
names the app, the record, the sentence a person would read, and what the change did.

## Watching one invocation

`ws.invocationOutput(invocation)` (`invocation_output` in Python) is the same stream narrowed to one
run: its events, from `started` to `succeeded`, `failed` or `cancelled`, and stops there. That is how a
caller follows a long action without polling its status.

## Webhooks

On an API server, the account's `account/webhooks#create` with `{"workspace", "url", "events"?, "path"?}` posts each
event to `url` as it happens: the same events, as JSON, with a `Tilo-Signature` header (an HMAC-SHA256 of
the timestamp and the body under the secret the call answers once) and a `Tilo-Delivery` id to deduplicate
by. A delivery not answered with a `2xx` is tried again, in order, for close to three hours. The
[API reference](../developers/api.md#webhooks) has the details.

## Tools

A watch is the workspace's event stream (`GET /v1/workspaces/{workspace}/events`, resumed with
`Last-Event-ID`), so anything that speaks the API can watch, and both SDKs wrap it. The `tilo` command has
no watch verb today.

## Where next

- [Invocations and history](invocations-and-history.md): what a `change` event is a step of.
- [Approvals](approvals.md): the events a person's answer releases.
