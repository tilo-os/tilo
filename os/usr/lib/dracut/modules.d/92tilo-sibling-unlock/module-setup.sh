#!/usr/bin/bash
# Unlock the root with another of the owner's devices: the client, the
# image's NetworkManager to reach them, and the units that run both. All of it
# stays idle unless the root's LUKS2 header carries a tilo-sibling-unlock
# token (tilo-root-discover checks), so an ordinary boot starts nothing here.

check() {
    require_binaries tilo-sibling-unlock NetworkManager cryptsetup ip systemctl || return 1
    return 255
}

depends() {
    # dbus: NetworkManager asks wpa_supplicant for Wi-Fi over the system bus.
    echo "systemd-cryptsetup dbus"
    return 0
}

install() {
    inst_multiple tilo-sibling-unlock NetworkManager cryptsetup ip systemctl systemd-creds
    tilo_initramfs_helper tilo-sibling-unlock
    # Wi-Fi, for a machine whose owner chose a Wi-Fi network for unlocking.
    inst_multiple -o wpa_supplicant
    inst_libdir_dir "NetworkManager/$(NetworkManager --version)"
    inst_libdir_file -o "NetworkManager/$(NetworkManager --version)/libnm-device-plugin-wifi.so"
    # shellcheck disable=SC2154 # dracut defines dbussystem and systemdsystemunitdir
    inst_multiple -o \
        "$dbussystem"/org.freedesktop.NetworkManager.conf \
        "$dbussystem"/wpa_supplicant.conf \
        /usr/share/dbus-1/system-services/fi.w1.wpa_supplicant1.service \
        "$systemdsystemunitdir"/wpa_supplicant.service
    # The internal DHCP client keys its client id on the machine id.
    [ -e "$initdir/etc/machine-id" ] || : > "$initdir/etc/machine-id"
    # shellcheck disable=SC2154 # dracut defines moddir before loading modules
    inst_simple "$moddir/tilo-sibling-unlock.conf" /etc/NetworkManager/tilo-sibling-unlock.conf
    inst_simple "$moddir/tilo-sibling-unlock@.service" "$systemdsystemunitdir/tilo-sibling-unlock@.service"
    inst_simple "$moddir/tilo-sibling-unlock-network.service" "$systemdsystemunitdir/tilo-sibling-unlock-network.service"
}

# The image build's copy without its symbol table, when it made one
# (os/scripts/build-initramfs.sh, TILO_INITRAMFS_HELPERS). The installed file
# is removed, never written through: it may share its inode with the rootfs's.
tilo_initramfs_helper() {
    [ -n "${TILO_INITRAMFS_HELPERS:-}" ] || return 0
    local stripped="$TILO_INITRAMFS_HELPERS/$1"
    [ -f "$stripped" ] || { derror "tilo: no stripped $1 in $TILO_INITRAMFS_HELPERS"; return 1; }
    # shellcheck disable=SC2154 # dracut defines initdir before installing modules
    rm -f -- "$initdir/usr/bin/$1"
    cp --preserve=mode -- "$stripped" "$initdir/usr/bin/$1" || return 1
    dinfo "tilo: initramfs $1 without its symbol table"
}
