# Keys, minted grants and the account

A product that runs one agent per customer needs three things from hosted
Tilo: a credential its backend holds without a browser in the loop, a way to
make one workspace per customer and act in it, and a credential narrowed to
that one workspace to hand the customer's agent. This note says how each one
is built from what already exists: grants, the person's certificates, the
relay's own operations and the agent surface's MCP tools. It adds no second
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
`tilo_sk_…`, that lasts as long as the certificate does. That token resolves
to the grant, so every existing rule applies unchanged:

- the scope check at the door and the relay;
- the certificate's expiry, where it has one;
- the person's revocations, checked at every handshake;
- the History attribution of each run;
- revocation at `/grants`, which forgets the key.

### Server keys last until revoked

A key reaching all of a person's workspaces is a server key: it runs a
backend unattended, and a backend whose key ran out every three months would
break silently. So, like Stripe's and AWS's secret keys, it lasts until the
person revokes it. They may choose an expiry instead: `expires_in` (seconds)
on `/keys/new` and `account/keys#create`, `tilo keys create --expires-in`, the
SDKs' `keys.create(..., expires_in)`, or the expiry menu on the grants page.

The certificate says it is a server key itself
(`DeviceCapabilities::server_key`, an extended capability bit under the
grant's signature), because hosts verify it without the door's records.
`verify_device_cert` takes a grant without an expiry, or one longer than 90
days, only with that marker and only reaching all workspaces. Older verifiers
refuse the bit. A grant an agent gets through OAuth consent never carries it
and still expires within 90 days, and so does a key reaching one workspace.

What bounds a leaked server key is revocation, which hosts check at every
handshake and which ends everything it minted. Leaks are made cheap to find
and end:

- the `tilo_sk_` prefix lets secret scanners recognise the token;
- the token is shown exactly once (the door keeps only its digest);
- the keys list (`/grants`, `account/keys#list`, `tilo keys ls`) shows when
  each key was last used and which of the person's devices approved it;
- revoking is one click on `/grants`, or `tilo keys revoke <grant>`.

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
- it expires no later than its issuer, and within 90 days: a minted key
  always expires, even when its issuer is a server key that does not;
- its key is neither the issuer's nor the root's.

The relay verifies every certificate it is shown with
`verify_device_cert_at`, checks the scope (`capabilities.agent`), and checks
the person's revocations, issuer included. A minted grant passes those checks
as one-workspace grants do. It cannot list or make workspaces, and the relay
admits it only to its own workspace.

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
  children are the grant: they never outlive it. The relay checks the
  certificate against the person's revocations, including its issuer, at
  every handshake and audit.

## The account is a resource

Making workspaces and keys are actions on the account, reached through the
same tools as everything else (see [the SDK design](../developers/sdk.md)).
An agent, the SDK and the CLI all use them the same way. In a session at the
door's root (`/mcp`) with a grant reaching all workspaces, a call that names
no `workspace` and whose path is under `account` is answered by the door:

| Path | What it is |
|---|---|
| `account/workspaces#list` | the relay's `room.list` as the grant: the workspaces of the person's own there |
| `account/workspaces#create {name, host?}` | the relay's `room.create-own` as the grant, which then joins it; a person may have any number of workspaces of their own, sharing what the host keeps for them |
| `account/workspaces#delete {workspace}` | refused for now: hosts have no operation that deletes a workspace |
| `account/keys#list` | this key and the keys it minted, each with `expires_at_ms` (`null` until revoked), `last_used_at_ms` and `created_by` |
| `account/keys#create {name, workspace?, expires_in?}` | asks the person; the run is `pending` until they approve, and `tilo_status` then answers the key |
| `account/keys#mint {workspace, name?, expires_in?}` | a minted grant: `{grant, token, mcp_url, expires_at_ms}` |
| `account/keys#revoke {grant}` | this key or one it minted, and everything it minted |

Answers are the agent surface's text (`ok`, `failed: …`, `pending <id>`) and
the same answer typed in `structuredContent`, so programs get JSON.

Everything else is the agent surface `tilo mcp` serves
(`tilo_runtime::mcp::McpServer`), which the door runs for each session over
the grant's own session with the relay: `tilo_ls`, `tilo_read`, `tilo_find`,
`tilo_do`, `tilo_status` and `tilo`. Its actions are asked of the relay's own
device, whose app workers run them with no tab open. File bytes go into the
workspace's content store with `tilo_put_*`, carried over the same session,
and come back out of it with `tilo_get` (`{content, offset?, length?}`,
answering the bytes base64 a piece at a time).
After that, files go through the apps' own actions, which take the
`tilo.content:<hash>` reference.

## How a grant gets into a workspace

The door holds no device of anyone. The grant's own key is the device: it
dials the relay's iroh endpoint with the grant's certificate, as a `tilo
login` computer dials with its own, and enters the way that computer does
(`tilo_session_host::login`).

The relay admits an agent's grant (`tilo_room_host::workspace_access`) as
its person's agent, iff

- its scope reaches the workspace;
- it is an exact member device there: it redeemed an invitation for its
  person, as every new device of theirs does, so the workspace's roster holds
  it under its person;
- in a governed workspace, its person is still a member;
- and the plane is one an agent works on (subscribe, state, publish, replay,
  channel access, remote actions, the live plane, content discovery), never
  compute, environments or governed membership, and never by attachment or a
  same-owner host's enrolment.

What it does there is its person's agent's: the relay makes its caller the
agent a device of the person handed its session to
(`Caller::delegated_to`), so an agent's role cap holds it at Editor at most,
and it governs, invites and administers nothing. The relay's own operations
treat it as its person within its scope (`host_authority`): a grant reaching
all workspaces makes and lists the person's own; nothing else.

**A grant reaching all the person's workspaces** goes through the site's door
to the site's home workspace, as the person's page does (the door the site's
`entry.json` names for the relay), asks the relay there for the person's own
workspace (`room.create-own`), which answers with an invitation for the
person, redeems it and joins.

**A minted grant** reaches one workspace and never the home workspace. Its
parent, which reaches all of them, asks for the invitation into that
workspace (`room.create-own` naming it, which answers only the person's own
workspace and never makes another), and the minted grant redeems it itself.
The parent asks rather than the relay admitting the minted grant on its
person's membership alone because a workspace's roster admits devices one by
one, each by an invitation: the minted grant enters exactly as every device
of the person does, and its certificate's chain is checked at that join and
at every handshake after.

A grant the person certified for one workspace directly (an agent pointed at
`/w/<workspace>`) has no parent to ask, and is not admitted yet.
