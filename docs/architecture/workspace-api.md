# Keys, minted grants and the account

A product that runs one agent per customer needs three things from hosted
Tilo: a credential its backend holds without a browser in the loop, a way to
make one workspace per customer and act in it, and a credential narrowed to
that one workspace to hand the customer's agent. This note says how each one
is built from what already exists: grants, the person's certificates, the
host's own operations and the runtime's MCP tools. It adds no second
authority and no second action model.

## API keys are grants

A grant is a key the front door made, certified by the person's root as an
agent's grant (`AgentScope::AllWorkspaces` or one workspace) and kept by the
door. An agent reaches it through OAuth today: it registers, the person
approves in their own Tilo, and the agent redeems a code for tokens.

An API key is the same grant with the OAuth client taken out. The person
asks for one (the grants page, or `tilo keys create`), the door makes the key,
and the person's own Tilo signs them in and certifies the key exactly as it
does for an agent (`/sign-in`, the same proof and certificate). The person's
device is still the only thing that certifies. What changes is what comes
back. Instead of a code for a client, the door issues one bearer token,
`tilo_sk_…`, that lasts as long as the certificate does (89 days at most).
That token resolves to the grant, so every existing rule applies unchanged:

- the scope check at the door, the host and the runtime;
- the certificate's expiry;
- the person's revocations, checked at every handshake;
- the History attribution of each run;
- revocation at `/grants`, which forgets the key.

`tilo keys create` cannot open a browser on a server, so it follows the
device-authorization pattern. It asks the door for an approval link and a poll
secret, prints the link, and polls until the person approves. The key goes
only to the poller; the browser shows "approved". A key created from the
grants page itself is shown on the page once.

## Minted grants: delegation in the certificate chain

A backend holding an all-workspaces key mints a one-workspace grant for each
customer's agent. The person is not asked again. Their all-workspaces grant
already reaches that workspace, and the minted grant reaches strictly less.

This is a certificate the parent grant's key signs, carrying the parent's
root-signed certificate as its `issuer`. That is the one-level chain
`DeviceCert::issuer` already has for a person's stand-in approver, and it is
signed under the same `tilo-devcert-v6` preimage, which binds the whole issuer
certificate. `verify_device_cert` takes such a certificate only when all of
the following hold:

- the issuer is an agent's grant reaching **all** workspaces, root-signed,
  with no issuer of its own (so the chain is never longer than one step);
- the issuer is the same person's, and was valid when it issued;
- the certificate is an agent's grant for **one** workspace and nothing else;
- it expires no later than its issuer;
- its key is neither the issuer's nor the root's.

Hosts and runtimes already verify every certificate they are shown with
`verify_device_cert_at`, check the scope (`capabilities.agent`), and check the
person's revocations. A minted grant passes those checks as one-workspace
grants do. It cannot list or make workspaces, and the host shows it only its
own workspace.

### Revocation cascades

There are two places a grant ends, and both cascade:

- **At the door**: revoking a grant (at `/grants`, `account/keys#revoke`,
  or `tilo keys revoke`) revokes every grant it minted, forgets their keys,
  invalidates their tokens and ends their sessions. The door holds the keys,
  so nothing can act with them afterwards.
- **In the identity**: a revocation the person's root signs for the parent
  key refuses every certificate that key issued, at any time
  (`RevocationList::refuses`). A stand-in's revocation counts only from its
  time, because the devices it vouched for are the person's own. A grant's
  children are the grant: they never outlive it. The runtime's admission
  checks the certificate against the person's revocations, including its
  issuer.

## The account is a resource

Making workspaces and keys are actions on the account, reached through the
same tools as everything else (see [the SDK design](../developers/sdk.md)).
An agent, the SDK and the CLI all use them the same way. In a session at the
door's root (`/mcp`) with a grant reaching all workspaces, a call that names
no `workspace` and whose path is under `account` is answered by the door:

| Path | What it is |
|---|---|
| `account/workspaces#list` | the hosts' `room.list` as the grant |
| `account/workspaces#create {name, host?}` | the host's `room.create` as the grant; the person's runtime joins |
| `account/workspaces#delete {workspace}` | refused for now: hosts have no operation that deletes a workspace |
| `account/keys#list` | this key and the keys it minted |
| `account/keys#create {name, workspace?}` | asks the person; the run is `pending` until they approve, and `tilo_status` then answers the key |
| `account/keys#mint {workspace, name?, expires_in?}` | a minted grant: `{grant, token, mcp_url, expires_at_ms}` |
| `account/keys#revoke {grant}` | this key or one it minted, and everything it minted |

Answers are the agent surface's text (`ok`, `failed: …`, `pending <id>`) and
the same answer typed in `structuredContent`, so programs get JSON.

Everything else goes to the workspace's own runtime, as the grant: `tilo_ls`,
`tilo_read`, `tilo_find`, `tilo_do`, `tilo_status` and `tilo`. File bytes go
into the workspace's content store with the runtime's `tilo_put_*` tools.
After that, files go through the apps' own actions, which take the
`tilo.content:<hash>` reference.
