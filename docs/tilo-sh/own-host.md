# Your own host

A [host](hosts.md) is a computer that keeps workspaces. tilo.sh is ours. The same thing runs on a machine
you control, and the SDKs, the command and your agents reach it the same way — only the address changes.

## Point at it

Three places name the host, and they agree:

| | How |
| :--- | :--- |
| The SDKs | `TILO_API_URL` in the environment, or `Tilo(url="…")` / `new Tilo({ url: "…" })` |
| The command | `--api`, or `TILO_API_URL` |
| An agent's MCP client | the URL you add: `<host>/mcp`, or `<host>/w/<workspace>/mcp` for one workspace |

A key belongs to its person, not to one host: it is the grant they approved, and it reaches their
workspaces wherever those are held. An API server in front of your host accepts the same key as one in
front of ours, and a key still works when the server that made it stops answering.

## Run one from the Tilo repository

The product's parts are in the Tilo repository, and one command brings them all up on one machine, with
production's entrypoints and environment:

```sh
git clone https://code.tilo.so/tilo/tilo
cd tilo
os/containers/tilo-sh/stack.sh up
eval "$(os/containers/tilo-sh/stack.sh env)"   # TILO_API_URL, TILO_SITE, and the local CA
```

That gives you, on loopback ports, the API the SDKs and agents use, the host your workspaces live on, and
the page a person opens — the same programs the hosted product runs. It builds them from the checkout with
the cargo drop-in `TILO_SH_CARGO` names, else with `$CARGO`, else with this machine's `cargo`; it also
wants `caddy` and `bubblewrap`, and says which is missing. Everything listens on `127.0.0.1`;
`stack.sh status` says where, and `stack.sh logs <service>` says what happened.

Then use it exactly as the hosted one:

```sh
tilo keys create "My backend"        # approve the link it prints in `stack.sh browser`
export TILO_API_KEY=tilo_sk_…
claude mcp add --transport http tilo-local http://localhost:24110/mcp
```

`stack.sh env` also sets the certificate authority the local site is signed with, for Node and Python,
so the SDKs trust it. For a browser of your own, import the CA file it names as a trusted authority.

## What is not there yet

- **A switch that makes an ordinary computer a host for agents.** There is none in Tilo today: the way to
  have a host of your own is the local stack above, which is what we test the product with. When a Tilo
  desktop can host workspaces for its person's agents, it will be the same API and the same keys, pointed
  at that machine. On a machine that is yours alone, `TILO_SH_TERMINAL_ON_MACHINE=1 stack.sh up` has
  your agents' Terminal start real programs (`git`, `cargo`) on it, unconfined, as the stack's own user.
- **Webhooks.** A host tells you what happened over the [event stream](events.md) it serves. Nothing calls
  your server.
- **A hosted Browser needs a machine with KVM.** Running pages in a workspace's own Linux machine is part of
  that machine's VM host, which the local stack starts where the computer has `/dev/kvm`; without one,
  workspaces, keys and the API work and the Browser and a VM's commands do not.

## Where next

- [Hosts](hosts.md): what a host keeps, and one host versus several.
- [Quickstart](quickstart.md): the same steps against tilo.sh.
