#!/usr/bin/env bash
# Run only under Containerfile.windows-guest's network-disabled RUN instruction.
set -euo pipefail
test "$(uname -m)" = x86_64
for reference in "${WINDOWS_GUEST_BUILDROOT_REF:?}" "${WINDOWS_GUEST_RPMS_REF:?}"; do
    python3 /tilo/windows_guest_contract.py reference --buildroot-ref "$reference" >/dev/null
done
test ! -e /guestroot
python3 /tilo/windows_guest_contract.py verify --roots /tilo/windows-guest-packages.txt \
    --repo /repo --buildroot-ref "$WINDOWS_GUEST_BUILDROOT_REF"
if [ "${DEVELOPMENT_WINE_REF:-scratch}" != scratch ]; then
    test "$(cat /repo/development-wine/reference.txt)" = "$DEVELOPMENT_WINE_REF"
    python3 /tilo/development-wine-repository.py verify --directory /repo/development-wine \
        --key-sha256 "${DEVELOPMENT_WINE_KEY_SHA256:?}" --packages /repo \
        --fedora-key /etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary >/dev/null
else
    test ! -e /repo/development-wine
    test -z "${DEVELOPMENT_WINE_KEY_SHA256:-}"
fi
python3 /tilo/windows_guest_contract.py roots --roots /tilo/windows-guest-packages.txt > /guest-packages.txt
mapfile -t packages < /guest-packages.txt
mkdir /guestroot
# Explicit reposdir hides every host network repository. RUN --network=none is
# the second boundary. No --noscripts, --nodeps, --skip-broken or RPM extraction.
dnf5 --installroot=/guestroot --releasever=44 --use-host-config \
    --setopt=reposdir=/repo --setopt=install_weak_deps=True \
    --setopt=gpgcheck=True --setopt=tsflags= install -y "${packages[@]}"
metadata=/guestroot/usr/lib/tilo/windows-guest
mkdir -p "$metadata"
cp /repo/windows-guest-contract.json /repo/windows-guest-roots.txt "$metadata/"
if [ -d /repo/development-wine ]; then
    cp -a /repo/development-wine "$metadata/development-wine"
fi
printf '%s\n' "$WINDOWS_GUEST_RPMS_REF" > "$metadata/rpms-ref.txt"
rpm --root /guestroot -qa --qf '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{LICENSE}\n' \
    | LC_ALL=C sort > "$metadata/installed-packages.tsv"

# Test the actual installed guest helpers natively, not copies from the buildroot.
chroot /guestroot /usr/bin/python3 -c 'import ctypes, ssl, lzma, hashlib, tarfile, tomllib; assert ssl.OPENSSL_VERSION'
chroot /guestroot /usr/bin/localedef -i en_US -f UTF-8 en_US.UTF-8
chroot /guestroot /usr/bin/locale -a | grep -qi '^en_US\.utf8$'

# Keep the uniform upstream pair separate from specially marked Fedora Wine
# builtins. Runtime prefix installation/overrides are a separate acceptance gate.
python3 /tilo/install-windows-guest-dxvk.py /guestroot /tilo-assets /tilo/external-assets.tsv
python3 /tilo/validate-windows-guest.py /guestroot > "$metadata/payload.json"
# Preserve the complete validated installroot. Any FEX-specific runtime view
# belongs to a separate, explicitly validated assembly policy.
