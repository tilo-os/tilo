#!/usr/bin/env bash
# firewalld refuses to load a zone and a policy that share a name, and the image
# build stops at its first firewall-offline-cmd with NAME_CONFLICT. Every zone
# and policy the image ships has a name of its own.
#
#   ./os/tests/firewalld_names.sh
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
dir="$repo/os/usr/lib/firewalld"
fail=0

names="$(for kind in zones policies; do
  for f in "$dir/$kind"/*.xml; do
    [ -e "$f" ] || continue
    name="${f##*/}"
    printf '%s %s\n' "${name%.xml}" "$kind"
  done
done)"
clashes="$(awk '{ print $1 }' <<<"$names" | sort | uniq -d)"
if [ -n "$clashes" ]; then
  printf 'FAIL  a zone and a policy share a name, which firewalld refuses: %s\n' "$(tr '\n' ' ' <<<"$clashes")" >&2
  fail=1
else
  printf 'ok    every shipped firewalld zone and policy has its own name (%s)\n' "$(grep -c . <<<"$names")"
fi

# A policy's ingress and egress zones are shipped zones or firewalld's own.
for f in "$dir"/policies/*.xml; do
  [ -e "$f" ] || continue
  for zone in $(sed -n 's/.*<\(ingress\|egress\)-zone name="\([^"]*\)".*/\2/p' "$f"); do
    case "$zone" in ANY|HOST) continue ;; esac
    [ -e "$dir/zones/$zone.xml" ] && continue
    [ -e "/usr/lib/firewalld/zones/$zone.xml" ] && continue
    printf 'FAIL  %s names zone %s, which neither the image nor firewalld ships\n' "${f##*/}" "$zone" >&2
    fail=1
  done
done
[ "$fail" -ne 0 ] || printf 'ok    every shipped policy names zones that exist\n'
exit "$fail"
