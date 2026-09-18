#!/usr/bin/env bash
# Run only inside the disposable native snapshot buildroot.
set -euo pipefail
case "$(uname -m)" in
    x86_64|aarch64) ;; # Resolution/download only; no foreign scriptlets execute.
    *) echo 'unsupported native guest resolver architecture' >&2; exit 1 ;;
esac
python3 /tilo/windows_guest_contract.py reference --buildroot-ref "${WINDOWS_GUEST_BUILDROOT_REF:?}" >/dev/null
test ! -e /resolve-root
test ! -e /tilo-windows-guest-cache
test ! -e /repo
mkdir /resolve-root /repo
python3 /tilo/windows_guest_contract.py roots --roots /tilo/windows-guest-packages.txt > /repo/windows-guest-roots.txt
mapfile -t packages < /repo/windows-guest-roots.txt
wine_args=()
if [ "${DEVELOPMENT_WINE_REF:-scratch}" != scratch ]; then
    python3 /tilo/windows_guest_contract.py reference --buildroot-ref "$DEVELOPMENT_WINE_REF" >/dev/null
    python3 /tilo/development-wine-repository.py verify --directory /development-wine/signed-wine \
        --key-sha256 "${DEVELOPMENT_WINE_KEY_SHA256:?}" >/dev/null
    cp -a /development-wine/signed-wine /wine-input
    createrepo_c --checksum sha256 /wine-input/rpms
    wine_args=(--repofrompath=tilo-development-wine,file:///wine-input/rpms
        --setopt=tilo-development-wine.gpgcheck=True
        --setopt=tilo-development-wine.gpgkey=file:///wine-input/public.asc
        --setopt=tilo-development-wine.priority=1)
    mkdir /repo/development-wine
    cp /wine-input/public.asc /wine-input/development-wine.json /wine-input/paired-contract.json /repo/development-wine/
    printf '%s\n' "$DEVELOPMENT_WINE_REF" > /repo/development-wine/reference.txt
elif [ -n "${DEVELOPMENT_WINE_KEY_SHA256:-}" ]; then
    echo 'development Wine public-key pin requires its immutable artifact' >&2; exit 1
fi

# No inherited RPM database: even packages already present in the buildroot
# must enter this transaction. DNF scriptlets are preserved at installation.
dnf5 --forcearch=x86_64 --installroot=/resolve-root --releasever=44 --use-host-config \
    --setopt=cachedir=/tilo-windows-guest-cache --setopt=keepcache=True \
    --setopt=install_weak_deps=True --setopt=gpgcheck=True --setopt=tsflags= \
    "${wine_args[@]}" install -y --downloadonly "${packages[@]}"
# DNF cache locations can be host-relative or installroot-relative. Both names
# are fresh, private locations in this disposable image, never global caches.
python3 /tilo/windows_guest_contract.py collect --repo /repo \
    --cache /tilo-windows-guest-cache --cache /resolve-root/tilo-windows-guest-cache
if [ "${DEVELOPMENT_WINE_REF:-scratch}" != scratch ]; then
    # DNF may use file:// RPMs in place, without copying them into its cache.
    # Preserve the entire verified pair; installation still selects normal roots.
    python3 /tilo/windows_guest_contract.py collect --repo /repo --cache /wine-input/rpms
fi
test -f /etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary
rpmkeys --import /etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary
if [ "${DEVELOPMENT_WINE_REF:-scratch}" != scratch ]; then
    python3 /tilo/development-wine-repository.py verify --directory /repo/development-wine \
        --key-sha256 "$DEVELOPMENT_WINE_KEY_SHA256" --packages /repo \
        --fedora-key /etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary >/dev/null
else
    for package in /repo/*.rpm; do
        result=$(rpmkeys --checksig --verbose "$package")
        grep -Eq '^[[:space:]]+.*[Ss]ignature.*: OK$' <<< "$result"
    done
fi
createrepo_c --checksum sha256 /repo
cat > /repo/windows-guest.repo <<'EOF'
[tilo-windows-guest]
name=Tilo complete Fedora 44 Windows guest snapshot
baseurl=file:///repo
enabled=1
gpgcheck=1
gpgkey=file:///etc/pki/rpm-gpg/RPM-GPG-KEY-fedora-44-primary
EOF
if [ "${DEVELOPMENT_WINE_REF:-scratch}" != scratch ]; then
    sed -i 's|^gpgkey=.*|& file:///repo/development-wine/public.asc|' /repo/windows-guest.repo
fi
rpm -qp --qf '%{NAME}\t%{EPOCHNUM}\t%{VERSION}\t%{RELEASE}\t%{ARCH}\t%{LICENSE}\n' /repo/*.rpm \
    | LC_ALL=C sort > /repo/windows-guest-package-inventory.tsv
python3 /tilo/windows_guest_contract.py record --roots /tilo/windows-guest-packages.txt \
    --repo /repo --buildroot-ref "$WINDOWS_GUEST_BUILDROOT_REF"
