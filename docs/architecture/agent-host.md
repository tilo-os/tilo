# Agent host on an existing OS

How a foreign Linux host (a Cursor/Grok Bot box, a CI runner, a NAS sidecar)
uses Tilo **without** installing the desktop session, Wayland compositor, or
OS image.

## Three roles (do not collapse)

| Role | What it is | Typical surface |
|---|---|---|
| **Relay / headless store-and-forward** | Authenticated Iroh workspace host under the user's identity | `tilo-relay` (`os/containers/tilo-relay/`) |
| **Home server / full always-on peer** | Same image + Compose: device cert with host capability, full file replica, LAN pair from Settings | `tilo-relay` + `fs replica` |
| **Agent on an existing OS** | Process on a foreign host that acts in workspaces via resources/actions (MCP), using a certified device | `tilo-runtime` MCP beside a local enrolled relay |

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

6. Join a workspace invite when needed:
   `tilo-runtime --profile /absolute/path/to/agent-profile --join '<invitation>'`

## An agent on one of your devices steering another

An agent on device A (a Tilo box, or a laptop running an enrolled
`tilo-runtime`) can act on device B — its apps, files, terminal and
workspace — exactly as if it ran on B, over B's own agent door. There is no
TLS or HTTP listener, no SSH and no hosted proxy: the API server is carried over
iroh, as ALPN `tilo-agent-door/1` on the endpoint each device already runs
(`crates/runtime/tilo-runtime/src/remote_door.rs`).

```text
Claude Code on A --stdio--> tilo-runtime-mcp --connect --device B
                                 | {"forward":{"device":"B"}}   (A's session or runtime)
                                 v
                  A's endpoint --iroh, tilo-agent-door/1--> B's endpoint
                                                              | device check
                                                              v
                                              B's agent door (sheet or setting)
                                                              v
                                              B's broker, as agent claude-code@a-…
```

1. **The client.** `tilo agent connect claude --device NAME [--workspace ID]`
   registers `tilo-runtime-mcp --connect --device NAME` as the MCP server
   `tilo-<name>`. The client opens its own host's door (the desktop's
   `agent.sock`, or a standalone runtime's `native.sock`) and asks, in one
   first line, to be carried to that device. Its host — the process that
   holds A's device key and endpoint — dials B, shows A's device certificate,
   and answers the client whether B's door opened; from then on the client's
   stdio is B's door. A device is named by its label or its endpoint id.
2. **B decides who dials from the transport.** The connection's endpoint id
   is A's device key (the QUIC handshake proved it). The certificate A shows
   must name that key, be signed by B's person's identity root, hold now, not
   be a store node's, and not be revoked in the person's own store, which
   carries revocations between all their devices. Anything else is refused,
   with the reason, before anything else on the stream is read and before
   anyone is asked.
3. **Admission on B is the local door's.** The person is asked on the same
   sheet (“Claude Code, from your device Studio laptop”), and “Always allow”
   is remembered per (device, client). B's device setting **Agents from your
   other devices** (`agent_door.accept_own_devices`, off unless turned on,
   kept on B so a box branched from it inherits it) admits such an agent
   without a sheet — for a headless box nobody sits at. It never admits a
   device that fails step 2, nor a program of B's own login, and an agent from
   another device may never act as the person.
