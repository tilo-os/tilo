#!/usr/bin/env bash
# Assert that printing is isolated the way docs/printing-threat-model.md says.
#
# Every check here is written so that *not being able to look* is a failure.
# This project has been bitten three separate times by verification that
# passed because it could not observe its subject, so a missing file, an
# unreadable unit or an absent tool is a FAIL with a reason, never a skip.
#
# Exit codes:
#   0 — every invariant holds.
#   1 — at least one failed; each is reported on stderr.

set -uo pipefail

TEST_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
OS_ROOT="${OS_ROOT:-$(cd "$TEST_DIR/.." && pwd)}"

CONFINEMENT="$OS_ROOT/usr/lib/systemd/system/cups.service.d/tilo-confinement.conf"
SOCKET_DROPIN="$OS_ROOT/usr/lib/systemd/system/cups.socket.d/tilo-local-only.conf"
CUPSD_CONF="$OS_ROOT/etc/cups/cupsd.conf"
CONTAINERFILE="$OS_ROOT/Containerfile"
SCANNER_RS="$OS_ROOT/../crates/platform/tilo-capture/src/scanner.rs"
PORTAL_PRINT_RS="$OS_ROOT/../crates/session/shell/src/portal/print.rs"

fails=0
checks=0

fail() {
    echo "FAIL: $*" >&2
    fails=$((fails + 1))
}

ok() {
    checks=$((checks + 1))
}

# Read a file or fail loudly. An absent file must never read as "the thing we
# were worried about is not there".
need_file() {
    local path="$1" what="$2"
    if [ ! -r "$path" ]; then
        fail "cannot read $what at $path — this check cannot observe its subject"
        return 1
    fi
    return 0
}

# `grep -q` for a directive that must be present.
must_contain() {
    local path="$1" pattern="$2" why="$3"
    need_file "$path" "$(basename "$path")" || return
    if grep -Eq "$pattern" "$path"; then
        ok
    else
        fail "$(basename "$path") is missing /$pattern/ — $why"
    fi
}

# ...and one that must be absent.
must_not_contain() {
    local path="$1" pattern="$2" why="$3"
    need_file "$path" "$(basename "$path")" || return
    if grep -Eq "$pattern" "$path"; then
        fail "$(basename "$path") contains /$pattern/ — $why"
    else
        ok
    fi
}

echo "== the scheduler is confined =="
for directive in \
    'NoNewPrivileges=yes' \
    'ProtectSystem=strict' \
    'ProtectHome=yes' \
    'PrivateTmp=yes' \
    'RestrictNamespaces=yes' \
    'RestrictRealtime=yes' \
    'RestrictSUIDSGID=yes' \
    'LockPersonality=yes' \
    'ProtectKernelTunables=yes' \
    'ProtectKernelModules=yes' \
    'ProtectKernelLogs=yes' \
    'ProtectControlGroups=yes' \
    'ProtectClock=yes' \
    'ProtectHostname=yes' \
    'MemoryDenyWriteExecute=yes' \
    'SystemCallArchitectures=native' \
    'CapabilityBoundingSet=' \
    'RestrictAddressFamilies=' \
    'DevicePolicy=closed' \
; do
    must_contain "$CONFINEMENT" "^${directive}" "the scheduler must be confined by it"
done

# The capability set must be an allow list, not the whole of root.
must_not_contain "$CONFINEMENT" '^CapabilityBoundingSet=~?$' \
    "an empty bounding set line grants everything it was meant to remove"
must_not_contain "$CONFINEMENT" 'CAP_SYS_ADMIN' \
    "a print scheduler has no business with CAP_SYS_ADMIN"
must_not_contain "$CONFINEMENT" 'CAP_NET_ADMIN' \
    "a print scheduler has no business with CAP_NET_ADMIN"
must_not_contain "$CONFINEMENT" 'CAP_NET_BIND_SERVICE' \
    "this cupsd binds no privileged port"

echo "== nothing listens on the network =="
must_contain "$SOCKET_DROPIN" '^ListenStream=/run/cups/cups.sock' \
    "the local socket must be the only door"
must_not_contain "$SOCKET_DROPIN" '^ListenStream=[^/]' \
    "a non-path ListenStream is a TCP listener"
must_contain "$CUPSD_CONF" '^Listen /run/cups/cups.sock' \
    "cupsd.conf must bind the local socket"
