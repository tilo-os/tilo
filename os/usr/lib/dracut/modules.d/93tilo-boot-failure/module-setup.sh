#!/usr/bin/bash
# The boot-failure screen in the initrd: a unit failing here (the disk not
# unlocking, the system not mounting) shows "Tilo couldn't start" instead of
# systemd's administrator console. See /usr/libexec/tilo-boot-failure.

check() {
    require_binaries /usr/libexec/tilo-boot-failure /usr/libexec/tilo-boot-failure-input || return 1
    return 255
}

depends() {
    echo "plymouth systemd"
    return 0
}

installkernel() {
    # Keys and taps reach tilo-boot-failure-input through evdev; the ESP the
    # record goes to is FAT. Whichever of these the kernel builds in is skipped.
    hostonly='' instmods evdev usbhid hid-generic hid-multitouch i2c-hid-acpi \
        vfat nls_cp437 nls_iso8859-1 nls_utf8 || :
}

install() {
    inst_multiple bash bootctl cat cp cryptsetup cut date find grep head install \
        journalctl logger mkdir mount mountpoint mv sed sleep sort stat sync \
        systemctl tr umount
    inst_multiple -o /usr/lib/systemd/systemd-pcrextend
    inst_simple /usr/libexec/tilo-boot-failure
    inst_simple /usr/libexec/tilo-boot-failure-input
    inst_multiple /usr/share/tilo/boot-failure/locales/*.ftl
    # Both emergency units show the screen. Their stock ExecStartPre= quits
    # plymouth, which the screen is drawn on.
    # shellcheck disable=SC2154 # dracut defines moddir and initdir
    local unit
    for unit in emergency.service dracut-emergency.service; do
        inst_simple "$moddir/10-tilo-boot-failure.conf" \
            "/usr/lib/systemd/system/$unit.d/10-tilo-boot-failure.conf"
    done
}
