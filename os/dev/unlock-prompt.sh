#!/usr/bin/env bash
# Answer an installed guest's disk-unlock prompt from outside the guest.
#
#   ./os/dev/unlock-prompt.sh SECRET
#
# A VM whose installed root is LUKS2 stops in its initramfs until a secret is
# typed. Nothing is reachable over the network at that point, so the only input
# is the emulated keyboard (os/dev/vm.sh type) — the same keys a person at the
# machine would press. The guest is told to put /dev/console on the screen
# (disk-image.sh --encrypt appends `console=tty0` last for exactly this), so
# what is typed reaches the prompt rather than a serial line nothing is reading.
#
# It types blindly, like os/dev/test-encrypted-install.sh: the prompt's arrival
# is not observable from here, and a secret typed slightly early stays in the
# tty's line buffer until the prompt reads it. Callers repeat it (the
# boot-assessment harness's UNLOCK does, every few seconds) and stop once the
# guest answers.
#
# Its two variables come from the harness, as for every vm.sh action.
set -euo pipefail
secret=${1:?usage: unlock-prompt.sh SECRET}
[ -n "$secret" ] || { echo 'unlock-prompt: empty secret' >&2; exit 2; }
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
printf '%s' "$secret" | "$here/vm.sh" type - >/dev/null 2>&1 || exit 1
"$here/vm.sh" keys ret >/dev/null 2>&1 || true