4. **What the agent is.** It works as `Caller::local_person(person)
   .delegated_to(agent)`, in one workspace (the one it asked for with
   `--workspace` when B holds it, else the one in front), through the same
   gate as every caller — and, as every admitted connection of B's door does
   (a program of B's login or of one of its environments alike), in a run of
   its own: B mints a task agent for the connection, B's device delegates it,
   and everything the agent changes is a step of that run, decided by the
   person's live grants capped at an agent's
   (`crates/session/session/src/door_runs.rs`). The run ends with the
   connection. A step the person's grants ask first about is put to them on
   B's sheet, naming the agent as B's desk seats it: Allow once, Always allow
   the agent (for as long as its run goes), or Deny
   (`crates/session/session/src/run_questions.rs`). The agent waits while the
   sheet is up; it leaving, its run ending or the person dismissing the sheet
   withdraws the question, and the agent is told why. Its id names the device it came from
   (`claude-code@studio-laptop-3f2a9c1b`), so what it does is told apart from
   the same client on B and from another device's, and its seat on B's bar
   carries that name; the person's Stop there ends the connection on A too.
   Disconnecting either side ends the other; a question still up when the
   agent leaves is withdrawn.

The tool set is the local door's, so a remote session differs only in its
attribution, plus `tilo_devices` on every door: the person's devices as this
one knows them, with each one's endpoint id and whether its door answers now
— how an agent finds the boxes it can be connected to.

## An agent on a session nobody sits at: the agent key

A session started for an agent (`cargo headless`, a test, a box with no one
at it) has no one to answer its door's sheet. The person decides as they
start it instead: `tilo-session --agent-key FILE` keeps a secret key in FILE
(made there, 0600, when FILE is empty or absent), and once the session knows
its person, their root certifies an agent's grant for it
(`DeviceCapabilities::agent_grant`, all their workspaces, 30 days, renewed as
the session starts) and keeps it with their devices
(`DeviceFleetStore::certify_agent_grant`). The grant's key is derived from
the secret, so whoever holds FILE holds the grant.

An MCP client presents the key in its `initialize`
(`params.capabilities.experimental["so.tilo"].key`); `tilo mcp` and
`tilo-runtime-mcp --connect` write the key the file in `TILO_AGENT_KEY`
names. The door admits it as the grant's agent without a sheet
(`agent-key-<8 hex>@<place>#<n>`, shown with the client's own name), in the
workspace in front or the one it asked for, in a run of its own like any
admitted connection, so what it does is attributed to that grant. It is
revoked as a device is: from the person's device list, or with Revoke on the
agent's card, which has their root sign the revocation; the door then refuses
the key, saying why, and a revoked key is never certified again (a new key is
a new FILE). A key the session was not started with is refused, never asked
about. The key never reaches the request the door goes on to answer, its
log or the sheet (`crates/session/session/src/agent_door/agent_key.rs`).
`cargo headless` starts its session with `STATE/agent.key`, and
`cargo headless --check` proves an agent with it is admitted, lists the apps
and opens one.

Device-to-device *actions* (`tilo.own-device`) still admit only the person at
the device, never an agent: an agent reaches another device through that
device's agent door, where its person decides.

## Hosted workspaces: the API server

A hosting provider puts [`tilo-api`](../../crates/hosting/tilo-api/README.md)
in front of the `tilo-runtime` HTTP endpoints of the workspaces it hosts. It is
the proxy those endpoints expect: the person authorizes an agent there (OAuth
2.1 as the MCP authorization profile asks, signing in with a device of theirs
through `tilo_user_identity::SignInProof`) for all their workspaces or one, and
each agent session is routed to the endpoint of the workspace it acts in with
that endpoint's token. Listing and creating workspaces go to the person's hosts
as their own operations (`room.list`, `room.create`), asked by a device of the
person the API server holds; a host may refuse.

## What this is not

- Not a full OS, compositor, or apps UI install
- Not copying `user-identity/privkey.bin` into the agent or relay
- Not exposing MCP without a token, loopback included (the runtime refuses
  that: every process and account on the machine can reach a loopback port)
- Not collapsing the agent into the home-peer principal: agents that need an
  `AgentDelegation` still use the agents path

## Related

- [`os/containers/tilo-relay/README.md`](../../os/containers/tilo-relay/README.md) — home peer
- [`os/containers/tilo-agent-host/README.md`](../../os/containers/tilo-agent-host/README.md) — Compose recipe
- [`workspace.md`](workspace.md) — members, devices and Iroh