must_not_contain "$CUPSD_CONF" '^[[:space:]]*Port[[:space:]]' \
    "a Port directive turns the desktop into an IPP server"
must_not_contain "$CUPSD_CONF" '^[[:space:]]*Listen[[:space:]]+[^/]' \
    "a Listen on an address turns the desktop into an IPP server"

echo "== discovery is off, and cups-browsed is not installed =="
must_contain "$CUPSD_CONF" '^Browsing Off' \
    "browsing advertises this machine's printers"
must_contain "$CUPSD_CONF" '^DefaultShared No' \
    "a client must not be able to enable sharing behind the user's back"
must_not_contain "$CONTAINERFILE" '^[[:space:]]*cups-browsed[[:space:]]*\\?$' \
    "cups-browsed accepts remote advertisements and creates local queues from them"
must_not_contain "$CONTAINERFILE" 'systemctl enable cups-browsed' \
    "cups-browsed must never be enabled"
# Avahi is what DNS-SD discovery would need. Its absence is load-bearing.
must_not_contain "$CONTAINERFILE" '^[[:space:]]*avahi[[:space:]]*\\?$' \
    "installing avahi would give printer discovery a transport"

echo "== the firewall opens nothing for printing =="
must_not_contain "$CONTAINERFILE" 'add-service=ipp' \
    "printing must not open a port; it is outbound only"

echo "== cupsd is socket-activated, not always running =="
must_contain "$CONTAINERFILE" 'systemctl enable cups\.socket' \
    "the socket, not the service, so the IPP parser is not running on idle machines"
must_not_contain "$CONTAINERFILE" 'systemctl enable cups\.service' \
    "enabling the service defeats socket activation"

echo "== filters run unprivileged =="
must_contain "$CUPSD_CONF" '^User lp' \
    "filter processes touch document bytes and vendor PPD code"
must_contain "$CUPSD_CONF" '^Group lp' \
    "filter processes touch document bytes and vendor PPD code"

echo "== scanner helpers are resource bounded =="
must_contain "$SCANNER_RS" 'run_prepared_with_limits' \
    "scan acquisition needs independent image and diagnostic ceilings"
must_contain "$SCANNER_RS" 'SCAN_TIMEOUT' \
    "a wedged or malicious scanner backend must not pin the caller forever"
must_not_contain "$SCANNER_RS" 'wait_with_output\(\)' \
    "raw scanimage execution has no deadline or descendant cleanup"
must_contain "$PORTAL_PRINT_RS" 'run_prepared_with_input_limit' \
    "untrusted print documents need bounded spooler input and descendant cleanup"
must_not_contain "$PORTAL_PRINT_RS" 'wait_with_output\(\)|\.output\(\)' \
    "the print portal contains an unbounded spooler capture"

echo "== apps reach printing through the portal, never the socket =="
# The whole point of the Print portal: no sandbox may be handed the cups
# socket. Sandboxes are built by tilo-launcher (its bind plan, and the
# shared confinement primitives it uses); if a bind for the socket ever
# appears there, this fails.
SANDBOX_SRCS=(
    "$OS_ROOT/../crates/packaging/launcher/src"
    "$OS_ROOT/../crates/system/sandbox-core/src"
)
if [ -d "${SANDBOX_SRCS[0]}" ] && [ -d "${SANDBOX_SRCS[1]}" ]; then
    if grep -rEq 'cups\.sock|/run/cups' "${SANDBOX_SRCS[@]}"; then
        fail "the sandbox builder references the cups socket — apps must go through the Print portal"
    else
        ok
    fi
else
    # Running from inside the image, where the Rust sources are absent. The
    # invariant still has to be checked against something observable, so
    # check the shipped launcher binary instead of quietly passing.
    if [ -x "$OS_ROOT/usr/libexec/tilo/tilo-launcher" ]; then
        if strings "$OS_ROOT/usr/libexec/tilo/tilo-launcher" 2>/dev/null | grep -q '/run/cups'; then
            fail "the shipped launcher binary references /run/cups"
        else
            ok
        fi
    else
        fail "neither the launcher sources nor /usr/libexec/tilo/tilo-launcher is present — cannot check that the cups socket stays out of sandboxes"
    fi
fi

echo
if [ "$fails" -gt 0 ]; then
    echo "printing isolation: $fails failed, $checks passed" >&2
    exit 1
fi
echo "OK: printing isolation ($checks checks)"
