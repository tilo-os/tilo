#!/usr/bin/env bash
# Replace the tilo-session packages layered on this rpm-ostree host with ones
# built from this checkout, in one transaction.
#
#   dist/rpm/host-update.sh                build from HEAD, stage the swap
#   dist/rpm/host-update.sh --no-cargo     repackage binaries already in target/release
#                                          (and build the app collection if HEAD has none)
#   dist/rpm/host-update.sh --no-build     install RPMs already built for HEAD
#   dist/rpm/host-update.sh --apply-live   also make it the running /usr (no reboot)
#   dist/rpm/host-update.sh --stage-only   leave the deployment for shutdown to write
#   dist/rpm/host-update.sh --dry-run      print what would run
#
# The packages are released as 0.<commit time>.git<commit>, so `rpm -q
# tilo-session` answers "which tree am I logged into" and a stale RPM left in
# dist/rpm/build from another commit is refused rather than installed.
#
# An existing layer is replaced with `rpm-ostree uninstall OLD --install NEW`.
# Uninstalling and installing as separate transactions leaves a deployment
# without a session entry in between, and plain `install` refuses a package
# whose name is already layered. The developer subpackage follows what is
# layered now; --devel / --no-devel override that.
#
# Without --apply-live the new packages take effect at the next boot. With it,
# rpm-ostree applies the new deployment to the running system, so logging out
# and choosing "Tilo" at the greeter starts the new build. Do not apply live
# while a Tilo session is running on this machine: its binary is replaced
# underneath it.
#
# rpm-ostree only stages the deployment; ostree writes it at a clean shutdown,
# so a hard reset or a hang discards it and boots the old packages. The helper
# therefore finalizes the deployment at once (`ostree admin finalize-staged`),
# which merges /etc as it is now. --stage-only keeps rpm-ostree's behaviour and
# says loudly that the install is not yet durable.
#
# The runtime bundle's trusted publisher lives in /etc and changes with every
# local build. /etc is merged three ways, so a copy that was ever changed stays
# as it is and the launcher refuses the new runtime; the helper copies the
# packaged key into both the running /etc (with --apply-live) and the new
# deployment's /etc.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
rpm_ostree=${TILO_RPM_OSTREE:-rpm-ostree}
ostree=${TILO_OSTREE:-ostree}
sysroot=${TILO_SYSROOT:-}
trust=etc/tilo/runtime-bundle-trusted-publishers.txt
build_rpm=${TILO_BUILD_RPM:-$repo_root/dist/rpm/build-rpm.sh}
out=${TILO_PACKAGE_OUT:-$repo_root/dist/rpm/build}
version=$(<"$repo_root/dist/VERSION")

build=cargo apply_live=0 dry_run=0 devel=auto allow_dirty=0 finalize=1
usage() { sed -n '2,39p' "${BASH_SOURCE[0]}" | sed 's/^# \{0,1\}//'; }
while (( $# )); do
    case $1 in
        --no-cargo) build=repackage ;;
        --no-build) build=none ;;
        --apply-live) apply_live=1 ;;
        --stage-only) finalize=0 ;;
        --dry-run) dry_run=1 ;;
        --devel) devel=yes ;;
        --no-devel) devel=no ;;
        --allow-dirty) allow_dirty=1 ;;
        -h|--help) usage; exit 0 ;;
        *) echo "host-update: unknown option $1" >&2; usage >&2; exit 2 ;;
    esac
    shift
done

die() { echo "host-update: $*" >&2; exit 1; }
say() { printf '\n== %s\n' "$*"; }
run() {
    printf '+'; printf ' %q' "$@"; printf '\n'
    (( dry_run )) || "$@"
}
as_root() { if [[ $(id -u) -eq 0 ]]; then run "$@"; else run sudo "$@"; fi; }

[[ -e /run/ostree-booted || -n ${TILO_RPM_OSTREE:-} ]] \
    || die "this is not an rpm-ostree host; install the RPMs with your package manager"

# ---------------------------------------------------------------------------
# 1. Name the build after its commit.
# ---------------------------------------------------------------------------
commit=$(git -C "$repo_root" rev-parse --short=10 HEAD)
stamp=$(TZ=UTC git -C "$repo_root" log -1 --format=%cd --date=format-local:%Y%m%d%H%M HEAD)
release=0.$stamp.git$commit
if [[ -n $(git -C "$repo_root" status --porcelain --untracked-files=no) ]]; then
    (( allow_dirty )) || die "the checkout has uncommitted changes; commit them or pass --allow-dirty"
    release=$release.dirty
fi

case $build in
    cargo) say "building tilo-session $version-$release"
           TILO_RPM_RELEASE=$release run "$build_rpm" ;;
    repackage) say "repackaging tilo-session $version-$release"
               TILO_RPM_RELEASE=$release run "$build_rpm" --no-cargo ;;
    none) ;;
esac

