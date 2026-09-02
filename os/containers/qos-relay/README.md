# qOS relay-only home server

This Compose file runs the supported, headless `qos-relay` deployment on a NAS
or home server. It is an always-on authenticated workspace relay: it keeps the
workspace CRDT, forwards updates between connected peers, and persists updates
for an offline peer until that peer returns.

It is deliberately **not** a full NAS mirror. This image does not start the
qOS filesystem/blob host plane, so it does not retain file bytes, ebook/media
assets, or full-resolution previews. It also does not run app backends, compute
jobs, or remote-control tunnelling. The relay does retain CRDT workspace state
such as open workspace layout and app state that the existing collaboration
replica publishes. File thumbnails and other file data are not promised by this
deployment; they require the future filesystem host plane and an explicit
residency policy.

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

Do not configure `full_content_replication`, `app_serving`, `compute`, or
`remote_control_relay` for this container. The standalone daemon clamps those
advertisements off even if a hand-edited configuration requests them. A future
composite NAS deployment will start and health-check the filesystem/blob host
before enabling full file residency.
