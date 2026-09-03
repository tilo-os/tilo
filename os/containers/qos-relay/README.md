# qOS composite home peer

This Compose file runs the supported, headless `qos-relay` deployment on a NAS
or home server. It is an always-on authenticated workspace relay and complete
filesystem peer: it keeps the workspace CRDT, persists offline updates, and
hydrates every manifest, chunk, and thumbnail in each `workspaces[]` entry of
the transferred `/etc/qos/home-peer.toml` bundle.

## Before starting

Use a dedicated, already-enrolled host device identity. Create this local
directory structure next to `compose.yaml`:

```text
identity/
├── device/keypair.bin
├── device/cert.bin
└── user-identity/pubkey.bin
```

The device certificate must be valid for the same user root and have the qOS
host capability. The directory contains a device private key, so restrict it
to the service account (`chmod -R go-rwx identity`) and do not commit or copy
it into an image. Do not place `primary.json` or any user-root private key in
this directory.

`qos-relay` intentionally does not enroll devices or mint certificates. Enrol
the NAS from a trusted qOS device through the normal identity flow, then copy
only the three artifacts above through an operator-controlled secure channel.
The host image's first-boot enrollment helper is not an alternative: it is
currently incomplete and must not be used to fabricate an identity.

## Start

```sh
cp relay.toml.example relay.toml
# Transfer the exported bundle as ./home-peer.toml.
podman compose up -d
podman compose logs -f qos-relay
```

The startup log contains the stable Iroh node ID/endpoint address. Configure
that endpoint on a qOS desktop with `QOS_COLLAB_HOST` (or
`QOS_COLLAB_HOSTS`), then create invitations as usual. New invitations include
the configured host address, so a collaborator can reconnect through this
relay while the inviter's desktop is offline.

The named volume `qos-relay-state` is essential: it holds the stable Iroh
endpoint secret, CRDT snapshots, pending offline queues, revocations, and
metrics. Back it up together with the non-secret `relay.toml`; protect the
identity directory separately. Replacing either volume changes availability
properties, so restore rather than recreate them after a failure.

`metrics.json` only proves that the local daemon started. Verify end-to-end
reachability from an enrolled client; NAT, UDP filtering, and relay fallback
are outside a container health check.

## Trust and collaborators

The example uses `allow_matching_user`: it is ready for the owner's phones,
laptops, and other certified devices. For a collaborator with a different
qOS identity, do not weaken this to `allow_any` casually. Use an explicit
cross-user host policy and bind the workspace with the permitted device
members using `qos-relay-admin`; membership remains enforced independently of
transport trust. This release does not automate that control-plane enrollment,
so keep the default unless the operator has completed that existing procedure.

Workspace bindings written by `qos-relay-admin` are durable, but changing
`relay.toml` itself requires a container restart at present:

```sh
podman compose exec qos-relay qos-relay-admin --config /etc/qos/relay.toml status
podman compose restart qos-relay
```

The aggregate replica status is the content-capacity health check:
`full_content_replication` is advertised only while every configured workspace
has a fresh complete status. App, compute, and remote-control services remain
separate from filesystem replication.
