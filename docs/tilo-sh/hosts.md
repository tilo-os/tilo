# Hosts

A **host** is a server that keeps workspaces online. `tilo.sh` is our hosted service, or you can
deploy your own server. Both expose the same API to your keys, SDKs, CLI, and agents.

## What a host provides

A host keeps a copy of your workspace's documents, files, and history online so they remain
reachable even when your laptop or desktop is asleep. Your devices sync directly with it and with
each other.

A host can also provide compute for each workspace: a dedicated Linux environment with a Browser,
Terminal, and Linux apps that sleeps automatically when idle ([Hosted Browser](hosted-browser.md)).

## Multiple hosts

You can keep workspaces across more than one host. Listing workspaces shows which host stores each
one and your role in it:

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

curl -sS --fail-with-body "$api/resources/account/workspaces/actions/list" -H "$auth" \
  -H "Content-Type: application/json" -d '{}' \
  | jq -r '.result.workspaces[] | "\(.name)  on \(.host)  as \(.role // "owner")"'

created=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Idempotency-Key: my-own" \
  -H "Content-Type: application/json" -d '{"name": "A workspace of my own"}' \
  | jq -r .result.workspace)
echo "created: $(jq -r .name <<<"$created") on $(jq -r .host <<<"$created")"
```

The rows you list carry the role you hold in each workspace: `owner` for one of your own, else the role
you were invited with.

The API in front of the hosts keeps no workspace itself. It checks the key against the grant it carries
and forwards each call to the host that serves that workspace best right now, so the same key works
through any of our API servers and through yours. When only hosts that hold a workspace's encrypted data
are online, none that can read it, the call is refused in plain words rather than answered.

## One workspace on more than one server

A workspace you made on one server can be kept by another as well: make it on your own server and have tilo.sh
keep it too, say. Both keep the whole workspace, a change on either reaches the other, and you and its
other members reach it at either, also while the first one is off.

A server names itself to you with its own `web-host.json` line, which its operator hands over: its
certificate and where it is dialled. No site of the server's is asked who it is, so what you hold is
what names it.

```sh
tilo do workspace/servers#add host="$(cat web-host.json)" -w Tilo  # the server keeps it as well
tilo do workspace/servers#list -w Tilo                             # the servers keeping it, the one asked first
tilo do workspace/servers#remove server=tilo.sh -w Tilo            # it lets the workspace go
tilo do workspace/servers#move server=old.example.com host="$(cat web-host.json)" -w Tilo
                                                                  # it keeps the workspace, then the old one lets it go
tilo do workspace/servers#content -w Tilo                          # how much content is still only on members' devices
```

A server fetches the files and attachments the workspace's records name from its members' devices while
they are on. Before you switch off the last device that holds a copy (a laptop you moved the workspace
from, say), `tilo do workspace/servers#content -w Tilo` says whether the server keeps all of it.

The same is `workspace/servers#add {host}`, `#list`, `#remove {server}`, `#move {server, host}` and
`#content` for an agent over MCP, and
`POST /v1/workspaces/{workspace}/resources/workspace/servers/actions/{action}` over HTTP, as every action of the workspace's published tree. Only the workspace's owners choose its
servers, from a device of theirs or a key they created reaching all their workspaces. A move that the new
server refuses leaves the workspace where it was.

The other server decides whether it takes the workspace, by its own rules: it keeps one only for an owner
of it whom it let in (on tilo.sh: you signed in there), and only while that person's workspaces there
have capacity. Otherwise it refuses, and says why.

## Running your own host

You can point the SDKs, the command and an agent's MCP client at a Tilo host you run. Everything above
holds there too: it is the same API, and the key comes from the same place. [Your own host](own-host.md)
has the steps.
