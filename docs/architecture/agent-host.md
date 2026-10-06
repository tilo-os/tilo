# Agent host on an existing OS

How a foreign Linux host (a Cursor/Grok Bot box, a CI runner, a NAS sidecar)
uses Tilo **without** installing the desktop session, Wayland compositor, or
OS image.

## Three roles (do not collapse)

| Role | What it is | Typical surface |
|---|---|---|
| **Relay / headless store-and-forward** | Authenticated Iroh workspace host under the user's identity | `tilo-relay` (`os/containers/tilo-relay/`) |
| **Home server / full always-on peer** | Same image + Compose: device cert with host capability, full file replica, LAN pair from Settings | `tilo-relay` + `fs replica` |
| **Agent on an existing OS** | Process on a foreign host that acts in rooms via resources/actions (MCP), using a certified device | `tilo-runtime` MCP beside a local enrolled relay |

The agent is **not** a separate cloud Tilo API and **never** receives the human
root private key (`user-identity/privkey.bin`). Enrollment mints a device key
inside the container or profile; the user's device approves it.

## Preferred stack

```text
User's Tilo device  --Iroh-->  local tilo-relay container (home peer, enrolled)
                                    ^
                                    | Iroh (same user; invites / home-peers)
                              tilo-runtime (personal device, enrolled)
                                    ^
                                    | loopback MCP HTTP + token  (or stdio --connect)
                              Agent host (assistant MCP client)
```

- The **relay** stays on `network_mode: host` so LAN pair/discover works.
- The **MCP** listener binds **loopback only** by default
  (`TILO_RUNTIME_MCP_HTTP=127.0.0.1:7391`) with a bearer token file.
- Relay and runtime use **separate device keys** (Iroh endpoint ids cannot be
  shared by two processes). Both are certified under the **same user root**.
- Operator story: pair the home server once, then pair the agent runtime as an
  ordinary personal device (`tilo-enroll pair-device --headless`).

## Operator path (Compose)

See [`os/containers/tilo-agent-host/README.md`](../../os/containers/tilo-agent-host/README.md).

Summary:

1. `mkdir -p relay-identity relay-config agent-profile mcp-secrets && chmod 700 relay-identity agent-profile mcp-secrets`
2. `podman compose up -d` in `os/containers/tilo-agent-host/`
3. On the trusted Tilo device: Settings → Devices & Sync → **Pair home server**
4. On the agent container (or logs): note the six-digit personal-device code;
   approve that device from the trusted Tilo device
5. Point the assistant MCP client at `http://127.0.0.1:7391/mcp` with
   `Authorization: Bearer $(cat mcp-secrets/mcp-token)`, or use stdio:

   ```json
   {
     "mcpServers": {
       "tilo": {
         "command": "tilo-runtime",
         "args": ["--profile", "/absolute/path/to/agent-profile", "--connect"]
       }
     }
   }
   ```

6. Join a room invite when needed:
   `tilo-runtime --profile /absolute/path/to/agent-profile --join '<invitation>'`

## What this is not

- Not a full OS, compositor, or apps UI install
- Not copying `user-identity/privkey.bin` into the agent or relay
- Not exposing MCP beyond loopback without a token (the runtime refuses that)
- Not collapsing the agent into the home-peer principal: agents that need an
  `AgentDelegation` still use the agents path in [`agents.md`](agents.md)

## Related

- [`../runtime-mcp.md`](../runtime-mcp.md) — MCP tools, HTTP transport, profiles
- [`os/containers/tilo-relay/README.md`](../../os/containers/tilo-relay/README.md) — home peer
- [`os/containers/tilo-agent-host/README.md`](../../os/containers/tilo-agent-host/README.md) — Compose recipe
- [`collaboration.md`](collaboration.md) — identity, Iroh, hosts
- [`agents.md`](agents.md) — agent principals and MCP projection
