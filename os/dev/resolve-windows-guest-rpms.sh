#!/usr/bin/env bash
# Run only inside the disposable native snapshot buildroot.
set -euo pipefail
case "$(uname -m)" in
    x86_64|aarch64) ;; # Resolution/download only; no foreign scriptlets execute.
    *) echo 'unsupported native guest resolver architecture' >&2; exit 1 ;;
esac
python3 /qos/windows_guest_contract.py reference --buildroot-ref "${WINDOWS_GUEST_BUILDROOT_REF:?}" >/dev/null
test ! -e /resolve-root
test ! -e /qos-windows-guest-cache
test ! -e /repo
mkdir /resolve-root /repo
python3 /qos/windows_guest_contract.py roots --roots /qos/windows-guest-packages.txt > /repo/windows-guest-roots.txt
mapfile -t packages < /repo/windows-guest-roots.txt

# No inherited RPM database: even packages already present in the buildroot
# must enter this transaction. DNF scriptlets are preserved at installation.
dnf5 --forcearch=x86_64 --installroot=/resolve-root --releasever=44 --use-host-config \
    --setopt=cachedir=/qos-windows-guest-cache --setopt=keepcache=True \
    --setopt=install_weak_deps=True --setopt=gpgcheck=True --setopt=tsflags= \
    install -y --downloadonly "${packages[@]}"
# DNF cache locations can be host-relative or installroot-relative. Both names
# are fresh, private locations in this disposable image, never global caches.
python3 /qos/windows_guest_contract.py collect --repo /repo \
    --cache /qos-windows-guest-cache --cache /resolve-root/qos-windows-guest-cache
test -f /etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-x86_64
rpmkeys --import /etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-x86_64
for package in /repo/*.rpm; do
    rpmkeys --checksig "$package"
done
createrepo_c --checksum sha256 /repo
cat > /repo/windows-guest.repo <<'EOF'
[qos-windows-guest]
name=qOS complete Fedora 44 Windows guest snapshot
baseurl=file:///repo
enabled=1
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-x86_64
EOF
rpm -qp --qf '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{LICENSE}\n' /repo/*.rpm \
    | LC_ALL=C sort > /repo/windows-guest-package-inventory.tsv
python3 /qos/windows_guest_contract.py record --roots /qos/windows-guest-packages.txt \
    --repo /repo --buildroot-ref "$WINDOWS_GUEST_BUILDROOT_REF"
