#!/usr/bin/env bash
# Live Linux accessibility smoke test. Cargo's hermetic jobs cannot run it, but
# the installed-image release VM does after graphical login. It also remains
# directly runnable by developers in a graphical session.
set -euo pipefail

command -v busctl >/dev/null
command -v gdbus >/dev/null
binary="${QOS_FILES_BINARY:-}"
if [[ -z "$binary" ]]; then
    repo_root=$(git rev-parse --show-toplevel)
    binary="$repo_root/target/debug/file-explorer"
fi
if [[ ! -x "$binary" ]]; then
    if [[ -n "${repo_root:-}" ]]; then
        cargo build -p file-explorer
    else
        echo "Files binary is missing or non-executable: $binary" >&2
        exit 1
    fi
fi

smoke_dir=$(mktemp -d /tmp/qos-files-atspi.XXXXXX)
app_pid=""
cleanup() {
    if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    case "$smoke_dir" in
        /tmp/qos-files-atspi.*) rm -r "$smoke_dir" ;;
    esac
}
trap cleanup EXIT INT TERM

atspi_address=$(
    gdbus call --session \
        --dest org.a11y.Bus \
        --object-path /org/a11y/bus \
        --method org.a11y.Bus.GetAddress \
        | sed -E "s/^\('([^']+)'.*/\1/"
)

env \
    XDG_CONFIG_HOME="$smoke_dir/config" \
    QOS_USER_HOME="$smoke_dir" \
    "$binary" >"$smoke_dir/file-explorer.log" 2>&1 &
app_pid=$!

property() {
    local bus_name=$1 object_path=$2 property_name=$3
    gdbus call --address "$atspi_address" \
        --dest "$bus_name" \
        --object-path "$object_path" \
        --method org.freedesktop.DBus.Properties.Get \
        org.a11y.atspi.Accessible "$property_name"
}

children() {
    local bus_name=$1 object_path=$2
    gdbus call --address "$atspi_address" \
        --dest "$bus_name" \
        --object-path "$object_path" \
        --method org.a11y.atspi.Accessible.GetChildren
}

first_object_path() {
    grep -Eo '/org/a11y/atspi/accessible/[0-9/]+' | head -n 1
}

app_bus=""
for _ in $(seq 1 200); do
    app_bus=$(
        busctl --address="$atspi_address" list --no-pager \
            | awk -v pid="$app_pid" '$2 == pid { print $1; exit }'
    )
    [[ -n "$app_bus" ]] && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.05
done

if [[ -z "$app_bus" ]]; then
    echo "file-explorer did not register on the AT-SPI bus" >&2
    sed -n '1,80p' "$smoke_dir/file-explorer.log" >&2
    exit 1
fi

atspi_root=/org/a11y/atspi/accessible/root
window_path=""
for _ in $(seq 1 400); do
    window_path=$(children "$app_bus" "$atspi_root" | first_object_path || true)
    [[ -n "$window_path" ]] && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.05
done

if [[ -z "$window_path" ]]; then
    echo "file-explorer registered, but did not publish an accessible window" >&2
    sed -n '1,80p' "$smoke_dir/file-explorer.log" >&2
    exit 1
fi

content_path=$(children "$app_bus" "$window_path" | first_object_path || true)
if [[ -z "$content_path" ]]; then
    echo "accessible window has no application content" >&2
    exit 1
fi

content_name=$(property "$app_bus" "$content_path" Name)
[[ "$content_name" == *"Files activity"* ]] || {
    echo "unexpected Files root name: $content_name" >&2
    exit 1
}

top_level=$(children "$app_bus" "$content_path")
names=""
while IFS= read -r child_path; do
    [[ -n "$child_path" ]] || continue
    names+="$(property "$app_bus" "$child_path" Name)"$'\n'
done < <(printf '%s\n' "$top_level" | grep -Eo '/org/a11y/atspi/accessible/[0-9/]+' )

[[ "$names" == *"Breadcrumbs"* ]] || {
    echo "AT-SPI tree is missing Breadcrumbs" >&2
    exit 1
}
[[ "$names" == *"Places"* ]] || {
    echo "AT-SPI tree is missing Places" >&2
    exit 1
}

printf 'AT-SPI smoke passed: Files activity, Breadcrumbs, and Places are exported.\n'
