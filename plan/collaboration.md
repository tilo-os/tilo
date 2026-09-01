# Collaboration architecture

qOS uses one collaboration model across direct peer-to-peer sessions, a
user-owned home server, and a hosted workspace server. Iroh is the only
network transport. The placement of a peer changes availability and storage,
not identity or document semantics.

## Identity

First setup creates two related Ed25519 identities:

- A human root, stored as the personal `primary.json` signing identity and as
  `/var/lib/qos/user-identity/{privkey,pubkey}.bin` on the primary device.
- One device key at `/var/lib/qos/device/keypair.bin`, certified by the human
  root in `/var/lib/qos/device/cert.bin`.

The device key is also the Iroh `EndpointId`. The shell, filesystem sync, and
workspace host therefore present the same device identity. A home or hosted
server receives a separate device certificate with `can_act_as_host`; it never
receives the human root secret.

Existing installations are migrated idempotently by `shell
--provision-identity`. Existing keys are checked for agreement and are never
silently replaced.

Native qOS and the legacy-host compatibility route deliberately have different
trust models. A normal qOS installation creates identity through the dedicated
OOBE and requires a LUKS2-encrypted system before that identity is created.
TPM2 machines use one required startup PIN bound to the TPM; machines without
TPM2 require a normal disk passphrase. That same credential is the local
session-unlock credential, and successful disk authentication hands directly
to the installed user's first session rather than immediately prompting for a
second password. Native provisioning remains responsible for protected-key
migration and does not infer a person's identity from the implementation-level
Unix account.

Only when qDesktop is installed on top of an existing Linux distribution does
identity associate with the already-authenticated Unix login. Its files live in
that user's XDG trees with ordinary Unix ownership, and a newly-created identity
takes its public display label from the account's passwd entry. This route does
not create a second login or identity password, and it does not rewrite or
migrate a pre-existing identity record.

## Direct collaboration

`shell/src/network/actor.rs` owns an Iroh endpoint and one `iroh-gossip` topic
per workspace. An invite contains the inviter's authenticated `EndpointAddr`,
workspace room, expiry, and an Ed25519 signature. Iroh chooses a direct path
when possible and uses its encrypted relay path when NAT traversal cannot
establish one.

Direct collaboration has no server requirement. It is the fast path for an
ad-hoc invitation and remains active when a workspace host is configured.

An invite also signs the maximum role its bearer receives: Viewer,
Commenter, or Editor. The inviter chooses that role before the token is
minted; changing it invalidates the signature. After the joining device
presents its person-signed device certificate, the workspace authority turns
that signed choice into the ordinary room-membership capability. From then on
the same role checks gate app actions whether peers connect directly, through
a home host, or through hosted infrastructure.

## Workspace hosts

`crates/hosting/qos-relay` is the persistent server tier. Despite the retained
binary name, it is an authenticated Iroh workspace host, not a transport
relay. Peers prove possession of certified device keys on the `qos-relay/1`
ALPN before subscribing.

The shell can attach a host with `ShellNetwork::connect_host`. For unattended
configuration, `QOS_COLLAB_HOST` accepts an Iroh endpoint id or the JSON
representation of a complete `EndpointAddr`. Durable messages travel to both the direct gossip mesh and the
host. The host persists and store-and-forwards them; presence stays ephemeral.
Multiple hosts federate through the same authenticated Iroh protocol.

This gives three deployments without three protocols:

1. Direct invitation: peers exchange an invite and collaborate over Iroh.
2. Home server: a certified always-on device pins workspaces and bridges
   offline periods.
3. Hosted server: the same daemon and protocol run on infrastructure operated
   for the user.

## Wire boundaries

- Direct fan-out: deterministic `iroh-gossip` topic derived from workspace id.
- Persistent host: `qos-relay/1`, mutually authenticated with device certs.
- Content: `iroh-blobs` on the filesystem endpoint.
- Messages: postcard-encoded `NeighbourMsg`; non-ephemeral messages are stored
  by the host as opaque co-edit deltas.

Automerge remains the convergence layer. Transport events never decide
authority: room governance, signed operations, device certificates, and
revocations are validated above Iroh.

## Operational rules

- Losing reachability must not create a new identity.
- An unreadable or mismatched key/certificate is a hard error.
- Invites bind the complete Iroh address to the signing endpoint id.
- A host is optional for correctness but required for 24/7 availability and
  offline store-and-forward.
- The legacy standalone transport-relay crate and all libp2p dependencies are
  removed.
