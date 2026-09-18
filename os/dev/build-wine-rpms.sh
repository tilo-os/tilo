#!/usr/bin/env bash
set -euo pipefail
case "$(uname -m)" in x86_64|i686|i386) ;; *) echo 'native x86 Wine build required' >&2; exit 1;; esac
case "${WINE_TARGET:?}" in x86_64|i686) ;; *) exit 1;; esac
case "${BUILD_JOBS:-2}" in ''|*[!0-9]*|0) echo 'BUILD_JOBS must be positive' >&2; exit 1;; esac
python3 /tilo/wine_rpm_contract.py verify-buildroot "$WINE_TARGET" /tilo/wine-source.json "${WINE_BUILDROOT_REF:?}"
python3 /tilo/wine_rpm_contract.py source /wine-source /tilo/wine-source.json
test ! -e /wine-build
test ! -e /wine-output
mkdir -p /wine-build /wine-output/rpms /wine-output/provenance
SOURCE_DATE_EPOCH=$(python3 /tilo/wine_rpm_contract.py setting source_date_epoch /tilo/wine-source.json)
export SOURCE_DATE_EPOCH
build_dist=$(python3 /tilo/wine_rpm_contract.py setting dist /tilo/wine-source.json)
if [[ "$WINE_TARGET" == i686 ]]; then
    export CC='gcc -m32' CXX='g++ -m32'
else
    export CC='gcc -m64' CXX='g++ -m64'
fi
# Fedora's source and staging patches remain byte-for-byte unchanged. Normal
# RPM dependency checks, scriptlets, docs, licenses and debuginfo stay enabled.
rpm -i --define '_topdir /wine-build' /wine-source/wine-11.0-3.fc44.src.rpm
python3 /tilo/wine_rpm_contract.py spec /wine-build/SPECS/wine.spec /tilo/wine-source.json
mv /wine-build/SPECS/wine.spec /wine-output/provenance/upstream.spec
python3 /tilo/wine_rpm_contract.py prepare-spec /wine-output/provenance/upstream.spec /tilo/wine-source.json /wine-build/SPECS/wine.spec
cp /wine-build/SPECS/wine.spec /wine-output/provenance/wine.spec
rpmbuild -ba /wine-build/SPECS/wine.spec \
    --target "$WINE_TARGET" --without new_wow64 \
    --define "dist $build_dist" \
    --define '_topdir /wine-build' --define "_smp_mflags -j${BUILD_JOBS:-2}" \
    > /wine-output/provenance/build.log 2>&1
while IFS= read -r -d '' package; do
    test ! -e "/wine-output/rpms/${package##*/}"
    cp "$package" /wine-output/rpms/
done < <(find /wine-build/RPMS -type f -name '*.rpm' -print0)
cp /wine-source/wine-11.0-3.fc44.src.rpm /wine-output/provenance/
cp /wine-build/SRPMS/*.src.rpm /wine-output/provenance/
cp /usr/lib/tilo/wine-buildroot.json /usr/lib/tilo/wine-buildrequires.txt /wine-output/provenance/
python3 /tilo/wine_rpm_contract.py output "$WINE_TARGET" /tilo/wine-source.json "$WINE_BUILDROOT_REF"
