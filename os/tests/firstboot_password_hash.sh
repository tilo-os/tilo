#!/bin/bash
set -euo pipefail

repo_root="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
# The source path is derived from the checked-out repository so this test also
# works outside the repository root.
# shellcheck disable=SC1091
source "$repo_root/os/usr/libexec/tilo-firstboot-user"

digest="$(printf 'A%.0s' {1..86})"

accept() {
  valid_sha512_crypt "$1" || {
    printf 'valid SHA-512 crypt hash was rejected: %s\n' "$1" >&2
    exit 1
  }
}

reject() {
  if valid_sha512_crypt "$1"; then
    printf 'malformed SHA-512 crypt hash was accepted: %s\n' "$1" >&2
    exit 1
  fi
}

accept "\$6\$salt\$$digest"
accept "\$6\$rounds=5000\$installerSalt\$$digest"
accept "\$6\$rounds=1000\$.\$$digest"
accept "\$6\$rounds=999999999\$0123456789./ABCD\$$digest"

reject "\$5\$salt\$$digest"
reject "\$6\$\$$digest"
reject "\$6\$0123456789ABCDEFG\$$digest"
reject "\$6\$salt\$${digest%?}"
reject "\$6\$salt\$${digest}A"
reject "\$6\$salt=bad\$$digest"
reject "\$6\$salt\$${digest%?}="
reject "\$6\$rounds=999\$salt\$$digest"
reject "\$6\$rounds=1000000000\$salt\$$digest"
reject "\$6\$rounds=05000\$salt\$$digest"
reject "\$6\$rounds=abc\$salt\$$digest"
reject "\$6\$rounds=5000salt\$$digest"

printf 'firstboot SHA-512 crypt validation: ok\n'
