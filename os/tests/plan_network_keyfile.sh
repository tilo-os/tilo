#!/usr/bin/env bash
# The NetworkManager connection a planned static network becomes
# (os/usr/libexec/tilo-plan-network): the installed server keeps it, and the
# sibling unlock's token carries it into early boot, so a machine whose only
# address is a static public one reaches the owner's devices before its disk
# is open. A plan is a document from the person's devices; a malformed one is
# refused rather than written for NetworkManager to read as root.
#
#   bash os/tests/plan_network_keyfile.sh
#
# Works from the source tree, needs no root and no NetworkManager.
set -uo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)"
helper="$repo/os/usr/libexec/tilo-plan-network"
fail=0
ok()  { printf 'ok    %s\n' "$*"; }
bad() { printf 'FAIL  %s\n' "$*" >&2; fail=1; }

work="$(mktemp -d)"
trap 'rm -rf "$work"' EXIT

write() { # NAME ADDRESSES GATEWAY DNS [INTERFACE]
  local name=$1
  shift
  "$helper" "$work/$name" "$@" >"$work/$name.log" 2>&1
}
refused() { # NAME DESCRIPTION [args...]
  local name=$1 description=$2
  shift 2
  if "$helper" "$work/$name" "$@" >"$work/$name.log" 2>&1; then
    bad "$description was accepted"
  elif [ -e "$work/$name" ]; then
    bad "$description was refused after a keyfile was written"
  else
    ok "$description is refused"
  fi
}

# --- one family, with its gateway -------------------------------------------
if write v4 '65.108.204.148/26' '65.108.204.129' '185.12.64.1,185.12.64.2' enp3s0; then
  ok "an IPv4 plan becomes a connection"
else
  bad "an IPv4 plan was refused: $(cat "$work/v4.log")"
fi
grep -qx 'interface-name=enp3s0' "$work/v4" && ok "the connection is bound to the planned link" \
  || bad "the connection names no interface"
grep -qx 'address1=65.108.204.148/26,65.108.204.129' "$work/v4" \
  && ok "the address carries its gateway" || bad "the address lost its gateway"
grep -qx 'method=manual' "$work/v4" && ok "the address is static" || bad "the address is not static"
grep -qx 'dns=185.12.64.1;185.12.64.2;' "$work/v4" \
  && ok "both DNS servers are kept" || bad "the DNS servers were changed"
grep -q '^method=ignore' "$work/v4" && ok "the family the plan left out is ignored" \
  || bad "an unplanned family would take an address of its own"
[ "$(stat -c %a "$work/v4")" = 600 ] \
  && ok "the keyfile is owner-only" || bad "the keyfile is readable by others"

# --- IPv6 alone -------------------------------------------------------------
write v6 '2a01:4f9:1a:a083::2/64' 'fe80::1' '' \
  && ok "an IPv6 plan becomes a connection" || bad "an IPv6 plan was refused"
grep -qx 'address1=2a01:4f9:1a:a083::2/64,fe80::1' "$work/v6" \
  && ok "the IPv6 address carries its gateway" || bad "the IPv6 gateway was dropped"
grep -qx 'method=manual' "$work/v6" || bad "the IPv6 side is not static"

# --- both families, a gateway each -----------------------------------------
write dual '65.108.204.148/26,2a01:4f9:1a:a083::2/64' '65.108.204.129,fe80::1' '185.12.64.1,2001:4860:4860::8888' \
  && ok "a dual-stack plan becomes a connection" || bad "a dual-stack plan was refused: $(cat "$work/dual.log")"
grep -qx 'address1=65.108.204.148/26,65.108.204.129' "$work/dual" \
  && ok "each gateway stays on the address family it belongs to" || bad "the IPv4 gateway went to the wrong family"
grep -qx 'address1=2a01:4f9:1a:a083::2/64,fe80::1' "$work/dual" \
  && ok "the other family gets its own gateway and starts at one" || bad "the other family's gateway is missing"
grep -qx 'dns=2001:4860:4860::8888;' "$work/dual" \
  && ok "each DNS server goes to its own family" || bad "an IPv6 DNS server was written under IPv4"

# One gateway for a dual-stack plan still goes to its family alone.
write one-gateway '10.0.0.5/24,2001:db8::5/64' '2001:db8::1' '' \
  && ok "a plan with one gateway for two families is accepted" || bad "one gateway was refused"
grep -qx 'address1=10.0.0.5/24' "$work/one-gateway" \
  && ok "the unaddressed family gets no gateway" || bad "a gateway was invented for IPv4"
grep -qx 'address1=2001:db8::5/64,2001:db8::1' "$work/one-gateway" \
  && ok "the IPv6 address carries its gateway" || bad "the IPv6 gateway was dropped"

# --- no interface: the connection matches any wired link -------------------
write any '10.0.0.5/24' '' '' && ok "a plan without a named link is still a connection" \
  || bad "a plan without a named link was refused"
grep -q '^interface-name=' "$work/any" && bad "an unnamed link was pinned to an interface" \
  || ok "an unnamed link is not pinned"

# --- refusals ---------------------------------------------------------------
refused bad-address "an address without a prefix" '10.0.0.5' '' ''
refused bad-prefix "an address with a non-numeric prefix" '10.0.0.5/x' '' ''
refused no-address "a plan with no address" '' '10.0.0.1' ''
refused bad-gateway "a gateway that is not an address" '10.0.0.5/24' 'not-an-address' ''
refused gateway-family "an IPv6 gateway with no IPv6 address" '10.0.0.5/24' 'fe80::1' ''
refused two-v4-gateways "two IPv4 gateways" '10.0.0.5/24' '10.0.0.1,10.0.0.2' ''
refused two-v6-gateways "two IPv6 gateways" '2001:db8::5/64' '2001:db8::1,2001:db8::2' ''
refused bad-dns "a DNS server that is not an address" '10.0.0.5/24' '' 'dns.example'
refused bad-interface "an interface name with a shell metacharacter" '10.0.0.5/24' '' '' 'eth0;reboot'
refused injection "an address carrying a shell command" '10.0.0.5/24;reboot' '' ''
if "$helper" "$work/missing" >/dev/null 2>&1; then bad "a call without a destination was accepted"; else
  ok "a call without a destination is refused"
fi

exit "$fail"
