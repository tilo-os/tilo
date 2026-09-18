#!/usr/bin/env bash
# Two-netns share pipeline test driver.
#
# Sets up two network namespaces bridged by a veth pair, then spawns
# the share host in nsA and the viewer in nsB. Frames flow host →
# viewer over the veth link; input events flow viewer → host over a
# second UDP port on the same link. Real network stack — no
# in-process shortcuts.
#
# Requires:
#   - root or `sudo` (ip netns operations need CAP_NET_ADMIN)
#   - a `cargo test` build of shell that includes the `it` integration
#     target's opt-in `netns-e2e` roles
#
# Usage:
#   ./scripts/two-netns-share.sh
#
# Set VETH_A_IP / VETH_B_IP to change the peer addresses.

set -euo pipefail

VETH_A_IP="${VETH_A_IP:-10.42.0.1}"
VETH_B_IP="${VETH_B_IP:-10.42.0.2}"
VIDEO_PORT="${VIDEO_PORT:-9080}"
INPUT_PORT="${INPUT_PORT:-9081}"
NS_A="${NS_A:-tilo-share-a}"
NS_B="${NS_B:-tilo-share-b}"

if [[ $EUID -ne 0 ]]; then
    echo "netns creation requires root; re-run under sudo." >&2
    exit 1
fi

cleanup() {
    ip netns del "$NS_A" 2>/dev/null || true
    ip netns del "$NS_B" 2>/dev/null || true
}
trap cleanup EXIT INT TERM

echo "== creating netns pair: $NS_A + $NS_B (veth $VETH_A_IP <-> $VETH_B_IP)"
ip netns add "$NS_A"
ip netns add "$NS_B"
ip link add vethA type veth peer name vethB
ip link set vethA netns "$NS_A"
ip link set vethB netns "$NS_B"
ip netns exec "$NS_A" ip addr add "$VETH_A_IP/24" dev vethA
ip netns exec "$NS_A" ip link set vethA up
ip netns exec "$NS_A" ip link set lo up
ip netns exec "$NS_B" ip addr add "$VETH_B_IP/24" dev vethB
ip netns exec "$NS_B" ip link set vethB up
ip netns exec "$NS_B" ip link set lo up

# Sanity check the link.
ip netns exec "$NS_A" ping -c 1 -W 2 "$VETH_B_IP" >/dev/null
echo "== veth link up"

# Compile before either timed role starts. Letting two `cargo test` processes
# race the build lock can consume the viewer's entire four-second window
# before the host has bound its socket.
cargo test -p shell --test it --features netns-e2e --locked --no-run

echo "== spawning viewer in $NS_B (background)"
ip netns exec "$NS_B" env \
    TWO_NETNS_ROLE=viewer \
    TWO_NETNS_VIDEO_ADDR="$VETH_B_IP:$VIDEO_PORT" \
    TWO_NETNS_INPUT_ADDR="$VETH_A_IP:$INPUT_PORT" \
    cargo test -p shell --test it --features netns-e2e --locked \
    -- --exact two_netns_share::netns_viewer_process --nocapture &
VIEWER_PID=$!

# Give the viewer a beat to bind.
sleep 1

echo "== spawning host in $NS_A"
ip netns exec "$NS_A" env \
    TWO_NETNS_ROLE=host \
    TWO_NETNS_VIDEO_ADDR="$VETH_B_IP:$VIDEO_PORT" \
    TWO_NETNS_INPUT_ADDR="$VETH_A_IP:$INPUT_PORT" \
    cargo test -p shell --test it --features netns-e2e --locked \
    -- --exact two_netns_share::netns_host_process --nocapture

wait $VIEWER_PID
echo "== netns pipeline test complete"
