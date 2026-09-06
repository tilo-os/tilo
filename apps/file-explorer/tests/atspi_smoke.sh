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

# Files composes the toolbar and responsive body into semantic containers. A
# backend is free to flatten a container or retain it, so inspecting only the
# root's immediate children makes this smoke depend on AccessKit's incidental
# tree shape. Walk a small, cycle-safe subtree instead: the contract is that
# visible named controls are exported, not which intermediate group owns them.
# Keep the provider's real object paths too: the action assertion below must
# operate on the exact node an external screen reader discovered.
declare -A node_names=()
names=""
collect_tree() {
    local current child_path
    node_names=()
    names=""
    declare -a pending=("$content_path")
    declare -A visited=()
    while [[ ${#pending[@]} -gt 0 && ${#visited[@]} -lt 256 ]]; do
        current=${pending[0]}
        pending=("${pending[@]:1}")
        [[ -n ${visited[$current]+yes} ]] && continue
        visited[$current]=1
        local name
        name=$(property "$app_bus" "$current" Name) || return 1
        node_names["$current"]=$name
        names+="$name"$'\n'
        while IFS= read -r child_path; do
            [[ -n "$child_path" ]] && pending+=("$child_path")
        done < <(
            children "$app_bus" "$current" \
                | grep -Eo '/org/a11y/atspi/accessible/[0-9/]+' || true
        )
    done
    ((${#node_names[@]} > 0))
}

node_named() {
    local expected=$1 current needle
    needle="'$expected'"
    for current in "${!node_names[@]}"; do
        [[ ${node_names[$current]} == *"$needle"* ]] && {
            printf '%s\n' "$current"
            return 0
        }
    done
    return 1
}

wait_for_names() {
    local expected missing attempt
    for attempt in $(seq 1 200); do
        if collect_tree; then
            missing=""
            for expected in "$@"; do
                node_named "$expected" >/dev/null || missing+=" $expected"
            done
            [[ -z "$missing" ]] && return 0
        fi
        sleep 0.05
    done
    echo "AT-SPI tree never exposed:$missing" >&2
    printf '%s\n' "$names" >&2
    sed -n '1,120p' "$smoke_dir/file-explorer.log" >&2
    return 1
}

click_named() {
    local expected=$1 path candidate needle actions result
    needle="'$expected'"
    # A control can share its spoken label with neighbouring explanatory text.
    # Select the click-capable object from the live provider, never by a
    # brittle traversal position or a precomputed AccessKit id.
    for candidate in "${!node_names[@]}"; do
        [[ ${node_names[$candidate]} == *"$needle"* ]] || continue
        actions=$(gdbus call --address "$atspi_address" \
            --dest "$app_bus" \
            --object-path "$candidate" \
            --method org.a11y.atspi.Action.GetActions 2>/dev/null) || continue
        if [[ $actions == *"('click',"* ]]; then
            path=$candidate
            break
        fi
    done
    [[ -n ${path:-} ]] || {
        echo "AT-SPI tree has no click-capable node named '$expected'" >&2
        return 1
    }
    result=$(gdbus call --address "$atspi_address" \
        --dest "$app_bus" \
        --object-path "$path" \
        --method org.a11y.atspi.Action.DoAction 0)
    [[ ${result//[[:space:]]/} == '(true,)' ]] || {
        echo "AT-SPI click on '$expected' was rejected: $result" >&2
        return 1
    }
}

wait_for_names "Breadcrumbs" "View options"

# Exercise an ordinary screen-reader action against an actual package payload,
# then observe the application state it changed. `New folder` appears only in
# the menu opened by View options, so a provider that merely advertises Click
# but fails to deliver it through accesskit_winit cannot pass.
click_named "View options"
wait_for_names "New folder"

printf 'AT-SPI smoke passed: Files activity, toolbar controls, and View options action are exported.\n'
