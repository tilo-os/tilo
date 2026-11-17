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

The API in front of the hosts keeps no workspace itself. It checks the key against the grant it carries
and forwards each call to the host that serves that workspace best right now, so the same key works
through any of our API servers and through yours. When only hosts that hold a workspace's encrypted data
are online, none that can read it, the call is refused in plain words rather than answered.

## One workspace on more than one host

A workspace you made on one host can be held by another as well: make it on your own host and have tilo.sh
keep it too, say. Both keep the whole workspace, a change on either reaches the other, and you and its
other members reach it at either, also while the first one is off.

```sh
tilo workspace hosts add https://tilo.sh -w Tilo      # tilo.sh holds it as well
tilo workspace hosts ls -w Tilo                       # the hosts holding it, the one it was made on first
tilo workspace hosts remove https://tilo.sh -w Tilo   # tilo.sh lets it go
```

The same is `workspace/hosts#add {origin}`, `#list` and `#remove {host}` for an agent over MCP, and
`POST`, `GET` and `DELETE /v1/workspaces/{workspace}/hosts` over HTTP. Only the workspace's owners
choose its hosts, from a device of theirs or a key they created reaching all their workspaces.

The other host decides whether it takes the workspace, by its own rules: it keeps one only for an owner
of it whom it let in (on tilo.sh: you signed in there), and only while that person's workspaces there
have room. Otherwise it refuses, and says why. The host the workspace was made on stays the one that
decides who is in it; the others follow it.

## Running your own host

You can point the SDKs, the command and an agent's MCP client at a Tilo host you run. Everything above
holds there too: it is the same API, and the key comes from the same place. [Your own host](own-host.md)
has the steps.