# ---------------------------------------------------------------------------
# 2. The RPMs for exactly this commit.
# ---------------------------------------------------------------------------
rpm_arch=$(rpm --eval '%{_arch}' 2>/dev/null || uname -m)
pick() {
    local name=$1 arch=$2 found
    found=$(find "$out/RPMS/$arch" -maxdepth 1 -type f \
        -name "$name-$version-$release.*.$arch.rpm" -print 2>/dev/null | sort)
    [[ $(wc -l <<<"$found") -le 1 ]] || die "more than one $name build for $release in $out/RPMS/$arch"
    printf '%s' "$found"
}
session_rpm=$(pick tilo-session "$rpm_arch")
apps_rpm=$(pick tilo-session-apps "$rpm_arch")
devel_rpm=$(pick tilo-session-devel "$rpm_arch")
if [[ -z $session_rpm || -z $apps_rpm ]] && (( ! dry_run )); then
    found=$(find "$out/RPMS" -name 'tilo-session-*.rpm' -printf '  %f\n' 2>/dev/null)
    die "no tilo-session $version-$release RPMs in $out/RPMS (built for HEAD $commit)${found:+; found:
$found}"
fi

# ---------------------------------------------------------------------------
# 3. What is layered now, on the deployment rpm-ostree will change.
# ---------------------------------------------------------------------------
# The first deployment is the pending one if there is one, else the booted one.
mapfile -t layered < <("$rpm_ostree" status --json | python3 -c '
import json, sys
dep = json.load(sys.stdin)["deployments"][0]
names = {"tilo-session", "tilo-session-apps", "tilo-session-devel"}
for pkg in dep.get("requested-local-packages") or []:
    name = pkg.rsplit(".", 1)[0].rsplit("-", 2)[0]
    if name in names:
        print(name)
for name in dep.get("requested-packages") or []:
    if name in names:
        print(name)
')

has_devel=0
for name in "${layered[@]}"; do [[ $name != tilo-session-devel ]] || has_devel=1; done
case $devel in
    yes) want_devel=1 ;;
    no) want_devel=0 ;;
    auto) want_devel=$has_devel ;;
esac
install=("$session_rpm" "$apps_rpm")
if (( want_devel )); then
    [[ -n $devel_rpm || $dry_run -eq 1 ]] || die "no tilo-session-devel $version-$release RPM"
    install+=("$devel_rpm")
fi

# ---------------------------------------------------------------------------
# 4. One transaction.
# ---------------------------------------------------------------------------
if (( apply_live )) && pgrep -x tilo-shell >/dev/null 2>&1; then
    die "a Tilo session is running; not replacing its binary live (reboot instead)"
fi
say "layering tilo-session $version-$release"
if (( ${#layered[@]} )); then
    args=()
    for rpm_file in "${install[@]}"; do args+=("--install=$rpm_file"); done
    as_root "$rpm_ostree" uninstall "${layered[@]}" "${args[@]}"
else
    as_root "$rpm_ostree" install "${install[@]}"
fi

copy_trust() {
    local root=$1
    as_root install -m 0644 "$root/usr/$trust" "$root/$trust"
    if [[ -z $sysroot ]] && command -v restorecon >/dev/null 2>&1; then
        as_root restorecon "$root/$trust"
    fi
}

if (( apply_live )); then
    say "applying the new deployment to the running system"
    as_root "$rpm_ostree" apply-live --allow-replacement
    copy_trust "$sysroot"
fi

# The pending deployment: whether it is still staged, and where it lives.
pending() {
    "$rpm_ostree" status --json | python3 -c '
import json, sys
dep = json.load(sys.stdin)["deployments"][0]
print("staged" if dep.get("staged") else "written",
      "/ostree/deploy/%s/deploy/%s.%d" % (dep["osname"], dep["checksum"], dep["serial"]))
'
}

if (( finalize )); then
    say "writing the deployment now rather than at shutdown"
    as_root "$ostree" admin finalize-staged || true
fi
(( ! dry_run )) || exit 0
read -r state deployment < <(pending)
if [[ $state == staged ]]; then
    cat >&2 <<'WARN'

!! host-update: the new deployment is only STAGED. ostree writes it at a clean
!! shutdown; a hard reset, a crash or a hung suspend discards it and the machine
!! boots the previous packages. Write it now with
!!     sudo ostree admin finalize-staged
!! then copy the new deployment's usr/etc/tilo/runtime-bundle-trusted-publishers.txt
!! over its etc/ copy.
WARN
    exit 1
fi
# A written deployment's /etc sits on the read-only /sysroot mount. Finalizing
# merged /etc as it is now, so after --apply-live it already matches; otherwise
# rewrite it in place (keeping its label) under a private writable remount, as
# rpm-ostree itself does.
target=$sysroot$deployment
if ! cmp -s "$target/usr/$trust" "$target/$trust"; then
    if [[ -n $sysroot ]]; then
        copy_trust "$target"
    else
        as_root unshare -m sh -c 'mount -o remount,rw /sysroot && cat "$1/usr/$2" >"$1/$2"' \
            sh "$target" "$trust"
    fi
fi

echo
if (( apply_live )); then
    echo "Installed. Log out and choose \"Tilo\" at the greeter."
else
    echo "Installed for the next boot. Reboot, then choose \"Tilo\" at the greeter (or rerun with --apply-live)."
fi
