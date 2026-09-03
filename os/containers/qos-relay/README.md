# qOS composite home peer

This Compose file turns a NAS or home server into an always-on authenticated
workspace and filesystem peer. It stores offline updates and complete file
content for the workspaces selected in Settings.

## Before starting

On a trusted qOS device, create a home-server enrollment token. Put it in
`enrollment-token.txt` next to `compose.yaml`, then create the persistent
directories:

```text
mkdir -p identity config
chmod 700 identity config
```

The token is short-lived. The container creates its own device key; no user-root
private key is copied to the server.

## Start

```sh
podman compose up -d
```

Open Settings → Devices & Sync on the trusted qOS device and choose
**Pair home server**. The devices find each other on the LAN. The signed
certificate, selected workspace IDs, file ticket, and signed live authority
arrive as one authenticated provisioning response. The container writes its
configuration and starts both replica services automatically. Remove
`enrollment-token.txt` after pairing.

The named volumes are essential: they hold the stable Iroh
endpoint secret, CRDT snapshots, pending offline queues, revocations, and
metrics. Back it up together with the non-secret `relay.toml`; protect the
identity directory separately. Replacing either volume changes availability
properties, so restore rather than recreate them after a failure.

The health check reports ready only when the relay is live and the full file
replica has completed its current work.

## Trust and collaborators

The generated configuration uses `allow_matching_user`, so the owner's other
certified devices connect automatically. Workspace authority travels signed by
the owner identity and remains enforced by the filesystem replica. Inspect the
running state with:

```sh
podman compose exec qos-relay qos-relay-admin --config /etc/qos/relay.toml status
```

The aggregate replica status is the content-capacity health check:
`full_content_replication` is advertised only while every configured workspace
has a fresh complete status. App, compute, and remote-control services remain
separate from filesystem replication.
