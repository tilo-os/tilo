#!/usr/bin/env bash
# Format-independent assertions on an installed tilo-runtime package.
set -euo pipefail

[[ $# == 1 ]] || { echo "usage: $0 ROOT" >&2; exit 2; }
root=${1%/}
path() { printf '%s%s' "$root" "$1"; }
# Arch has no /usr/libexec and uses /usr/lib/tilo/runtime instead.
private=${TILO_RUNTIME_DIR:-/usr/libexec/tilo-runtime}

for file in "$private/tilo-runtime" "$private/tilo-launcher" "$private/store"; do
    [[ -x $(path "$file") && ! -L $(path "$file") ]] || { echo "missing executable: $file" >&2; exit 1; }
done
[[ $(readlink "$(path /usr/bin/tilo-runtime)") == "../${private#/usr/}/tilo-runtime" ]] || {
    echo "/usr/bin/tilo-runtime must link to $private/tilo-runtime" >&2
    exit 1
}
for file in /usr/lib/systemd/user/tilo-runtime.service \
            /usr/share/applications/so.tilo.store.desktop \
            /usr/share/icons/hicolor/scalable/apps/so.tilo.store.svg; do
    [[ -f $(path "$file") ]] || { echo "missing file: $file" >&2; exit 1; }
done
if [[ ! -f $(path /usr/share/licenses/tilo-runtime/LICENSE) \
      && ! -f $(path /usr/share/doc/tilo-runtime/copyright) ]]; then
    echo "missing Tilo license text" >&2
    exit 1
fi

grep -Fxq 'ExecStart=/usr/bin/tilo-runtime --daemon' "$(path /usr/lib/systemd/user/tilo-runtime.service)"
grep -Fxq "Exec=$private/store" "$(path /usr/share/applications/so.tilo.store.desktop)"
grep -Fxq 'Name=Tilo' "$(path /usr/share/applications/so.tilo.store.desktop)"
