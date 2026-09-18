#!/usr/bin/env bash
# Resolution step, inside a disposable native-x86 Fedora 44 target buildroot.
# Configure repositories using Fedora's target-specific buildroot policy first.
# In particular i686 uses Fedora Koji, not an invented i386 release mirror.
set -euo pipefail
case "$(uname -m)" in x86_64|i686|i386) ;; *) echo 'native x86 buildroot required' >&2; exit 1;; esac
case "${WINE_TARGET:?}" in x86_64|i686) ;; *) exit 1;; esac
test ! -e /wine-source-check
mkdir -p /wine-source-check /usr/lib/tilo
python3 /tilo/wine_rpm_contract.py source /wine-source /tilo/wine-source.json
# Source installation extracts the verified spec and sources; it does not run
# %prep, %build or foreign binary-package scriptlets.
rpm -i --define '_topdir /wine-source-check' /wine-source/wine-11.0-3.fc44.src.rpm
python3 /tilo/wine_rpm_contract.py spec /wine-source-check/SPECS/wine.spec /tilo/wine-source.json
build_dist=$(python3 /tilo/wine_rpm_contract.py setting dist /tilo/wine-source.json)
# builddep's conditional flags apply to SPEC files, not to SRPM inputs.
# Do not use --skip-unavailable, --nodeps, or remove any BuildRequires.
dnf5 builddep -y --without=new_wow64 --define="_target_cpu $WINE_TARGET" \
    --define="dist $build_dist" \
    --spec /wine-source-check/SPECS/wine.spec
rpmspec --target "$WINE_TARGET" --without new_wow64 --define "dist $build_dist" -q --buildrequires \
    /wine-source-check/SPECS/wine.spec | LC_ALL=C sort -u > /usr/lib/tilo/wine-buildrequires.txt
python3 /tilo/wine_rpm_contract.py record-buildroot "$WINE_TARGET" /tilo/wine-source.json
# Publish this complete root by OCI digest. No Wine compilation occurs here.
