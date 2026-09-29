#!/usr/bin/env bash
# The boot-chain invariants the source tree can answer (units, configs, their
# exact lines), in the fast suites: the full set runs only inside the image
# build, where a unit edit that breaks them fails the build an hour later.
exec bash "$(dirname "${BASH_SOURCE[0]}")/boot_chain_invariants.sh" --source
