# Hosts

A **host** is a computer that keeps workspaces. Ours is the one at tilo.sh. Yours can be a machine you run. To
you they are the same thing: a workspace lives on a host, and your keys, your code and your agents reach
it there.

## What a host keeps

A workspace's records, its files and its history live on its host, so they are there when no device of
yours is awake. Members' devices hold a copy and sync changes; the host is what makes a workspace
reachable at three in the morning and from a machine that has never seen it.

A host decides what it offers besides that. Ours runs each workspace's own Linux machine: a Browser that
opens web pages in it, a Terminal that runs commands in it, and Linux apps that open there. Your
workspace's machine is nobody else's, and it sleeps when nothing uses it ([the hosted
Browser](hosted-browser.md)).

## One host or several

A person can have workspaces on more than one host. Listing them says which host each is on, and a
workspace you make is one of your own, on the host that keeps your others:

```python example=workspaces-and-hosts
from tilo import Tilo

tilo = Tilo()
listing = tilo.workspaces.list()

for workspace in listing:
    print(f"{workspace.name}  on {workspace.host}  as {workspace.role or 'owner'}")

created = tilo.workspaces.create("A workspace of my own")
print("created:", created.name, "on", created.host)
```

```typescript example=workspaces-and-hosts
import { Tilo } from "tilo-sdk";

const tilo = new Tilo();
const listing = await tilo.workspaces.list();

for (const workspace of listing.workspaces) {
  console.log(`${workspace.name}  on ${workspace.host}  as ${workspace.role || "owner"}`);
}

const created = await tilo.workspaces.create("A workspace of my own");
console.log("created:", created.name, "on", created.host);
```

```curl example=workspaces-and-hosts
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"

curl -sS --fail-with-body "$api/workspaces" -H "$auth" \
  | jq -r '.workspaces[] | "\(.name)  on \(.host)  as \(.role // "owner")"'

created=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Idempotency-Key: my-own" \
  -H "Content-Type: application/json" -d '{"name": "A workspace of my own"}')
echo "created: $(jq -r .name <<<"$created") on $(jq -r .host <<<"$created")"
```

The rows you list carry the role you hold in each workspace: `owner` for one of your own, else the role
you were invited with.

## Running your own host

You can point the SDKs, the command and an agent's MCP client at a Tilo host you run. Everything above
holds there too: it is the same API, and the key comes from the same place. [Your own host](own-host.md)
has the steps.
