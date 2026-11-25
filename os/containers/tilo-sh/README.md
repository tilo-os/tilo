# tilo.sh on one machine

`stack.sh` starts the hosted product from this checkout. It runs the Lounge
server behind production's Caddy, the API server, the web entry and (where
the machine can) the VM host, with the entrypoints and environment
production runs: this checkout's programs as plain processes by default,
the release images with `up --images`. `check.sh` uses it end to end. `docs/testing-tilo-sh.md` describes
both, and `stack.sh help` covers the commands.

| File | What it is |
| --- | --- |
| `stack.sh` | the one command: `up`, `restart`, `env`, `status`, `logs`, `mail-account`, `browser`, `check`, `down` |
| `cargo.sh` | how the native stack compiles: the cargo drop-in `TILO_SH_CARGO` names (default `$CARGO`, else `cargo`), also the cargo the web bundle's build script gets |
| `build-on-builder.sh` | the programs built on the build server, in one command |
| `port-check.py` | which of a base's ports this machine would not give the stack (another process's, or the kernel's temporary ports), and where that machine's temporary-port range begins: `stack.sh up` checks it here and on the build server |
| `Caddyfile` | production's server routing (tilo/infra `apps/tilo-sh-server/Caddyfile`) on loopback ports |
| `check.sh` | the startup identity, the visa flow, a fresh visitor's timed entry and a passkey, as steps of one check (`stack.sh check`) or as flows beside the page's (`t test e2e`, `check.sh --flow`) |
| `caddy.env` | production's Caddy, one pin: the stack fetches it once, and `t test tilo-sh-caddy` builds it for the checks |
| `check-in-vm.sh` | `t test tilo-sh-local-vm`: `check.sh` as an unprivileged user in a micro-VM, its VM host started as root through that user's sudo as on a developer's machine |
| `integration/person.mjs` | a person in their own headless Chromium: arrives, follows an invitation, approves a key on `/sign-in` |
| `integration/hosted-visa.py` | a hosted workspace's Browser: opened, filled and submitted through page actions, with the person asked |
| `integration/hosted-terminal.py` | a hosted workspace's Linux commands: the plan a card admits, the Terminal's run and its History, an agent's key through the `tilo` command, and the tenant's account |
| `integration/model_stand_in.py` | a stand-in for a model provider (Anthropic's Messages protocol) on a loopback address: one fixed sentence, or one tool call and an end; it notes each request's path and whether a key header came, never a value |
| `integration/vm-proofs.py` | what the workspace's VM must be for the Browser in it: sandboxes, the browser user, its device key, memory |
| `integration/reviewer.py` | the visa run's reviewer endpoint, played by such a person |
| `integration/seed-history.py` | the Lounge's chat history, posted through the API server (`up --seed`) |

`TILO_SH_MAIL_STAND_IN=1 stack.sh up` also starts a stand-in mail server:
an IMAP and an SMTP server with real sockets that take one made-up login
(`lab@example.test`), show an empty inbox and send nothing. It is built with
the programs and listens on two of the stack's base ports; its certificate
is added to this stack's trust bundle alone. `stack.sh mail-account` prints
what a person types into a mail account's settings, so a sign-in can be
rehearsed in a browser against the stack with no real mail account. A stack
on the build server carries it the same way, and its ports are forwarded
here with the rest.

When a production manifest changes a service's environment, change the
matching `*_env` function in `stack.sh` the same way.
