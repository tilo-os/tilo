#!/usr/bin/env bash
# Live Linux accessibility smoke test. Cargo's hermetic jobs cannot run it, but
# the installed-image release VM does after graphical login. It also remains
# directly runnable by developers in a graphical session.
set -euo pipefail

command -v busctl >/dev/null
command -v gdbus >/dev/null
binary="${TILO_FILES_BINARY:-}"
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

smoke_dir=$(mktemp -d /tmp/tilo-files-atspi.XXXXXX)
app_pid=""
cleanup() {
    if [[ -n "$app_pid" ]] && kill -0 "$app_pid" 2>/dev/null; then
        kill "$app_pid" 2>/dev/null || true
        wait "$app_pid" 2>/dev/null || true
    fi
    case "$smoke_dir" in
        /tmp/tilo-files-atspi.*) [[ -d "$smoke_dir" ]] && rm -r "$smoke_dir" ;;
    esac
}
trap cleanup EXIT INT TERM

# Request accessibility as a screen reader does. This smoke must also work
# without a preceding compositor smoke enabling providers on the session bus.
busctl --user set-property org.a11y.Bus /org/a11y/bus org.a11y.Status IsEnabled b true

atspi_address=$(
    gdbus call --session \
        --dest org.a11y.Bus \
        --object-path /org/a11y/bus \
        --method org.a11y.Bus.GetAddress \
        | sed -E "s/^\('([^']+)'.*/\1/"
)

env \
    XDG_CONFIG_HOME="$smoke_dir/config" \
    TILO_USER_HOME="$smoke_dir" \
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

# AccessKit's first complete update follows the adapter's initial placeholder
# tree.  A client may observe the old child list while that transaction is
# being replaced; never retain that stale object path.  Re-resolve the named
# application root for every walk and require it to be queryable, so this is a
# race-safe assertion of the real provider rather than a sleep disguised as a
# test.
content_path=""
resolve_content_path() {
    local candidate content_name
    # Keep using the exact object a client already resolved while it remains
    # queryable.  Revalidating its Name makes this safe across an incremental
    # update; falling back below only happens for a real UnknownObject/stale
    # provider object, rather than turning every poll into a costly walk of a
    # large folder tree.
    if [[ -n "$content_path" ]]; then
        content_name=$(property "$app_bus" "$content_path" Name 2>/dev/null) || true
        if [[ "$content_name" == *"Files activity"* ]]; then
            return 0
        fi
    fi
    content_path=""
    while IFS= read -r candidate; do
        [[ -n "$candidate" ]] || continue
        content_name=$(property "$app_bus" "$candidate" Name 2>/dev/null) || continue
        if [[ "$content_name" == *"Files activity"* ]]; then
            content_path="$candidate"
            return 0
        fi
    done < <(
        children "$app_bus" "$window_path" 2>/dev/null \
            | grep -Eo '/org/a11y/atspi/accessible/[0-9/]+' || true
    )
    return 1
}

for _ in $(seq 1 400); do
    resolve_content_path && break
    kill -0 "$app_pid" 2>/dev/null || break
    sleep 0.05
done
if [[ -z "$content_path" ]]; then
    echo "accessible window never exposed a queryable Files activity root" >&2
    exit 1
fi

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
    local current child_path expected all_found
    local -a required=("$@")
    node_names=()
    names=""
    # Start every bounded retry at the live provider tree.  This catches an
    # accidental stale-object use while allowing the adapter's documented
    # initial-tree replacement to complete without hiding a broken endpoint.
    resolve_content_path || return 1
    declare -a pending=("$content_path")
    declare -A visited=()
    # The controls asserted below live in the application chrome/overlay
    # frontier, not an unbounded folder listing.  This keeps each live-bus
    # retry bounded even for a directory with thousands of rows; absence still
    # fails the gate rather than being masked by a partial result.
    while [[ ${#pending[@]} -gt 0 && ${#visited[@]} -lt 64 ]]; do
        current=${pending[0]}
        pending=("${pending[@]:1}")
        [[ -n ${visited[$current]+yes} ]] && continue
        visited[$current]=1
        local name
        name=$(property "$app_bus" "$current" Name 2>/dev/null) || return 1
        node_names["$current"]=$name
        names+="$name"$'\n'
        # The smoke asks for concrete names, not a catalogue dump.  As soon
        # as all requested nodes are present we have enough real object paths
        # to exercise the action, and should not make the release gate crawl
        # a large folder/resource catalogue one D-Bus round trip at a time.
        # The hard 256-node ceiling below remains the corruption guard when a
        # requested node is absent.
        all_found=true
        for expected in "${required[@]}"; do
            node_named "$expected" >/dev/null || {
                all_found=false
                break
            }
        done
        "$all_found" && return 0
        while IFS= read -r child_path; do
            # Depth-first keeps a freshly opened overlay adjacent to its
            # parent, ahead of a potentially large file-list subtree.
            [[ -n "$child_path" ]] && pending=("$child_path" "${pending[@]}")
        done < <(
            children "$app_bus" "$current" 2>/dev/null \
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
    missing=""
    for attempt in $(seq 1 200); do
        if collect_tree "$@"; then
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
# then observe the application state it changed. `New Folder` appears only in
# the menu opened by View options, so a provider that merely advertises Click
# but fails to deliver it through accesskit_winit cannot pass.
click_named "View options"
wait_for_names "New Folder"

printf 'AT-SPI smoke passed: Files activity, toolbar controls, and View options action are exported.\n'
