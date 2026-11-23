# Keys, minted grants and the account

A product that runs one agent per customer needs three things from hosted
Tilo: a credential its backend holds without a browser in the loop, a way to
make one workspace per customer and act in it, and a credential narrowed to
that one workspace to hand the customer's agent. This note says how each one
is built from what already exists: grants, the person's certificates, the
server's own operations and the agent surface's MCP tools. It adds no second
authority and no second action model.

## API keys are grants

A grant is a key the API server made, certified by the person's root as an
agent's grant (`AgentScope::AllWorkspaces` or one workspace) and kept by the
entry. An agent reaches it through OAuth today: it registers, the person
approves in their own Tilo, and the agent redeems a code for tokens.

An API key is the same grant with the OAuth client taken out. The person
asks for one (the grants page, or `tilo do account/keys#create`), the API server makes the key,
and the person's own Tilo signs them in and certifies the key exactly as it
does for an agent (`/sign-in`, the same proof and certificate). The person's
device is still the only thing that certifies. What changes is what comes
back. Instead of a code for a client, the API server issues one bearer token,
`tilo_sk_…`, that lasts as long as the certificate does. That token resolves
to the grant, so every existing rule applies unchanged:

- the scope check at the API server and the server;
- the certificate's expiry, where it has one;
- the person's revocations, checked at every handshake;
- the History attribution of each run;
- revocation at `/grants`, which forgets the key.

### Server keys last until revoked

A key reaching all of a person's workspaces is a server key: it runs a
backend unattended, and a backend whose key ran out every three months would
break silently. So, like Stripe's and AWS's secret keys, it lasts until the
person revokes it. They may choose an expiry instead: `expires_in` (seconds)
on `/keys/new` and `account/keys#create`, `tilo do account/keys#create expires_in=…`, the
SDKs' `keys.create(..., expires_in)`, or the expiry menu on the grants page.

