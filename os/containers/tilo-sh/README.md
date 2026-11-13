# tilo.sh on one machine

`stack.sh` starts the hosted product from this checkout. It runs the Lounge
relay behind production's Caddy, the API server, the web entry and (where
the machine can) the VM host, with the entrypoints and environment
production runs: this checkout's programs as plain processes by default,
the release images with `up --images`. `check.sh` uses it end to end. `docs/testing-tilo-sh.md` describes
both, and `stack.sh help` covers the commands.

| File | What it is |
| --- | --- |
| `stack.sh` | the one command: `up`, `restart`, `env`, `status`, `logs`, `browser`, `check`, `down` |
| `cargo.sh` | how the native stack compiles: `t build` (or the cargo drop-in `TILO_SH_CARGO` names), also the cargo the web bundle's build script gets |
| `Caddyfile` | production's relay routing (tilo/infra `apps/tilo-sh-relay/Caddyfile`) on loopback ports |
| `owner-link.mjs` | `stack.sh owner`'s link: a page under the site's own origin, which reads the user key that browser made for the Lounge and hands it over |
| `check.sh` | the startup identity, the visa flow, a fresh visitor's timed entry and a passkey, as steps of one check (`stack.sh check`) or as flows beside the page's (`t test e2e`, `check.sh --flow`) |
| `caddy.env` | production's Caddy, one pin: the stack fetches it once, and `t test tilo-sh-caddy` builds it for the checks |
| `check-in-vm.sh` | `t test tilo-sh-local-vm`: `check.sh` as an unprivileged user in a micro-VM, its VM host started as root through that user's sudo as on a developer's machine |
| `integration/person.mjs` | a person in their own headless Chromium: arrives, follows an invitation, approves a key on `/sign-in` |
| `integration/hosted-visa.py` | a hosted workspace's Browser: opened, filled and submitted through page actions, with the person asked |
| `integration/hosted-terminal.py` | a hosted workspace's Linux commands: the plan a card admits, the Terminal's run and its History, an agent's key through the `tilo` command, and the tenant's account |
| `integration/vm-proofs.py` | what the workspace's VM must be for the Browser in it: sandboxes, the browser user, its device key, memory |
| `integration/reviewer.py` | the visa run's reviewer endpoint, played by such a person |
| `integration/seed-history.py` | the Lounge's chat history, posted through the API server (`up --seed`) |

When a production manifest changes a service's environment, change the
matching `*_env` function in `stack.sh` the same way.
