# Identities and keys

Every change in a workspace has an author. Tilo answers who with one of three things: a person, a device
of theirs, or an agent acting for them. A **key** is what lets a program, or an agent, act as one of them.

## People and devices

A person's Tilo is an identity: a root key, and one device key for each device that holds a copy of it —
a laptop, a phone, a browser tab. Signing in on a new device needs one device that is already the person,
which approves the new one. That is also what approves a key: the person's own device signs it, so no
browser has to be in the loop when the key is used later.

## Agents

An agent is the person's agent, not a separate account. It acts as them, is held to an editor's role at
most, and governs, invites or administers nothing. Its calls show up under its own name, in its own
colour, in each record's history and in who-is-here.

An agent gets access in one of two ways. An MCP client registers itself at
`https://mcp.tilo.sh` and the person approves it once in Tilo; that grant reaches every workspace of
theirs, or one workspace when the agent was pointed at `https://mcp.tilo.sh/w/<workspace>/mcp`, and it
expires within 90 days. A **key** is the same grant for code that has no browser: your backend, a script,
a job.

## Keys

A key is a token that starts `tilo_sk_`. It is shown once, when it is made, and the host keeps only a
digest of it; the prefix is there so a secret scanner finds one committed by mistake.

What a key reaches is decided when it is made:

| The key | Reaches | Lasts |
| :--- | :--- | :--- |
| **All your workspaces** | list and make workspaces, mint keys, and act in any of them | until you revoke it, unless you set an expiry |
| **One workspace** | that workspace and nothing else, with no account calls | about three months at most |

A key for all your workspaces is what a backend holds: it runs unattended, so it does not break on a
date. What bounds it is revocation, which ends everything it minted, and the record of when it was last
used and which device of yours approved it.

A key for one workspace is what you hand a customer's agent. It cannot list your workspaces, make one, or
mint another key, and every action it takes is attributed to it.

### Narrowing, expiring, revoking

A key that reaches all your workspaces can **mint** keys that reach one workspace each. Minting asks
nobody: the parent already reaches that workspace, and the minted key reaches strictly less. A minted key
expires no later than the key that minted it, and revoking the parent ends every key minted from it.

```sh
tilo keys mint -w jane-doe --name "Jane Doe's agent"   # its MCP URL and token
```

Revoking is one call, or one click on [your grants page](https://mcp.tilo.sh/grants). It also ends the
key's open sessions: the agent's next call fails with `KeyRevoked`, which says when it was revoked
([errors](errors.md)).

### The page scope

A key can carry the authority to operate the pages a workspace's Browser holds. It is on unless you say
no: the key navigates, fills in, selects and clicks without asking the person each time, and a key minted
from it keeps its answer. It is not a licence to leave the page: a submission, a file to upload and a
password still ask the person, whatever the key says ([approvals](approvals.md#the-hosted-browser)). Turn
it off and every page action asks:

```sh
tilo keys create "Cautious backend" -w jane-doe --no-browser-pages
```

## A key in practice

This mints a key for one workspace, uses it as the agent would, and revokes it:

```python example=key-scopes
from tilo import KeyRevoked, Tilo

tilo = Tilo()
workspace = tilo.workspace(tilo.workspaces.create("One customer").id)

# A key that reaches this workspace and nothing else.
minted = tilo.keys.mint(workspace, name="Their agent")
print("agent's MCP URL:", minted.mcp_url)
print("it reaches:", minted.workspace)

# The agent's own client: with that key it sees this workspace's apps.
agent = Tilo(key=minted.token)
print("the agent sees", len(agent.workspace(minted.workspace).ls().rows), "apps")

# Revoking it ends the agent's access at once.
tilo.keys.revoke(minted.grant)
try:
    agent.workspace(minted.workspace).ls()
    print("still allowed")
except KeyRevoked as error:
    print("after revoking:", error.code, "at", error.revoked_at_ms)
```

```typescript example=key-scopes
import { KeyRevoked, Tilo } from "tilo-sdk";

const tilo = new Tilo();
const workspace = tilo.workspace((await tilo.workspaces.create("One customer")).id);

// A key that reaches this workspace and nothing else.
const minted = await tilo.keys.mint(workspace, "Their agent");
console.log("agent's MCP URL:", minted.mcpUrl);
console.log("it reaches:", minted.workspace);

// The agent's own client: with that key it sees this workspace's apps.
const agent = new Tilo({ key: minted.token });
const apps = await agent.workspace(minted.workspace).ls();
console.log("the agent sees", apps.rows.length, "apps");

// Revoking it ends the agent's access at once.
await tilo.keys.revoke(minted.grant);
try {
  await agent.workspace(minted.workspace).ls();
  console.log("still allowed");
} catch (error) {
  if (error instanceof KeyRevoked) {
    console.log("after revoking:", error.code, "at", error.revokedAtMs);
  } else {
    throw error;
  }
}
```

```curl example=key-scopes
api=$TILO_API_URL/v1
auth="Authorization: Bearer $TILO_API_KEY"
workspace=$(curl -sS --fail-with-body "$api/workspaces" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "One customer"}' | jq -r .id)

# A key that reaches this workspace and nothing else.
minted=$(curl -sS --fail-with-body "$api/keys" -H "$auth" -H "Content-Type: application/json" \
  -d "{\"workspace\": \"$workspace\", \"name\": \"Their agent\"}")
echo "agent's MCP URL: $(jq -r .mcp_url <<<"$minted")"
agent="Authorization: Bearer $(jq -r .token <<<"$minted")"

# With that key, the agent sees this workspace's apps.
apps=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources" -H "$agent" | jq '.rows | length')
echo "the agent sees $apps apps"

# Revoking it ends the agent's access at once.
curl -sS --fail-with-body -X DELETE "$api/keys/$(jq -r .grant <<<"$minted")" -H "$auth" > /dev/null
curl -sS "$api/workspaces/$workspace/resources" -H "$agent" \
  | jq -r '"after revoking: \(.error.code) at \(.error.revoked_at_ms)"'
```

## Where keys live

- **Your grants page**, `https://mcp.tilo.sh/grants`: every key and every agent you let in, with its
  scope, when it was last used, which of your devices approved it, and a Revoke button.
- **The command**: `tilo keys create "My backend"`, `tilo keys ls`, `tilo keys mint -w <workspace>`,
  `tilo keys revoke <grant>`.
- **The SDKs**: `tilo.keys.list()`, `create()`, `mint()`, `revoke()`, and `workspace.keys.mint()`.