The certificate says it is a server key itself
(`DeviceCapabilities::server_key`, an extended capability bit under the
grant's signature), because hosts verify it without the API server's records.
`verify_device_cert` takes a grant without an expiry, or one longer than 90
days, only with that marker and only reaching all workspaces. Older verifiers
refuse the bit. A grant an agent gets through OAuth consent never carries it
and still expires within 90 days, and so does a key reaching one workspace.

What bounds a leaked server key is revocation, which hosts check at every
handshake and which ends everything it minted. Leaks are made cheap to find
and end:

- the `tilo_sk_` prefix lets secret scanners recognise the token;
- the token is shown exactly once (the API server keeps only its digest);
- the keys list (`/grants`, the keys record `record:tilo.account:keys`, `tilo ls account/keys`) shows when
  each key was last used and which of the person's devices approved it;
- revoking is one click on `/grants`, or `tilo do "account/keys/<key>#revoke"` (a key by its name or grant).

`tilo do account/keys#create` cannot open a browser on a server, so it follows the
device-authorization pattern. It asks the API server for an approval link and a poll
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

The server verifies every certificate it is shown with
`verify_device_cert_at`, checks the scope (`capabilities.agent`), and checks
the person's revocations, issuer included. A minted grant passes those checks
as one-workspace grants do. It cannot list or make workspaces, and the server
admits it only to its own workspace.

### Revocation cascades

There are two places a grant ends, and both cascade:

- **At the API server**: revoking a grant (at `/grants`, `account/keys/<key>#revoke`,
  or `tilo do "account/keys/<grant>#revoke"`) revokes every grant it minted, forgets their keys,
  invalidates their tokens and ends their sessions. The API server holds the keys,
  so nothing can act with them afterwards.
- **In the identity**: a revocation the person's root signs for the parent
  key refuses every certificate that key issued, at any time
  (`RevocationList::refuses`). A stand-in's revocation counts only from its
  time, because the devices it vouched for are the person's own. A grant's
  children are the grant: they never outlive it. The server checks the
  certificate against the person's revocations, including its issuer, at
  every handshake and audit.

## The account is a resource

Making workspaces and keys are actions on the account, reached through the
same tools as everything else (see [the SDK design](../developers/sdk.md)).
The API runs them as it runs any action ([`api.md`](../developers/api.md)):
`POST /v1/resources/account/<resource>/actions/<action>`, which `tilo do` calls.
Every route is the same call an agent makes. In a session at the
entry's root (`/mcp`) with a grant reaching all workspaces, a call that names
no `workspace` and whose path is under `account` is answered by the API server:

| Path | What it is |
|---|---|
| `account/workspaces#list` | the server's `workspace.list` as the grant: the workspaces of the person's own there |
| `account/workspaces#create {name, host?}` | the server's `workspace.create-own` as the grant, which then joins it; a person may have any number of workspaces of their own, sharing what the host keeps for them |
| `account/workspaces#branch {workspace, name, labels?, id?}` | the server's `workspace.branch-own` as the grant, for one of the person's own: a new workspace of theirs holding the source's records with their history and the content they name, apart from it from then on ("Branching a workspace" in `workspace.md`). Answers `{workspace}` once the branch holds everything; `id` is the branch's, so a retry answers the same one. One not theirs is `not_found` |
| `account/workspaces#delete {workspace}` | the server's `workspace.delete` as the grant, for one of the person's own: final. The server tells everyone in it it was deleted, lets nobody in again, removes its record, retained state, history and content, and releases its storage; the API server then revokes every key that reached only it. Answers `{deleted, revoked_keys}`; one not theirs, or gone already, is `not_found`. The call is the confirmation |
| `record:tilo.account:keys` (a record, read with `tilo_read`, followed with `after`) | this key and the keys it minted, each with `expires_at_ms` (`null` until revoked), `last_used_at_ms` and `created_by`; no action lists them |
| `account/keys#create {name, workspace?, expires_in?}` | asks the person; the run is `pending` until they approve, and `tilo_status` then answers the key |
| `account/keys#mint {workspace, name?, expires_in?}` | a minted grant: `{grant, token, mcp_url, expires_at_ms}` |
| `account/keys/<key>#revoke` (a key by its name or grant) | this key or one it minted, and everything it minted |

Answers are the agent surface's text (`ok`, `failed: …`, `pending <id>`) and
the same answer typed in `structuredContent`, so programs get JSON.

Everything else is the agent surface `tilo mcp` serves
(`tilo_mcp::McpServer`), which the API server runs for each session over
the grant's own session with the server: `tilo_ls`, `tilo_read`, `tilo_find`,
`tilo_do`, `tilo_status` and `tilo`. Its actions are asked of the server's own
device, whose app workers run them with no tab open. File bytes go into the
workspace's content store with `tilo_put_*`, carried over the same session,
and come back out of it with `tilo_get` (`{content, offset?, length?}`,
answering the bytes base64 a piece at a time).
After that, files go through the apps' own actions, which take the
`tilo.content:<hash>` reference.

## How a grant gets into a workspace

The API server holds no device of anyone. The grant's own key is the device: it
dials the relay's iroh endpoint with the grant's certificate, as a `tilo
login` computer dials with its own, and enters the way that computer does
(`tilo_session_client::device`).

The server keeps one session per device: a second session of the same key cuts
the first off. So each holder keeps one session and every run shares it. The
API server holds one per grant for all of that grant's MCP sessions. A `tilo
login` computer holds one per device: the first `tilo mcp` starts the daemon,
`tilo-runtime`, which enters the workspace, and every `tilo mcp` on the computer (each
agent window, `tilo agent check`) connects to it over a socket only that
account can open. Each connection is its own agent run over that one session.
The holder leaves the workspace ten seconds after its last connection ends.

The server admits an agent's grant (`tilo_workspace_service::workspace_access`) as
its person's agent, iff

- its scope reaches the workspace;
- it is an exact member device there: it redeemed an invitation for its
  person, as every new device of theirs does, so the workspace's roster holds
  it under its person;
- in a governed workspace, its person is still a member;
- and the protocol is one an agent works on (subscribe, state, publish, replay,
  channel access, remote actions, the live session, content discovery), never
  compute, environments or governed membership, and never by attachment or a
  same-owner host's enrolment.

What it does there is its person's agent's: the server makes its caller the
agent a device of the person handed its session to
(`Caller::delegated_to`), so an agent's role cap holds it at Editor at most,
and it governs, invites and administers nothing. The server's own operations
treat it as its person within its scope (`host_authority`): a grant reaching
all workspaces makes and lists the person's own; nothing else.

A grant enters a workspace as every device of the person does
(`Device::enter_workspace`): on its session with the server, in no workspace,
it asks the server for the invitation into that workspace (`workspace.create-own`
naming it), redeems the invitation on the same connection, and joins. A grant reaching all the person's workspaces may
also make one; a minted grant, or one certified for one workspace directly,
enters only its own and makes none. The server asks for no parent and no site
workspace first: a workspace's roster admits devices one by one, each by an
invitation, and the grant's certificate chain is checked at that join and at
every handshake after.

A workspace of one's own is for a person the site let in. The server names its
site's home workspace (`Workspaces::admit_own_through`), and makes a new
workspace only for its own person, a member of the home workspace (its entry or
an invitation admitted them), a person an invitation into it still names, or a
person who already keeps workspaces of their own there. Every key and agent of
that person is the person, within its scope. Anyone else, a fresh root or any
key it certified, is refused with the API server's own refusal (`EntryRefused`, read
back as `DeviceRefusal::entry`), which offers the wait list when the API server takes
people by invitation. Entering a workspace a person was invited into stays
theirs either way, and so does keeping their workspace list once they are a
member of any workspace there.
