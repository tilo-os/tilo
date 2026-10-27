#!/usr/bin/env bash
# tilo-boot-health judges a node, which has no sign-in screen at all, by the
# node answering: not by its units being active, which is what a relay that is
# up and serving nothing looks like.
#
#   ./os/tests/boot_health_node.sh
#
# Each case runs the production script against a systemctl stand-in and a
# stand-in for tilo-node-readiness that plays one node's answer ("it serves",
# "it does not answer", "nothing was published"), and asserts the verdict.
# What only a booted machine can show — the real relay publishing its record,
# the record being signed, the host answering on the wire — is
# tilo-relay-client's own tests and the node's end-to-end runs.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
health_script="$repo/os/usr/libexec/tilo-boot-health"
fail=0
ok() { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT
bin="$work/bin"
mkdir -p "$bin"

# One node history per case, read from the environment:
#   RELAY    yes|no|failed   what is-active / is-failed say about the relay
#   SESSION  yes|no          ...about the session beside it
#   ANSWERS  serves|<reason> what tilo-node-readiness prints, and whether it
#            succeeds: `serves` exits 0, anything else exits 1 with that text.
cat >"$bin/systemctl" <<'EOF'
#!/usr/bin/env bash
case "$1" in
  cat) exit 0 ;;
  is-active)
    [ "$2" = --quiet ] && shift
    case "$2" in
      tilo-node-relay@*) [ "$RELAY" = yes ] ;;
      tilo-node-session@*) [ "$SESSION" = yes ] ;;
      *) exit 1 ;;
    esac ;;
  is-failed)
    [ "$2" = --quiet ] && shift
    case "$2" in
      tilo-node-relay@*) [ "$RELAY" = failed ] ;;
      *) exit 1 ;;
    esac ;;
  *) exit 0 ;;
esac
EOF

cat >"$bin/readiness" <<'EOF'
#!/usr/bin/env bash
printf '%s\n' "$ANSWERS"
case "$ANSWERS" in
  serves*) exit 0 ;;
  *) exit 1 ;;
esac
EOF
chmod 755 "$bin/systemctl" "$bin/readiness"

# The node's own file, as a provisioned node writes it.
marker="$work/health"
printf '%s\n' \
  'TILO_NODE_HEALTH_UNIT=tilo-node-relay@quickuser.service' \
  "TILO_NODE_READINESS=$work/readiness.json" \
  'TILO_NODE_SESSION=tilo-node-session@quickuser.service' >"$marker"

# Runs the script; leaves its exit status in $status and its output in $out.
judge() {
  out="$(env -i PATH="$bin:/usr/bin:/bin:/usr/sbin:/sbin" \
    RELAY=yes SESSION=yes ANSWERS=serves \
    TILO_NODE_HEALTH_MARKER="$marker" \
    TILO_NODE_READINESS_PROGRAM="$bin/readiness" \
    TILO_BOOT_HEALTH_BOARD_MARKER="$work/no-board" \
    TILO_BOOT_HEALTH_TIMEOUT=2 TILO_BOOT_HEALTH_NODE_DEADLINE=3 \
    TILO_BOOT_HEALTH_POLL=1 \
    "$@" bash "$health_script" 2>&1)"
  status=$?
}

judge
[ "$status" = 0 ] && grep -q 'this boot is good' <<<"$out" \
  && ok "a node that answers, with its session running, is a good boot" \
  || bad "a node that answers was judged '$status': $out"

# The point of the whole check: the units are fine and the node answers
# nothing, which is what a broken update leaves behind.
judge ANSWERS='the node does not answer: nothing answered the readiness handshake'
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'answered nothing' <<<"$out" \
  && ok "a relay that is active and answers nothing is a failed boot" \
  || bad "a silent node was judged '$status': $out"

judge ANSWERS='the node published no readiness: nothing within 90s'
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'never reached serving' <<<"$out" \
  && ok "a relay that never reached serving is a failed boot" \
  || bad "an unstarted node was judged '$status': $out"

judge ANSWERS="not this node's readiness: the record was written by another"
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'written by another node' <<<"$out" \
  && ok "a record another node signed says nothing about this one" \
  || bad "another node's record was judged '$status': $out"

judge ANSWERS='another host answers there: 00112233'
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'another host answered' <<<"$out" \
  && ok "a different host on the address is not this node" \
  || bad "another host's answer was judged '$status': $out"

# A session that is not running means the apps the node serves are not there,
# and answering alone does not make that a working node.
judge SESSION=no
[ "$status" != 0 ] && [ "$status" != 124 ] \
  && grep -q "tilo-node-session@quickuser.service did not come up" <<<"$out" \
  && ok "a node whose session is not running is a failed boot" \
  || bad "a node without its session was judged '$status': $out"

judge RELAY=failed
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'did not come up' <<<"$out" \
  && ok "a relay that failed is a failed boot, whatever the answer would be" \
  || bad "a failed relay was judged '$status': $out"

# A node that answers a moment after the check starts is a good boot: the
# check polls while the relay finishes starting.
cat >"$bin/readiness" <<'EOF'
#!/usr/bin/env bash
count_file="$ANSWERS.count"
looks=$(( $(cat "$count_file" 2>/dev/null || echo 0) + 1 ))
echo "$looks" >"$count_file"
if [ "$looks" -ge 2 ]; then
  echo "serves"
  exit 0
fi
echo "the node does not answer: not yet"
exit 1
EOF
chmod 755 "$bin/readiness"
judge ANSWERS="$work/late"
[ "$status" = 0 ] && ok "a node that answers on a later poll is a good boot" \
  || bad "a node that answered late was judged '$status': $out"

# A machine that is not a node keeps the desktop's verdict: the file is what
# makes it one, so removing it must not break every desktop boot.
rm -f "$marker"
judge
[ "$status" != 0 ] && grep -q 'greetd.service did not become active' <<<"$out" \
  && ok "without the node's file the machine is judged as the desktop it is" \
  || bad "a machine with no node file was judged '$status': $out"

# A file that names nothing cannot be observed, and a check that cannot observe
# its subject has to fail rather than skip: the verdict under it keeps an
# update for good.
printf '%s\n' 'TILO_NODE_HEALTH_UNIT=tilo-node-relay@quickuser.service' >"$marker"
judge
[ "$status" != 0 ] && [ "$status" != 124 ] && grep -q 'names no TILO_NODE_HEALTH_UNIT or' <<<"$out" \
  && ok "a node file naming no readiness fails the boot instead of skipping it" \
  || bad "an unobservable node file was judged '$status': $out"

exit "$fail"
