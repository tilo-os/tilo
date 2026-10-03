# Collaboration architecture

Tilo uses one collaboration model across direct peer-to-peer sessions, a
user-owned home server, and a hosted workspace server. Iroh is the only
network transport. The placement of a peer changes availability and storage,
not identity or document semantics.

## Identity

First setup creates two related Ed25519 identities:

- A human root, stored as the personal `primary.json` signing identity and as
  `/var/lib/tilo/user-identity/{privkey,pubkey}.bin` on the primary device.
- One device key at `/var/lib/tilo/device-private/keypair.bin`, certified by the human
  root in `/var/lib/tilo/device/cert.bin`.

The device key is also the Iroh `EndpointId`. The shell, filesystem sync, and
workspace host therefore present the same device identity. A home or hosted
server receives a separate device certificate with `can_act_as_host`; it never
receives the human root secret. A company node runs a session beside its
relay for the person's apps and agents: that session has a key of its own,
which the node vouches for and the root certifies at install as one of the
person's devices (never a host), so it reads and writes the node's rooms as
an ordinary channel member.

The same chain can endorse a self-built OS image without exporting the human
root: `ImageEndorsement` binds an immutable OCI SHA-256 manifest digest and Tilo
generation to a root-certified device with a separate, time-bounded,
root-signed image-signing authorization. Device enrollment authority alone is
insufficient. Verification checks both grants and then the device's
domain-separated Ed25519 image signature. Mutable tags are never a signing
input. Official releases additionally retain their transparency-logged
Cosign signature; the identity endorsement expresses owner trust, not a bypass
of release-channel policy.

Existing installations are migrated idempotently by `shell
--provision-identity`. Existing keys are checked for agreement and are never
silently replaced.

Native Tilo and the legacy-host compatibility route deliberately have different
trust models. A normal Tilo installation creates identity through the dedicated
OOBE and requires a LUKS2-encrypted system before that identity is created.
Machines with TPM2 use one required startup PIN only after a dedicated preflight
proves verified boot continues from the live medium through the installed
target. Secure Boot state on the installer alone is not enough. Machines
without every part of that boundary require one normal disk passphrase. That
same credential is the local
session-unlock credential, and successful disk authentication hands directly
to the installed user's first session rather than immediately prompting for a
second password. Native provisioning remains responsible for protected-key
migration and does not infer a person's identity from the implementation-level
Unix account.

Only when Tilo Desktop is installed on top of an existing Linux distribution does
identity associate with the already-authenticated Unix login. Its files live in
that user's XDG trees with ordinary Unix ownership, and a newly-created identity
takes its public display label from the account's passwd entry. This route does
not create a second login or identity password, and it does not rewrite or
migrate a pre-existing identity record.

## Direct collaboration

`crates/session/shell/src/network/actor.rs` owns an Iroh endpoint and one `iroh-gossip` topic
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
that signed choice into the ordinary room-membership capability. Redemption
carries a hash of the exact signed invite, so multiple outstanding invites
cannot exchange names or roles and an ambient room participant cannot consume
the next grant. From then on the same role checks gate app actions whether
peers connect directly, through a home host, or through hosted infrastructure.

Every inbound room message is authorized after device-certificate
authentication. Membership with `Read` is required to request or receive room
state and presence; publishing room mutations requires `Write`. The sole
bootstrap exception accepts the first governance snapshot only from the exact
authority endpoint signed into the invite. Topic ids are treated as lossy
routing hints, so a collision is rejected rather than selecting an arbitrary
room.

Only a locally governing administrator can mint an invite or change membership
and capabilities. Ordinary role controls expose Viewer, Commenter, and Editor;
Owner is structural governance authority and is never created or transferred
by cycling a role chip. What a role may change depends on the path the change
takes:

- **Directed action invocations** (an action invoked through the session action
  broker by a person, script or agent) are checked against the action's
  declared permission requirement — `Read`, `Comment`, `Write`, `Invite` or
  `Admin` — and the caller's live capabilities.
- **Replicated app documents are checked only as a whole.** The SDK signs every
  document change, snapshot or incremental and whatever action produced it, as
  `OpClass::Splice`, which requires `Write`. The identity service checks the
  role table and live capability restrictions before signing, and every broker
  checks them again on receipt. Owners and Editors can therefore change any
  part of an app document; Commenters and Viewers can change none of it, so a
  comment stored inside an app document is refused for a Commenter; and an
  Editor is not limited to particular actions.
- **Governance, compaction and redaction operations** are signed with their own
  classes, which the role table grants to the owner only.

Per-action enforcement does not exist for replicated app state, and signing the
action's declared class would not create it: the broker holds no Automerge
document, the payload may be MLS ciphertext, and an Automerge change does not
name the action that made it, so a Commenter could label any edit as a comment.
Enforcing actions such as "commenters may add comments" needs structure the
receiver can check — for example comments kept as their own records that
accept only `Comment`-class changes. Until then, treat the app-document role
split as write or no write.

Publishing a surface stream, granting or injecting remote control, and room
state responses likewise require `Write`; passive presence, requests, and
validated history reads require `Read`.

Live session handles in cursor, app-presence, remote-input, stream, and
scrollback messages are bound to the authenticated transport endpoint before
dispatch. A member therefore cannot impersonate another member merely by
copying that member's numeric session handle.

## Workspace hosts

`crates/hosting/tilo-relay` is the persistent server tier. Despite the retained
binary name, it is an authenticated Iroh workspace host, not a transport
relay. Peers prove possession of certified device keys on the `tilo-relay/1`
ALPN before subscribing.

The shell can attach a host with `ShellNetwork::connect_host`. For unattended
configuration, `TILO_COLLAB_HOST` accepts an Iroh endpoint id or the JSON
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
- Persistent host: `tilo-relay/1`, mutually authenticated with device certs.
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
