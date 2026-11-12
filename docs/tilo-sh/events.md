# Webhooks and events

A backend that waits for something to happen should not poll for it. Tilo answers a **watch**: a stream of what
happened in a workspace, from a cursor, one long call at a time. There are no webhooks yet: nothing calls
your server. A watch is what exists, and this page is about it.

## The stream

`watch` answers three kinds of event, all in one stream, each once:

| Kind | What it says |
| :--- | :--- |
| `change` | A step in a record's history: its path, what it did (`sentence`), who (`by`), the run, and whether it has been undone |
| `run` | An invocation starting or finishing: `started`, `succeeded`, `failed` or `cancelled`, with its invocation id, path and run |
| `presence` | Who is in the workspace, and their agents, whenever that changes |

Each answer carries a `cursor`. Asking again from it continues where you stopped, so a caller that keeps its
cursor sees each event once. `wait_ms` is how long a call holds when nothing happens: 25 seconds by
default, 60 at most. A watch is a long poll, not a push: one call at a time, through the same session
every other call takes, and the next call starts where the last stopped.

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

    async def first_event():
        async for event in workspace.watch(wait_ms=5000):
            return event
        raise RuntimeError("the watch ended without an event")

    watcher = asyncio.create_task(first_event())
    await asyncio.sleep(1)                      # let the watch attach
    await workspace.do("notes#create-page", title="Arrivals", markdown="- 09:40 Jane Doe\n")

    event = await asyncio.wait_for(watcher, timeout=30)
    print("kind:", event.kind, "path:", event.path, "phase:", event.phase, "run:", event.run)
    await tilo.aclose()


asyncio.run(main())
```

```typescript example=watch-stream
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("Watching")).id);

async function firstEvent() {
  for await (const event of workspace.watch(null, 5000)) {
    return event;
  }
  throw new Error("the watch ended without an event");
}

const watcher = firstEvent();
await new Promise((resolve) => setTimeout(resolve, 1000)); // let the watch attach
await workspace.do("notes#create-page", { title: "Arrivals", markdown: "- 09:40 Jane Doe\n" });

const event = await Promise.race([
  watcher,
  new Promise<never>((_, reject) => setTimeout(() => reject(new Error("no event in 30s")), 30000)),
]);
console.log("kind:", event.kind, "path:", event.path, "phase:", event.phase, "run:", event.run);
```

The first event to arrive is often the `run` event of the call you just made, which is what the example
prints. A `change` event names the record's path and the sentence a person would read.

## Watching one run

A `Run` has the same stream narrowed to itself: `run.output()` answers its events, from `started` to
`succeeded`, `failed` or `cancelled`, and stops there. That is how a caller follows a long action without
polling `status`.

## Tools

A watch is the `tilo_watch` call of the API a workspace serves, so any MCP client that can call a tool can
watch, and both SDKs wrap it. The `tilo` command has no watch verb today.

## Where next

- [Runs and history](runs-and-history.md): what a `change` event is a step of.
- [Approvals](approvals.md): the events a person's answer releases.
