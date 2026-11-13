# Tilo API server

The MCP endpoint of hosted Tilo (`crates/hosting/tilo-api`): the URL an
agent is told to add, with OAuth 2.1 for agents, sign-in with one of the
person's devices, and routing of each session to the runtime of the
workspace's host. It runs in the same namespace as the tilo.sh relay, behind
the relay's Caddy, which terminates TLS; the server serves plain HTTP.

## What the pod needs

- **No privilege**: the image runs as uid/gid 65532, and the container needs no
  capability.
- **Outbound reach**: the relay's origin over HTTPS, and its iroh endpoint over
  UDP (QUIC), for the MCP sessions it carries. Nothing else is exposed.
- **A writable volume at `/state`**: `TILO_API_STATE` names one private file
  there, holding the registered clients and the grants people gave them, and
  the server writes nothing else — the root filesystem can stay read-only.
- **The environment**, which is the whole configuration; the image holds
  none of it, so a changed value needs no new image:

  | | |
  | --- | --- |
  | `TILO_API_LISTEN` | `HOST:PORT` to listen on, e.g. `0.0.0.0:8080` |
  | `TILO_API_ORIGIN` | the origin agents reach, e.g. `https://mcp.tilo.sh` |
  | `TILO_API_WEB_ENTRY` | the person's Tilo on the web, e.g. `https://tilo.sh` |
  | `TILO_API_HOST` | the public origin of the relay their workspaces live on, e.g. `https://eu1.tilo.sh` |
  | `TILO_API_STATE` | the state file, e.g. `/state/api.json` |

| | |
| --- | --- |
| user | 65532:65532 |
| port | 8080/tcp, plain HTTP |
| volume | `/state` |
| health | `GET /health` answers `{"ok":true}` |
