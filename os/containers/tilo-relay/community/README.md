# Tilo community relay

The relay that hosts the **Tilo Community** room: the `tilo-relay` image in
room-host mode (`TILO_HOST_KIND=room-host`). It serves the public community
room it was delegated and each reporter's private room with the Tilo team,
and nothing else: no Files replica, no home-peer role. Any certified device
may connect; every room is gated by its own membership, and the relay can
admit Commenters and do nothing more (`docs/architecture/feedback.md`).

This directory is deployable but deploys nothing by itself:

| File | What |
|---|---|
| `kustomization.yaml` + `namespace.yaml`, `statefulset.yaml`, `service.yaml`, `network-policy.yaml` | A Kustomize base in the infra repository's `apps/<app>` convention |
| `compose.yaml` | The same container on one machine with Podman |

## Inputs

`scripts/community-ops/tilo-community up` produces all of them in
`$TILO_COMMUNITY_CUSTODY/deploy` (`tilo-community bundle` rewrites them):

- `identity/` — the relay's own paired identity (`device-keypair.bin`,
  `device-cert.bin`, `user-pubkey.bin`). A secret. It is a room-host
  certificate issued by the team's root; the team's root itself never leaves
  the team's desktop. The relay's Iroh endpoint id derives from this key,
  so keeping it keeps the address every invitation names.
- `records/<room>.json` — what the relay hosts: the room, its owner, the
  team's signed delegation to this relay and the standing invitations with
  their revocations. Public. Installed with `tilo-relay-admin community
  install` on every start; a revocation in either the record or the relay's
  state is never undone.
- `endpoint.json`, `community.toml` — for reference; `community.toml` is
  also written to `os/usr/share/tilo/community.toml` for the image and the
  packages.

## Kubernetes

A cluster overlay (in the infra repository,
`clusters/<cluster>/apps/tilo-community-relay/`) references this base and
adds:

- `relay-identity.sops.yaml` — Secret `tilo-community-relay-identity` from
  `deploy/identity`, SOPS-encrypted, listed by a sibling `ksops.yaml`;
- a `configMapGenerator` for `tilo-community-records` with `behavior:
  replace` and the `deploy/records/*.json` files;
- the image by digest (the `tilo-relay.yaml` workflow publishes
  `registry.tilo.so/tilo/tilo-relay`);
- the load balancer's static address and provider annotations on Service
  `tilo-community-relay` (UDP 4433) and the storage class of the `state`
  volume claim;
- a Prometheus scrape job for the pod's `metrics` port (9464, `/metrics`).

The pod runs as non-root with a read-only root filesystem. The identity is
copied from the Secret into a memory volume with owner-only modes at start,
`relay.toml` is derived from the environment on every start, and the
records are installed before the relay serves. `/metrics` answers only once
all of that succeeded, so it is also the readiness probe.

One replica only: a second pod would be a second device the team never
paired. The `state` volume holds the room's retained checkpoints, admitted
memberships and offline queues; back it up and restore rather than recreate.

## Podman

Put `identity/` (mode 0700) and `records/` from the bundle next to
`compose.yaml` and run `podman compose up -d`. UDP 4433 must reach the
machine at the address the invitation names; metrics stay on
`127.0.0.1:9464`.

## Operating

```sh
tilo-community status --relay-exec "kubectl -n tilo-community exec tilo-community-relay-0 --"
tilo-community rotate-invite     # new invitation beside the old; ship it
tilo-community revoke-invite ID  # then redeploy the records
```

`tilo-community guide` is the whole procedure and `tilo-community decisions`
lists what only the team can decide: whose identity owns the room, where its
root lives, where this relay runs and its public address.

The governance acts are actions of the team desktop, in a release build:
"Create a public room", "Delegate this public room to its host", "Mint this
public room's standing invitation", "Revoke a standing invitation" and "Make
someone an Editor of this public room" (Admin, confirmation required,
re-checked when they run; `shell::public_room_governance`). The palette
offers them to the team, and the action broker offers the same actions to
agents and peers holding Admin in the room. Delegating records the relay's
endpoint in the room's own governance, so every invitation the room mints
names its host without any session configuration. `tilo-community` names
each act when it is due, then checks what it produced — the invitation's
room, owner and relay, the relay's record — before going on.

Two ways for the operator tool itself to perform those acts later, each
needing a decision first:

- **(A)** The compositor serves the MCP projection of its own action broker
  (`runtime_host::mcp::McpServer` over the session's `BrokerActionHost`) on an
  owner-only socket, opted into per session. It adds a same-user action
  surface to release builds, which is why it is not there.
- **(B)** The operator tool runs as a launcher-launched agent (the
  `apps/agent` pattern) that the team grants an Admin role in the community
  room (`profile.assign`), reaching the actions through the broker door.
  Creating the room stays a person's act.

Inside the container, `tilo-relay-admin --config /etc/tilo/relay.toml
community status --room ROOM` shows what is hosted and `tilo-relay-admin
--config /etc/tilo/relay.toml endpoint` the relay's endpoint.
