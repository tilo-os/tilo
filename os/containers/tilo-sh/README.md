# tilo.sh on one machine

`stack.sh` starts the hosted product from this checkout. It runs the Lounge
relay behind production's Caddy, the front door, the web entry and (where
the machine can) the VM host, with the entrypoints and environment
production runs: this checkout's programs as plain processes by default,
the release images with `up --images`. `check.sh` uses it end to end. `docs/testing-tilo-sh.md` describes
both, and `stack.sh help` covers the commands.

| File | What it is |
| --- | --- |
| `stack.sh` | the one command: `up`, `restart`, `env`, `status`, `logs`, `browser`, `check`, `down` |
| `cargo.sh` | how the native stack compiles: `t build` (or the cargo drop-in `TILO_SH_CARGO` names), also the cargo the web bundle's build script gets |
| `Caddyfile` | production's relay routing (tilo/infra `apps/tilo-sh-relay/Caddyfile`) on loopback ports |
| `check.sh` | the visa flow and a fresh visitor's timed entry against the stack |
| `caddy.env` | production's Caddy, one pin: the stack fetches it once, and `t test tilo-sh-caddy` builds it for the checks |
| `check-in-vm.sh` | `t test tilo-sh-local-vm`: `check.sh` as an unprivileged user in a micro-VM, its VM host started as root through that user's sudo as on a developer's machine |
| `integration/person.mjs` | a person in their own headless Chromium: arrives, follows an invitation, approves a key on `/sign-in` |
| `integration/reviewer.py` | the visa run's reviewer endpoint, played by such a person |
| `integration/seed-history.py` | the Lounge's chat history, posted through the front door (`up --seed`) |

When a production manifest changes a service's environment, change the
matching `*_env` function in `stack.sh` the same way.
