# shellcheck shell=bash
# Action frames for the harnesses that drive a guest's session through
# `tilo-control raw`: the one wire its control socket answers actions on.

# The wire version the session's control socket speaks: the one its code
# declares, read from the tree the harness runs from, which is the tree the
# guest's image was built from. A copy of the number here went stale each time
# the wire moved, and the socket refused every frame for it.
action_frame_version() {
  local source
  source="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)/crates/core/actions/src/remote.rs"
  sed -n 's/^pub const REMOTE_ACTIONS_VERSION: u16 = \([0-9][0-9]*\);$/\1/p' "$source" | grep . \
    || { echo "action-frame: no REMOTE_ACTIONS_VERSION in $source" >&2; return 1; }
}

# The frame that invokes RESOURCE's ACTION of PROVIDER with ARGS (a JSON
# object, or nothing), its answer held up to WAIT_MS while it runs.
action_frame() {
  local provider=$1 resource=$2 action=$3 args=${4:-null} wait_ms=${5:-30000} version
  version="$(action_frame_version)" || return 1
  jq -cn --arg invocation "$(cat /proc/sys/kernel/random/uuid)" \
    --arg provider "$provider" --arg resource "$resource" --arg action "$action" \
    --argjson args "$args" --argjson wait_ms "$wait_ms" --argjson version "$version" \
    '{version: $version, message: {Invoke: {
        invocation: $invocation,
        target: {provider: $provider, address: {resource: $resource, action: $action}},
        argument: (if $args == null then "None" else {Json: $args} end),
        wait_ms: $wait_ms}}}'
}

# Whether the frame answer in FILE says its action completed; what it said
# instead goes to standard error.
action_completed() {
  jq -e '.message.Receipt.status | type == "object" and has("Completed")' "$1" >/dev/null \
    || { cat "$1" >&2; return 1; }
}
