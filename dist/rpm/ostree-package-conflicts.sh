#!/usr/bin/env bash
# Report every path the packaged `tilo-session` payload would replace that
# another package owns.
#
# `rpm-ostree install` layers the session into the next deployment of an
# immutable host (Silverblue, Kinoite, Bazzite, an rpm-ostree derivative). A
# path in the payload that another package already owns fails the whole
# transaction, and rpm-ostree reports it only inside a long deployment log:
#
#   error: Checkout of "lib/...": Hardlinking from ...: File exists
#     The base rpm database and the layered files conflict on
#     /usr/lib/systemd/user/tilo-graphical-session.target
#
# The receiver then has no way to tell "Tilo is broken" from "this host ships a
# file at one of Tilo's paths". This check answers that before the next
# transaction: it names each conflicting path and its owner, installs nothing,
# and refuses with a non-zero exit so a script can branch on it.
#
#   dist/rpm/ostree-package-conflicts.sh          # the report
#   dist/rpm/ostree-package-conflicts.sh --files-only   # what it inspects
#
# RPM_DIR and RPM_ARCH select a staged payload other than the native one under
# dist/rpm/build/RPMS; a checkout that has not built RPMs is checked against the
# running host alone.
set -euo pipefail

repo_root=$(cd "$(dirname "${BASH_SOURCE[0]}")/../.." && pwd)
rpm_dir=${RPM_DIR:-$repo_root/dist/rpm/build/RPMS}
# The filesystem root the payload lands in. It is / on a real host; the test
# fixture points it at a directory holding the paths it wants to exercise.
fs_root=${TILO_PAYLOAD_ROOT:-}

# The paths the tilo-session packages install and replace in place. Files the
# payload creates under a catalog (systemd units, desktop entries, the portal
# directory) are not listed: a host that adds its own unit beside them does not
# collide, and the packaged payload is what dist/verify-payload.sh asserts.
payload=(
    /usr/bin/tilo-shell
    /usr/bin/tilo-launcher
    /usr/libexec/tilo-import-folders
    /usr/libexec/tilo-session
    /usr/libexec/tilo-session-runner
    /usr/libexec/tilo-session-report
    /usr/libexec/tilo-flight-recorder
    /usr/libexec/tilo-unlock-helper
    /etc/pam.d/tilo-unlock
    /usr/share/applications/tilo-import-folders.desktop
    /usr/share/wayland-sessions/tilo.desktop
    /usr/share/xdg-desktop-portal/portals/tilo.portal
    /usr/share/xdg-desktop-portal/tilo-portals.conf
    /usr/share/pipewire/tilo-echo-cancel.conf
    /usr/lib/systemd/user/tilo-graphical-session.target
    /usr/share/licenses/tilo-session/LICENSE
)

case ${1:-} in
    --files-only)
        printf '%s\n' "${payload[@]}"
        exit 0
        ;;
    "") ;;
    *) echo "usage: $0 [--files-only]" >&2; exit 2 ;;
esac

command -v rpm >/dev/null || {
    echo "rpm is not installed; this check reads the package database of an rpm-ostree host" >&2
    exit 1
}
rpm_arch=${RPM_ARCH:-$(rpm --eval '%{_arch}')}

# A path a Tilo package or one of its subpackages owns is legitimately ours.
# Anything else that occupies the path — another package, or a file no package
# claims — is a conflict.
owner_of() {
    # pipefail is on: a missing database is a failed query, not "no owner".
    rpm -qf --qf '%{NAME}\n' -- "$fs_root$1" 2>/dev/null
}
# Either the file is on this host and another package owns it, or a staged RPM
# installs it and another package owns the live path. A staged path that no
# package owns yet is Tilo's to create.
is_conflict() {
    local file=$1 owner
    [[ -e $fs_root$file ]] || return 1
    if ! owner=$(owner_of "$file"); then
        return 0
    fi
    [[ -z $owner ]] && return 0
    [[ $owner != tilo-session* ]]
}

declare -a conflicts=()
add_conflict() {
    if is_conflict "$1"; then conflicts+=("$1"); fi
}
for file in "${payload[@]}"; do
    add_conflict "$file"
done

# Reading the staged RPMs finds a conflict before Tilo is installed at all, and
# answers the same question the transaction will ask: whose file is this? A
# missing payload directory is the ordinary case on a host that has not built
# from source, and it leaves the running host answering alone.
staged_dir=$rpm_dir/$rpm_arch
[[ -d $staged_dir ]] || staged_dir=""
if [[ -n $staged_dir ]]; then
    while IFS= read -r rpm_file; do
        [[ -n $rpm_file ]] || continue
        while IFS= read -r file; do
            [[ -n $file ]] || continue
            add_conflict "$file"
        done < <(rpm -qlp -- "$rpm_file" 2>/dev/null)
    done < <(find "$staged_dir" -maxdepth 1 -type f -name 'tilo-session-[0-9]*.rpm' | sort)
fi

if (( ${#conflicts[@]} == 0 )); then
    echo "No path in the tilo-session payload is owned by another package."
    exit 0
fi

# A path can be reached both from the running host and from a staged RPM.
sorted=()
while IFS= read -r file; do
    sorted+=("$file")
done < <(printf '%s\n' "${conflicts[@]}" | sort -u)
conflicts=(${sorted[@]+"${sorted[@]}"})

{
    echo "The payload replaces files this host already has, so rpm-ostree would reject the deployment."
    for file in "${conflicts[@]}"; do
        if owner=$(owner_of "$file") && [[ -n $owner ]]; then
            printf '  %s belongs to %s\n' "$file" "$owner"
        else
            printf '  %s exists but no package owns it\n' "$file"
        fi
    done
    echo
    echo "An existing Tilo layering is refreshed in place, not layered beside the older release:"
    echo "  sudo rpm-ostree update"
    echo "To move to this release from an older layered one, replace the layering:"
    echo "  sudo rpm-ostree uninstall tilo-session && sudo rpm-ostree install tilo-session"
    echo "A host package that owns one of these paths must be removed first; the path cannot be shared."
} >&2
exit 1
