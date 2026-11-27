# Tilo server container

This Compose file turns a NAS or home server into an always-on authenticated
workspace server. It stores offline updates for the workspaces it serves.

## Before starting

Create the persistent directories:

```text
mkdir -p identity config
chmod 700 identity config
```

The container creates its own device key; no user-root private key is copied to
the server.

## Start

On a computer that holds the person's identity, run:

```sh
tilo device add --server
```

It prints a ticket (one line, starting `tilo1.`), good for one use and ten
minutes, and leaves the server's entry in the person's Servers workspace. Pass
it to the server's first start:

```sh
TILO_TICKET='tilo1.…' podman compose up -d
```

The container spends the ticket: it logs in, keeps the certificate and the
login on the identity volume, and writes its configuration, then starts the
server. A volume that already holds its device key, certificate and login
ignores `TILO_TICKET`, so a restart never needs one, and a ticket handed to it
is left unspent.

An empty volume with no ticket fails with the words that say how to make one;
a ticket that was used, or that ran out, says so. One ticket per volume, never
one per start.

The named volumes are essential: they hold the stable Iroh
endpoint secret, CRDT snapshots, pending offline queues, revocations, and
metrics. Back it up together with the non-secret `server.toml`; protect the
identity directory separately. Replacing either volume changes availability
properties, so restore rather than recreate them after a failure.

The health check reports ready once the daemon answers a status call, asked in its
own environment; it starts nothing and never waits on a socket file alone.

## The web UI

The server serves its own web UI: its members open its address in a browser and work
there, without the Tilo app. It is on by default and answers HTTPS on the port
`web_listen` names (8443 in the template), with a certificate its person's local CA
issues, kept on the identity volume, until the configuration names a PEM chain and key
(`web_tls`). The person's own devices trust that certificate; a browser elsewhere does not.

Its person turns the web UI off from any of their devices, through the `web.serve` action
on the server's entry in their Servers workspace; it stops serving at once, and turns back
on the same way. Nothing in `server.toml` switches it: the setting is the person's, kept
in their workspace, so every device of theirs reads and changes it.

## Trust and members

The generated configuration uses `allow_matching_user`, so the owner's other
certified devices connect automatically. Workspace authority travels signed by
the owner identity.

## Three roles

This image is the **server** — not the MCP server:

1. **Server / headless store-and-forward** — this daemon (authenticated workspace host).
2. **Home server / always-on server** — this Compose file: host-capable device cert, LAN pair from Settings.
3. **Agent on an existing OS** — an MCP client talking to the **local** runtime beside this server: `tilo mcp` over the profile's own socket, or the host's own `/v1` API and MCP server (`docs/developers/api.md`). Nothing here is exposed to the network.

