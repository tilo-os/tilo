#!/usr/bin/env bash
# The Atomic-host conflict precheck: a path another package owns is reported
# with its owner and refuses the deployment, and a payload of only Tilo's own
# files passes. Runs against a fixture ownership table, so it needs no RPM
# database, no rpm-ostree, no root and no built RPMs.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
helper="$repo_root/dist/rpm/ostree-package-conflicts.sh"
work=$(mktemp -d)
trap 'rm -rf -- "$work"' EXIT
mkdir -p "$work/rpms/x86_64" "$work/bin" "$work/root"
: >"$work/rpm-owners"
: >"$work/rpm-payload"
: >"$work/rpm-probes"

fail() { echo "test-ostree-package-conflicts: $*" >&2; exit 1; }

# The format of the fixture ownership table: "OWNER PATH" per line.
# The payload table holds the path list of one staged fixture RPM.
#
# `rpm --eval` names the fixture payload architecture; `rpm -qf` reads the
# ownership table; `rpm -qlp` reads the payload table. Every invocation is
# logged, because the helper discards rpm's stderr and a fixture that never ran
# would otherwise look like "not owned by anyone".
cat >"$work/bin/rpm" <<'RPMFIXTURE'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$*" >>"$FIXTURE/rpm-probes"
case ${1:-} in
    --eval) printf 'x86_64\n'; exit 0 ;;
    -qf|-qlp) ;;
    *) echo "fixture rpm: unexpected ${1:-}" >&2; exit 2 ;;
esac
query=$1
shift
[[ ${1:-} == -- ]] && shift
path=${@: -1}
# rpm -qlp lists what one fixture RPM installs; rpm -qf names the owner of a
# path this host already has.
if [[ $query == -qlp ]]; then
    cat "$FIXTURE/rpm-payload"
    exit 0
fi
table=$FIXTURE/rpm-owners
while IFS=' ' read -r owner listed; do
    [[ $listed == "$path" ]] && { printf '%s\n' "$owner"; exit 0; }
done <"$table"
exit 1
RPMFIXTURE
export FIXTURE="$work"
chmod +x "$work/bin/rpm"

# TILO_PAYLOAD_ROOT is the filesystem root the payload lands in; the fixture
# points it at a directory that holds the paths under test, so this never
# touches /usr on the machine running it.
run_helper() {
    PATH="$work/bin:$PATH" RPM_DIR="$work/rpms" TILO_PAYLOAD_ROOT="$work/root" \
        "$helper" "$@"
}
staged_rpm=$work/rpms/x86_64/tilo-session-0.1.5-1.fc44.x86_64.rpm

# 1. The inspected list is payload paths only, and reading it queries nothing.
files_only=$(run_helper --files-only)
[[ ! -s $work/rpm-probes ]] || fail "--files-only queried the RPM database"
[[ -n $files_only ]] || fail "payload list is empty"
for path in $files_only; do
    case $path in
        /usr/* | /etc/*) ;;
        *) fail "payload list carries a non-payload path: $path" ;;
    esac
done

# 2. No payload present: the check passes and says so.
run_helper >"$work/out" 2>&1 || fail "clean host was rejected: $(cat "$work/out")"
grep -Fq 'No path in the tilo-session payload is owned by another package' "$work/out" \
    || fail "clean host did not report a pass: $(cat "$work/out")"

# 3. A staged payload path is found even with no Tilo install, and the report
# names the path and the package that owns it.
#
# The compositor is the path an older release replaced with a different file,
# and the session target is the path a host package can ship itself. Both live
# under the fixture root, so the fixture works on any machine.
installed=/usr/bin/tilo-shell
mkdir -p "$work/root/usr/bin"
: >"$work/root$installed"
printf '%s\n' "gnome-shell $work/root$installed" >"$work/rpm-owners"
printf '%s\n' "payload $installed" >"$work/rpm-payload"
: >"$staged_rpm"
if run_helper >"$work/out" 2>&1; then
    fail "a payload path owned by another package was accepted"
fi
grep -Fq "$installed belongs to gnome-shell" "$work/out" \
    || fail "conflict report does not name the owner: $(cat "$work/out")"

# 4. The report carries the two real next steps, not just the refusal.
grep -Fq 'sudo rpm-ostree update' "$work/out" \
    || fail "conflict report does not give the refresh step: $(cat "$work/out")"
grep -Fq 'sudo rpm-ostree uninstall tilo-session' "$work/out" \
    || fail "conflict report does not give the replacement step: $(cat "$work/out")"

# 5. The same path owned by a Tilo subpackage is not a conflict: an upgrade must
# not be refused because the previous release left its own files in place.
printf '%s\n' "tilo-session-apps $work/root$installed" >"$work/rpm-owners"
run_helper >"$work/out" 2>&1 || fail "a Tilo subpackage owner was treated as a conflict: $(cat "$work/out")"

# 6. Without a built payload directory, the running host alone still answers.
rm -rf "$work/rpms"
printf '%s\n' "host-package $work/root$installed" >"$work/rpm-owners"
if run_helper >"$work/out" 2>&1; then
    fail "a host-owned payload path was accepted with no staged RPMs"
fi
grep -Fq "$installed belongs to host-package" "$work/out" \
    || fail "host conflict not reported without staged RPMs: $(cat "$work/out")"

# 7. A file at a payload path that no package owns is still a conflict: rpm-ostree
# cannot layer over an unowned file either, and silence would be the wrong answer.
rm -rf "$work/root/usr/bin/tilo-shell"
mkdir -p "$work/root/usr/lib/systemd/user"
: >"$work/root/usr/lib/systemd/user/tilo-graphical-session.target"
: >"$work/rpm-owners"
printf '%s
' "payload /usr/lib/systemd/user/tilo-graphical-session.target" >"$work/rpm-payload"
if run_helper >"$work/out" 2>&1; then
    fail "an unowned file at a payload path was accepted"
fi
grep -Fq '/usr/lib/systemd/user/tilo-graphical-session.target exists but no package owns it' "$work/out" \
    || fail "unowned conflict not reported: $(cat "$work/out")"

echo "Atomic-host package conflict precheck: PASS"
