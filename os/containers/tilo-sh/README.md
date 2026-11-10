# tilo.sh on one machine

`stack.sh` starts the hosted product from this checkout. It runs the Lounge
relay behind production's Caddy, the front door, the web entry and (where
the machine can) the VM host, with the images and environment production
runs. `check.sh` uses it end to end. `docs/testing-tilo-sh.md` describes
both, and `stack.sh help` covers the commands.

| File | What it is |
| --- | --- |
| `stack.sh` | the one command: `up`, `env`, `status`, `logs`, `browser`, `check`, `down` |
| `Caddyfile` | production's relay routing (tilo/infra `apps/tilo-sh-relay/Caddyfile`) on loopback ports |
| `check.sh` | the visa flow and a fresh visitor's timed entry against the stack |
| `check-in-vm.sh` | `t test tilo-sh-local`: `check.sh` as an unprivileged user in a micro-VM, rootless as on a developer's machine |
| `integration/person.mjs` | a person in their own headless Chromium: arrives, follows an invitation, approves a key on `/sign-in` |
| `integration/reviewer.py` | the visa run's reviewer endpoint, played by such a person |
| `integration/seed-history.py` | the Lounge's chat history, posted through the front door (`up --seed`) |

When a production manifest changes a service's environment, change the
matching `*_env` function in `stack.sh` the same way.
