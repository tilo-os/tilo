#!/usr/bin/env bash
# Exercise the one-boot greetd handoff cleanup without touching the host /etc.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/usr/libexec/tilo-firstboot-autologin-cleanup"
tmp="$(mktemp -d "${TMPDIR:-/tmp}/tilo-firstboot-autologin.XXXXXX")"
cleanup() { rm -rf -- "$tmp"; }
trap cleanup EXIT INT TERM

mkdir -p "$tmp/etc/tilo" "$tmp/etc/greetd" "$tmp/usr/share/tilo" "$tmp/bin"
cat > "$tmp/usr/share/tilo/greetd-config.toml" <<'EOF'
[terminal]
vt = 1

[default_session]
command = "normal graphical greeter"
user = "greetd"
EOF
cat > "$tmp/etc/greetd/config.toml" <<'EOF'
[initial_session]
command = "first boot only"
user = "tilo"
EOF
printf 'version=1\n' > "$tmp/etc/tilo/firstboot-autologin-pending"
chmod 0600 "$tmp/etc/tilo/firstboot-autologin-pending"

# The checkout test is unprivileged.  The production helper requires a
# root-owned marker; emulate that one metadata fact while preserving every
# other operation, including atomic replacement and marker removal.
cat > "$tmp/bin/stat" <<'EOF'
#!/bin/bash
if [ "$1" = '-c' ] && [ "$2" = '%u:%a:%h' ]; then
  printf '0:600:1\n'
else
  exec /usr/bin/stat "$@"
fi
EOF
cat > "$tmp/bin/chcon" <<'EOF'
#!/bin/bash
exit 0
EOF
chmod 0755 "$tmp/bin/stat" "$tmp/bin/chcon"

test_helper="$tmp/tilo-firstboot-autologin-cleanup"
sed \
  -e "s#/etc/tilo/firstboot-autologin-pending#$tmp/etc/tilo/firstboot-autologin-pending#g" \
  -e "s#/etc/greetd/config.toml#$tmp/etc/greetd/config.toml#g" \
  -e "s#/usr/share/tilo/greetd-config.toml#$tmp/usr/share/tilo/greetd-config.toml#g" \
  "$helper" > "$test_helper"
chmod 0755 "$test_helper"

PATH="$tmp/bin:$PATH" "$test_helper"
cmp -s "$tmp/usr/share/tilo/greetd-config.toml" "$tmp/etc/greetd/config.toml"
[ ! -e "$tmp/etc/tilo/firstboot-autologin-pending" ]

# Once the marker is gone, later starts must leave an administrator's greeter
# configuration alone instead of continually overwriting it.
printf 'later greeter config\n' > "$tmp/etc/greetd/config.toml"
PATH="$tmp/bin:$PATH" "$test_helper"
grep -Fxq 'later greeter config' "$tmp/etc/greetd/config.toml"

# An unsafe marker must fail closed and stay in place for retry/diagnosis.  Do
# not use the metadata shim here: the unprivileged owner is intentionally wrong.
printf 'version=1\n' > "$tmp/etc/tilo/firstboot-autologin-pending"
chmod 0644 "$tmp/etc/tilo/firstboot-autologin-pending"
if PATH="/usr/bin:/bin" "$test_helper" >/dev/null 2>&1; then
  echo 'unsafe first-boot autologin marker was accepted' >&2
  exit 1
fi
grep -Fxq 'later greeter config' "$tmp/etc/greetd/config.toml"
[ -e "$tmp/etc/tilo/firstboot-autologin-pending" ]

echo 'first-boot autologin cleanup: ok'
