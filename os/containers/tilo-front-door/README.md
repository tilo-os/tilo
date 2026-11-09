# Tilo front door

The MCP endpoint of hosted Tilo (`crates/hosting/tilo-front-door`): the URL an
agent is told to add, with OAuth 2.1 for agents, sign-in with one of the
person's devices, and routing of each session to the runtime of the
workspace's host. It runs in the same namespace as the tilo.sh relay, behind
the relay's Caddy, which terminates TLS; the door serves plain HTTP.

## What the pod needs

- **No privilege**: the image runs as uid/gid 65532, and the container needs no
  capability.
- **Outbound reach**: the relay's origin over HTTPS, and its iroh endpoint over
  UDP (QUIC), for the MCP sessions it carries. Nothing else is exposed.
- **A writable volume at `/state`**: `TILO_DOOR_STATE` names one private file
  there, holding the registered clients and the grants people gave them, and
  the door writes nothing else — the root filesystem can stay read-only.
- **The environment**, which is the whole configuration; the image holds
  none of it, so a changed value needs no new image:

  | | |
  | --- | --- |
  | `TILO_DOOR_LISTEN` | `HOST:PORT` to listen on, e.g. `0.0.0.0:8080` |
  | `TILO_DOOR_ISSUER` | the origin agents reach, e.g. `https://mcp.tilo.sh` |
  | `TILO_DOOR_WEB_ENTRY` | the person's Tilo on the web, e.g. `https://tilo.sh` |
  | `TILO_DOOR_RELAY` | the public origin of the relay their workspaces live on, e.g. `https://eu1.tilo.sh` |
  | `TILO_DOOR_STATE` | the state file, e.g. `/state/door.json` |

| | |
| --- | --- |
| user | 65532 (distroless `nonroot`) |
| port | 8080/tcp, plain HTTP |
| volume | `/state` |
| health | `GET /health` answers `{"ok":true}` |
