# Tilo agent host (MCP beside local relay)

Compose recipe for role 3 in
[`docs/architecture/agent-host.md`](../../../docs/architecture/agent-host.md):
an MCP server beside a local `tilo-relay` home peer on an existing Linux host.

## Roles

1. **`tilo-relay`** — home peer / authenticated workspace + file replica (existing image).
2. **`tilo-mcp`** — `tilo-runtime` daemon with loopback MCP HTTP; enrolled as a
   **personal device** under the same user (separate device key from the relay).

Do **not** put a user-root private key in either identity directory.

## Before starting

```sh
mkdir -p relay-identity relay-config agent-profile mcp-secrets
chmod 700 relay-identity agent-profile mcp-secrets
```

## Start

```sh
podman compose up -d
```

Both services use `network_mode: host` so LAN enrollment reaches the physical
broadcast domain. The MCP HTTP socket still binds **127.0.0.1** only.

### 1. Pair the home server

Open Settings → Devices & Sync on the trusted Tilo device → **Pair home server**.
Confirm the six-digit code. The relay writes its device cert and home-peer bundle
under `relay-identity/` and `relay-config/`.

### 2. Pair the agent runtime

The MCP entrypoint runs `tilo-enroll pair-device --headless` until
`agent-profile/identity/device/cert.bin` exists. Approve that ordinary device from
the trusted Tilo device (same Devices & Sync surfaces as pairing another laptop).

### 3. Connect an assistant

A token is created at `mcp-secrets/mcp-token` on first start (mode 0600).

HTTP (Streamable MCP):

```sh
TOKEN=$(tr -d '[:space:]' < mcp-secrets/mcp-token)
curl -sS -H "Authorization: Bearer $TOKEN" http://127.0.0.1:7391/health
```

Configure the assistant with `http://127.0.0.1:7391/mcp` and that bearer token.
Stdio alternative on the host (same profile volume):

```json
{
  "mcpServers": {
    "tilo": {
      "command": "podman",
      "args": [
        "compose", "exec", "-T", "tilo-mcp",
        "tilo-runtime", "--profile", "/var/lib/tilo-runtime/profile", "--connect"
      ]
    }
  }
}
```

Join a workspace when you have an invite:

```sh
podman compose exec tilo-mcp \
  tilo-runtime --profile /var/lib/tilo-runtime/profile --join '<invitation>'
```

## Images

| Service | Default image | Build |
|---|---|---|
| `tilo-relay` | `registry.tilo.so/tilo/tilo-relay:latest` | `os/containers/tilo-relay/Containerfile` |
| `tilo-mcp` | `registry.tilo.so/tilo/tilo-runtime:latest` | this directory's `Containerfile` |

Override with `TILO_RELAY_IMAGE` / `TILO_MCP_IMAGE`. Neither image compiles
Rust inside its build: t builds the programs once and the image build stages them
beside the Containerfile. From the repository root:

```sh
# The programs both images ship (t's cache makes this seconds once warm).
t test relay-binaries

# The runtime image; agent-host-build.sh resolves the frozen RPM snapshot and
# stages the binaries for it (tools/ci/publish/agent-host-build.sh).
t test image-agent-host
```

`podman build` directly also works from the staged context that check leaves
(its `Containerfile` and `bin/`), and asks for `RPMS_REF`, the digest-pinned
package snapshot every Tilo server image installs from.

## Environment

| Variable | Default | Purpose |
|---|---|---|
| `TILO_DEVICE_NAME` | `Home server` | Relay pair label |
| `TILO_AGENT_DEVICE_NAME` | `Agent host` | Personal-device pair label |
| `TILO_RUNTIME_PROFILE` | `/var/lib/tilo-runtime/profile` | Runtime profile (identity, sockets) |
| `TILO_RUNTIME_MCP_HTTP` | `127.0.0.1:7391` | MCP listen address (loopback) |
| `TILO_RUNTIME_MCP_HTTP_TOKEN_FILE` | `/run/tilo/mcp-token` | Bearer token path |
| `TILO_COLLAB_HOST` | _(empty)_ | Optional Iroh endpoint id / JSON `EndpointAddr` for the local home peer |
| `TILO_ENROLLMENT_TOKEN_FILE` | — | Optional token for relay join (compat) |

## Contract test

```sh
./os/containers/tilo-agent-host/test-contract.sh
```

Checks Compose wiring, loopback MCP defaults, no user-root privkey mounts, and
the headless enroll entrypoint.
