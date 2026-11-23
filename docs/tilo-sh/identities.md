# Identities and keys

Every change in a workspace is attributed to an author: a person, one of their devices, or an agent
acting on their behalf. An **API key** lets a script, backend, or agent authenticate with a scoped
grant.

## People and devices

Each person has one root identity and a certified device key per device (laptop, phone, browser).
Adding a new device requires approval from an existing device. API keys work the same way: your
device signs the grant once, so scripts and agents do not need a browser in the loop later.

## Agents

An agent acts on behalf of a person rather than as a separate account. It is capped at the editor
role (it cannot administer the workspace or invite members), and its actions appear under its own
name and color in history and live presence.

Agents authenticate in one of two ways:

- **MCP:** Register at `https://mcp.tilo.sh` (all workspaces) or
  `https://mcp.tilo.sh/w/<workspace>/mcp` (single workspace). Approve once in Tilo; MCP grants
  expire within 90 days.
- **API key:** Use a static `tilo_sk_` token for headless scripts, CI, or background agents.

## Keys

An API key starts with `tilo_sk_`. It is displayed once on creation, and the server stores only its
hash. The prefix allows secret scanners to detect accidental commits.

Choose a scope when creating a key:

| Scope | Permissions | Default lifetime |
| :--- | :--- | :--- |
| **All workspaces** | List and create workspaces, mint scoped keys, and invoke actions in any workspace | Valid until revoked (optional expiry) |
| **Single workspace** | Actions in that workspace only; no account-level calls | Up to 90 days |

An account-wide key is intended for long-lived scripts or services and stays valid until you revoke
it. A single-workspace key isolates an agent or integration to one workspace — it cannot list your
other workspaces, create new ones, or mint additional keys.

### Minting, expiration, and revocation

An account-wide key can **mint** narrower single-workspace keys without prompting for interactive
approval, since the parent key already has access to that workspace. A minted key cannot outlive
its parent, and revoking the parent revokes every child key minted from it.

```sh
tilo do account/keys#mint workspace=project-alpha name="Alpha agent"   # prints MCP URL and token
```

Revoke any key via the CLI or on [your grants page](https://mcp.tilo.sh/grants). Revocation
terminates active sessions immediately; subsequent requests fail with `KeyRevoked`
([errors](errors.md)).

### The page scope

By default, a key can drive pages inside a workspace's Browser (navigate, fill fields, select,
click, read back), and keys minted from it inherit that setting. Actions that leave the page —
submitting a form, uploading a file, or entering a password — always require interactive approval
regardless of scope ([approvals](approvals.md#the-hosted-browser)). To require approval for every
browser page action, create it with `browser_pages=false`:

```sh
tilo do account/keys#create name="Read-only agent" workspace=project-alpha browser_pages=false
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
workspace=$(curl -sS --fail-with-body "$api/resources/account/workspaces/actions/create" -H "$auth" -H "Content-Type: application/json" \
  -d '{"name": "One customer"}' | jq -r .result.workspace.id)

# A key that reaches this workspace and nothing else.
minted=$(curl -sS --fail-with-body "$api/resources/account/keys/actions/mint" -H "$auth" -H "Content-Type: application/json" \
  -d "{\"workspace\": \"$workspace\", \"name\": \"Their agent\"}" | jq -r .result)
echo "agent's MCP URL: $(jq -r .mcp_url <<<"$minted")"
agent="Authorization: Bearer $(jq -r .token <<<"$minted")"

# With that key, the agent sees this workspace's apps.
apps=$(curl -sS --fail-with-body "$api/workspaces/$workspace/resources" -H "$agent" | jq '.rows | length')
echo "the agent sees $apps apps"

# Revoking it ends the agent's access at once.
curl -sS --fail-with-body "$api/resources/account/keys/$(jq -r .grant <<<"$minted")/actions/revoke" \
  -H "$auth" -H "Content-Type: application/json" -d '{}' > /dev/null
curl -sS "$api/workspaces/$workspace/resources" -H "$agent" \
  | jq -r '"after revoking: \(.error.code) at \(.error.revoked_at_ms)"'
```

## Where keys live

- **Your grants page**, `https://mcp.tilo.sh/grants`: every key and every agent you let in, with its
  scope, when it was last used, which of your devices approved it, and a Revoke button.
- **The command**: `tilo do account/keys#create name="My backend"` (with `--out <file>` it writes the
  key there instead), `tilo ls account/keys`, `tilo do account/keys#mint workspace=<workspace>`, and
  `tilo do "account/keys/<grant>#revoke"`.
- **The SDKs**: `tilo.keys.list()`, `create()`, `mint()`, `revoke()`, and `workspace.keys.mint()`.
